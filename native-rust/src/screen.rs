//! Native Vision OCR and Core Graphics change detection on immutable images.
#![allow(unsafe_op_in_unsafe_fn)]
use crate::ffi::*;
use objc2::{msg_send, rc::autoreleasepool, runtime::AnyObject};
use objc2_foundation::NSRange;
use serde_json::{Value, json};
use std::{path::Path, ptr};
/// Only this digest survives the check. If pixels are unavailable, run OCR.
pub unsafe fn fingerprint(image: &Image) -> Option<[u8; 32]> {
    let provider = CGImageGetDataProvider(image.0.0);
    if provider.is_null() {
        return None;
    }
    let data = Cf(CGDataProviderCopyData(provider));
    if data.0.is_null() {
        return None;
    }
    let len = u32::try_from(CFDataGetLength(data.0)).ok()?;
    let bytes = CFDataGetBytePtr(data.0);
    if bytes.is_null() || len == 0 {
        return None;
    }
    let mut digest = [0; 32];
    CC_SHA256(bytes.cast(), len, digest.as_mut_ptr());
    Some(digest)
}

/// Inspect only the frontmost app's window number; never read window titles.
pub unsafe fn foreground_window() -> (i32, u32) {
    let workspace: Obj = msg_send![class(c"NSWorkspace"), sharedWorkspace];
    let app: *mut AnyObject = msg_send![&*workspace, frontmostApplication];
    if app.is_null() {
        return (0, 0);
    }
    let pid: i32 = msg_send![app, processIdentifier];
    let list = Cf(CGWindowListCopyWindowInfo(1 | 16, 0));
    if !list.0.is_null() {
        let array = list.0.cast::<AnyObject>();
        let count: usize = msg_send![array, count];
        for i in 0..count {
            let window: *mut AnyObject = msg_send![array,objectAtIndex:i];
            let owner: *mut AnyObject = msg_send![window,objectForKey:kCGWindowOwnerPID];
            let layer: *mut AnyObject = msg_send![window,objectForKey:kCGWindowLayer];
            if owner.is_null() || layer.is_null() {
                continue;
            }
            let window_pid: i32 = msg_send![owner, intValue];
            let level: i32 = msg_send![layer, intValue];
            if window_pid == pid && level == 0 {
                let number: *mut AnyObject = msg_send![window,objectForKey:kCGWindowNumber];
                if !number.is_null() {
                    return (pid, msg_send![number, unsignedIntValue]);
                }
            }
        }
    }
    (pid, 0)
}
pub unsafe fn recognize(image: &Image) -> Result<(String, Vec<Value>), String> {
    read_image_text(image, true)
}

pub unsafe fn saved_text(image: &Image) -> Result<String, String> {
    read_image_text(image, false).map(|(text, _)| text)
}

unsafe fn read_image_text(
    image: &Image,
    with_word_boxes: bool,
) -> Result<(String, Vec<Value>), String> {
    autoreleasepool(|_| {
        let request = new(c"VNRecognizeTextRequest");
        let _: () = msg_send![&*request,setRecognitionLevel:0usize];
        let options = new(c"NSDictionary");
        let handler: objc2::rc::Allocated<AnyObject> =
            msg_send![class(c"VNImageRequestHandler"), alloc];
        let handler: Obj =
            msg_send![handler,initWithCGImage:image.0.0.cast::<CGImageOpaque>(), options:&*options];
        let requests: Obj = msg_send![class(c"NSArray"),arrayWithObject:&*request];
        let mut err = nil();
        let ok: bool = msg_send![&*handler,performRequests:&*requests, error:&mut err];
        if !ok {
            return Err(error(err).unwrap_or("Vision OCR failed".into()));
        }
        let results: *mut AnyObject = msg_send![&*request, results];
        let count: usize = msg_send![results, count];
        let mut text_out = String::new();
        let mut words = Vec::new();
        let mut line_index = 0;
        let lines = if with_word_boxes {
            count.min(200)
        } else {
            count
        };
        for i in 0..lines {
            let observation: *mut AnyObject = msg_send![results,objectAtIndex:i];
            let candidates: *mut AnyObject = msg_send![observation,topCandidates:1usize];
            let count: usize = msg_send![candidates, count];
            if count == 0 {
                continue;
            }
            let candidate: *mut AnyObject = msg_send![candidates,objectAtIndex:0usize];
            let string_ptr: *mut AnyObject = msg_send![candidate, string];
            let line = text(string_ptr);
            if with_word_boxes && text_out.len() + line.len() + 1 > 16000 {
                break;
            }
            let confidence: f32 = msg_send![candidate, confidence];
            text_out.push_str(&line);
            text_out.push('\n');
            if !with_word_boxes {
                if text_out.len() > 512 * 1024 {
                    return Err("Recognized text exceeds 512 KiB".into());
                }
                continue;
            }
            let mut cursor = 0;
            for word in line.split_whitespace() {
                if words.len() >= 1000 {
                    break;
                }
                let Some(relative) = line[cursor..].find(word) else {
                    continue;
                };
                let start = cursor + relative;
                cursor = start + word.len();
                let range = NSRange::new(
                    line[..start].encode_utf16().count(),
                    word.encode_utf16().count(),
                );
                let mut err = nil();
                let box_: *mut AnyObject =
                    msg_send![candidate,boundingBoxForRange:range, error:&mut err];
                if box_.is_null() {
                    continue;
                }
                let r: Rect = msg_send![box_, boundingBox];
                words.push(json!({"line":line_index,"text":word.chars().take(128).collect::<String>(),"x":r.origin.x,"y":r.origin.y,"width":r.size.width,"height":r.size.height,"confidence":confidence}));
            }
            line_index += 1;
        }
        Ok((text_out, words))
    })
}
pub unsafe fn save_jpeg(image: &Image, root: &Path) -> Result<String, String> {
    autoreleasepool(|_| {
        let rep: objc2::rc::Allocated<AnyObject> = msg_send![class(c"NSBitmapImageRep"), alloc];
        let rep: Obj = msg_send![rep,initWithCGImage:image.0.0.cast::<CGImageOpaque>()];
        let quality: Obj = msg_send![class(c"NSNumber"),numberWithDouble:0.65f64];
        let key = string("NSImageCompressionFactor");
        let properties: Obj =
            msg_send![class(c"NSDictionary"),dictionaryWithObject:&*quality, forKey:&*key];
        let data: *mut AnyObject =
            msg_send![&*rep,representationUsingType:3usize, properties:&*properties];
        if data.is_null() {
            return Err("JPEG encoding failed".into());
        }
        let len: usize = msg_send![data, length];
        if len > 2 * 1024 * 1024 {
            return Err("JPEG exceeded 2 MiB cap".into());
        }
        let bytes: *const std::ffi::c_void = msg_send![data, bytes];
        let path = format!(
            "images/{}-{}.jpg",
            std::process::id(),
            (crate::stamp() * 1e6) as u64
        );
        std::fs::create_dir_all(root.join("images")).map_err(|e| e.to_string())?;
        let target = root.join(&path);
        let tmp = target.with_extension("tmp");
        std::fs::write(&tmp, std::slice::from_raw_parts(bytes.cast::<u8>(), len))
            .and_then(|_| std::fs::rename(tmp, target))
            .map_err(|e| e.to_string())?;
        Ok(path)
    })
}
pub unsafe fn synthetic_image() -> Result<Image, String> {
    drawn_image(800, 180, 1)
}

pub unsafe fn drawn_image(width: usize, height: usize, lines: usize) -> Result<Image, String> {
    autoreleasepool(|_| {
        let space = Cf(CGColorSpaceCreateDeviceRGB());
        let ctx = Cf(CGBitmapContextCreate(
            ptr::null_mut(),
            width,
            height,
            8,
            0,
            space.0,
            1,
        ));
        if ctx.0.is_null() {
            return Err("Synthetic bitmap allocation failed".into());
        }
        CGContextSetRGBFillColor(ctx.0, 1.0, 1.0, 1.0, 1.0);
        CGContextFillRect(
            ctx.0,
            Rect {
                origin: Point::default(),
                size: Size {
                    width: width as f64,
                    height: height as f64,
                },
            },
        );
        let graphics: Obj = msg_send![class(c"NSGraphicsContext"),graphicsContextWithCGContext:ctx.0.cast::<CGContextOpaque>(), flipped:false];
        let _: () = msg_send![class(c"NSGraphicsContext"), saveGraphicsState];
        let _: () = msg_send![class(c"NSGraphicsContext"),setCurrentContext:&*graphics];
        let font_size = if width == 800 && height == 180 {
            40.0
        } else {
            width as f64 / 70.0
        };
        let font: Obj = msg_send![class(c"NSFont"),systemFontOfSize:font_size];
        let color: Obj = msg_send![class(c"NSColor"), blackColor];
        let attrs = new(c"NSMutableDictionary");
        let _: () = msg_send![&*attrs,setObject:&*font, forKey:&*string("NSFont")];
        let _: () = msg_send![&*attrs,setObject:&*color, forKey:&*string("NSColor")];
        for row in 0..lines {
            let label = if width == 800 && height == 180 {
                "Daybook synthetic decision".to_owned()
            } else {
                format!(
                    "Item {:02}   Review the project notes and confirm the next meeting date.",
                    row + 1
                )
            };
            let point = Point {
                x: width as f64 * 0.04,
                y: height as f64 * (0.9 - (row as f64 / 23.0)) - font_size,
            };
            let _: () = msg_send![&*string(&label),drawAtPoint:point, withAttributes:&*attrs];
        }
        let _: () = msg_send![class(c"NSGraphicsContext"), restoreGraphicsState];
        let image = CGBitmapContextCreateImage(ctx.0);
        if image.is_null() {
            Err("Synthetic image creation failed".into())
        } else {
            Ok(Image(Cf(image)))
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_single_changed_pixel_changes_the_fingerprint() {
        unsafe {
            let space = Cf(CGColorSpaceCreateDeviceRGB());
            let canvas = Cf(CGBitmapContextCreate(
                ptr::null_mut(),
                1280,
                64,
                8,
                0,
                space.0,
                1,
            ));
            assert!(!canvas.0.is_null());
            let fill = |w, h| {
                CGContextFillRect(
                    canvas.0,
                    Rect {
                        origin: Point::default(),
                        size: Size {
                            width: w,
                            height: h,
                        },
                    },
                )
            };
            CGContextSetRGBFillColor(canvas.0, 1., 1., 1., 1.);
            fill(1280., 64.);
            let before = Image(Cf(CGBitmapContextCreateImage(canvas.0)));
            let first = fingerprint(&before).unwrap();
            CGContextSetRGBFillColor(canvas.0, 0., 0., 0., 1.);
            fill(1., 1.);
            let after = Image(Cf(CGBitmapContextCreateImage(canvas.0)));
            assert_ne!(first, fingerprint(&after).unwrap());
            assert_eq!(Some(first), fingerprint(&before));
        }
    }
}
