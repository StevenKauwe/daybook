//! Native Vision OCR and Core Graphics change detection on immutable images.
#![allow(unsafe_op_in_unsafe_fn)]
use crate::ffi::*;
use objc2::{msg_send, rc::autoreleasepool, runtime::AnyObject};
use objc2_foundation::NSRange;
use serde_json::{Value, json};
use std::{path::Path, ptr};
pub fn changed(old: Option<&[u8]>, new: &[u8]) -> bool {
    match old {
        Some(old) if old.len() == new.len() && !new.is_empty() => {
            old.iter()
                .zip(new)
                .map(|(&a, &b)| (a as i64 - b as i64).unsigned_abs())
                .sum::<u64>() as f64
                / new.len() as f64
                >= 3.0
        }
        _ => true,
    }
}
pub unsafe fn thumbnail(image: &Image) -> Result<Vec<u8>, String> {
    let mut bytes = vec![0u8; 64 * 36 * 4];
    let space = Cf(CGColorSpaceCreateDeviceRGB());
    let context = Cf(CGBitmapContextCreate(
        bytes.as_mut_ptr().cast(),
        64,
        36,
        8,
        64 * 4,
        space.0,
        1,
    ));
    if context.0.is_null() {
        return Err("Could not allocate comparison image".into());
    }
    CGContextDrawImage(
        context.0,
        Rect {
            origin: Point::default(),
            size: Size {
                width: 64.0,
                height: 36.0,
            },
        },
        image.0.0,
    );
    Ok(bytes)
}
pub unsafe fn recognize(image: &Image) -> Result<(String, Vec<Value>), String> {
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
        for i in 0..count.min(200) {
            let observation: *mut AnyObject = msg_send![results,objectAtIndex:i];
            let candidates: *mut AnyObject = msg_send![observation,topCandidates:1usize];
            let count: usize = msg_send![candidates, count];
            if count == 0 {
                continue;
            }
            let candidate: *mut AnyObject = msg_send![candidates,objectAtIndex:0usize];
            let string_ptr: *mut AnyObject = msg_send![candidate, string];
            let line = text(string_ptr);
            let confidence: f32 = msg_send![candidate, confidence];
            text_out.push_str(&line);
            text_out.push('\n');
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
                words.push(json!({"text":word.chars().take(128).collect::<String>(),"x":r.origin.x,"y":r.origin.y,"width":r.size.width,"height":r.size.height,"confidence":confidence}));
            }
        }
        while text_out.len() > 16000 {
            text_out.pop();
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
    autoreleasepool(|_| {
        let space = Cf(CGColorSpaceCreateDeviceRGB());
        let ctx = Cf(CGBitmapContextCreate(
            ptr::null_mut(),
            800,
            180,
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
                    width: 800.0,
                    height: 180.0,
                },
            },
        );
        let graphics: Obj = msg_send![class(c"NSGraphicsContext"),graphicsContextWithCGContext:ctx.0.cast::<CGContextOpaque>(), flipped:false];
        let _: () = msg_send![class(c"NSGraphicsContext"), saveGraphicsState];
        let _: () = msg_send![class(c"NSGraphicsContext"),setCurrentContext:&*graphics];
        let font: Obj = msg_send![class(c"NSFont"),systemFontOfSize:40.0f64];
        let color: Obj = msg_send![class(c"NSColor"), blackColor];
        let attrs = new(c"NSMutableDictionary");
        let _: () = msg_send![&*attrs,setObject:&*font, forKey:&*string("NSFont")];
        let _: () = msg_send![&*attrs,setObject:&*color, forKey:&*string("NSColor")];
        let _: () = msg_send![&*string("Daybook synthetic decision"),drawAtPoint:Point{x:30.0,y:70.0}, withAttributes:&*attrs];
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
    #[test]
    fn change_detection() {
        assert!(!super::changed(Some(&[0; 8]), &[0; 8]));
        assert!(!super::changed(Some(&[0; 8]), &[2; 8]));
        assert!(super::changed(Some(&[0; 8]), &[255; 8]));
    }
}
