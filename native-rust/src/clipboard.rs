//! Clipboard access is invoked only by a user-initiated save. The named-board test uses its own board.
use crate::{ffi::*, screen};
use objc2::{msg_send, rc::autoreleasepool, runtime::AnyObject};
use serde_json::{Value, json};
use std::{path::Path, ptr};

const MAX_IMAGE_BYTES: usize = 64 * 1024 * 1024;

unsafe fn decode(data: *mut AnyObject) -> Result<Image, String> {
    if data.is_null() {
        return Err("Image data is unavailable".into());
    }
    let size: usize = msg_send![data, length];
    if size == 0 || size > MAX_IMAGE_BYTES {
        return Err("Images must be between one byte and 64 MiB".into());
    }
    let source = Cf(CGImageSourceCreateWithData(data.cast(), ptr::null()));
    if source.0.is_null() {
        return Err("Unsupported image format".into());
    }
    let props = Cf(CGImageSourceCopyPropertiesAtIndex(source.0, 0, ptr::null()));
    if props.0.is_null() {
        return Err("Could not read image dimensions".into());
    }
    let mut dims = [0usize; 2];
    for (dim, key) in dims
        .iter_mut()
        .zip([kCGImagePropertyPixelWidth, kCGImagePropertyPixelHeight])
    {
        let number: *mut AnyObject = msg_send![props.0.cast::<AnyObject>(), objectForKey:key];
        if !number.is_null() {
            *dim = msg_send![number, unsignedIntegerValue];
        }
    }
    if dims.contains(&0) || dims[0].saturating_mul(dims[1]) > 40_000_000 {
        return Err("Image exceeds the 40-megapixel limit or has invalid dimensions".into());
    }
    // ImageIO applies the source orientation while preserving its full pixel resolution.
    let options = new(c"NSMutableDictionary");
    let yes: Obj = msg_send![class(c"NSNumber"), numberWithBool:true];
    for key in [
        kCGImageSourceCreateThumbnailFromImageAlways,
        kCGImageSourceCreateThumbnailWithTransform,
    ] {
        let _: () = msg_send![&*options, setObject:&*yes, forKey:key];
    }
    let longest: Obj =
        msg_send![class(c"NSNumber"), numberWithUnsignedInteger:dims[0].max(dims[1])];
    let _: () =
        msg_send![&*options, setObject:&*longest, forKey:kCGImageSourceThumbnailMaxPixelSize];
    let image =
        CGImageSourceCreateThumbnailAtIndex(source.0, 0, (&*options as *const AnyObject).cast());
    if image.is_null() {
        return Err("Could not decode image".into());
    }
    Ok(Image(Cf(image)))
}

pub unsafe fn load_image(path: &Path) -> Result<Image, String> {
    if path.metadata().map_err(|e| e.to_string())?.len() > MAX_IMAGE_BYTES as u64 {
        return Err("Image exceeds 64 MiB".into());
    }
    autoreleasepool(|_| {
        let data: *mut AnyObject = msg_send![class(c"NSData"), dataWithContentsOfFile:&*string(path.to_str().ok_or("Invalid image path")?)];
        decode(data)
    })
}

pub unsafe fn ocr_file(path: &Path) -> Result<Value, String> {
    Ok(json!({"text":screen::saved_text(&load_image(path)?)?}))
}

pub unsafe fn write_png(image: &Image, path: &Path) -> Result<(), String> {
    let rep: objc2::rc::Allocated<AnyObject> = msg_send![class(c"NSBitmapImageRep"), alloc];
    let rep: Obj = msg_send![rep, initWithCGImage:image.0.0.cast::<CGImageOpaque>()];
    let data: *mut AnyObject =
        msg_send![&*rep, representationUsingType:4usize, properties:&*new(c"NSDictionary")];
    write_image_data(data, path)
}

unsafe fn write_image_data(data: *mut AnyObject, path: &Path) -> Result<(), String> {
    if data.is_null() {
        return Err("Could not encode clipboard image".into());
    }
    let len: usize = msg_send![data, length];
    if len > MAX_IMAGE_BYTES {
        return Err("Encoded image exceeds 64 MiB".into());
    }
    let ok: bool = msg_send![data, writeToFile:&*string(path.to_str().ok_or("Invalid output path")?), atomically:true];
    if !ok {
        return Err("Could not write clipboard image".into());
    }
    Ok(())
}

unsafe fn read_board(board: *mut AnyObject, path: &Path) -> Result<Value, String> {
    let types: *mut AnyObject = msg_send![board, types];
    let offers = |kind: &str| -> bool {
        !types.is_null() && msg_send![types, containsObject:&*string(kind)]
    };
    // A browser can offer both the copied bitmap and its URL. Keep the image in that case.
    for kind in ["public.png", "public.tiff", "public.jpeg"] {
        if !offers(kind) {
            continue;
        }
        let data: *mut AnyObject = msg_send![board, dataForType:&*string(kind)];
        if !data.is_null() {
            if kind == "public.png" {
                write_image_data(data, path)?;
            } else {
                write_png(&decode(data)?, path)?;
            }
            return Ok(json!({"kind":"image"}));
        }
    }
    for kind in ["public.utf8-plain-text", "public.url", "public.file-url"] {
        if !offers(kind) {
            continue;
        }
        let value: *mut AnyObject = msg_send![board, stringForType:&*string(kind)];
        if value.is_null() {
            continue;
        }
        let len: usize = msg_send![value, lengthOfBytesUsingEncoding:4usize];
        if len == 0 {
            continue;
        }
        if len > 512 * 1024 {
            return Err("Clipboard text exceeds 512 KiB".into());
        }
        let data: *mut AnyObject = msg_send![value, dataUsingEncoding:4usize];
        if data.is_null() {
            return Err("Could not read clipboard text".into());
        }
        let bytes: *const std::ffi::c_void = msg_send![data, bytes];
        let text = std::str::from_utf8(std::slice::from_raw_parts(bytes.cast(), len))
            .map_err(|e| e.to_string())?;
        if !text.trim().is_empty() {
            return Ok(json!({"kind":"text","text":text}));
        }
    }
    Err("Copy some text, a link, or an image first".into())
}

unsafe fn read_version(
    board: *mut AnyObject,
    path: &Path,
    version: isize,
) -> Result<Value, String> {
    let check = || -> Result<(), String> {
        let current: isize = msg_send![board, changeCount];
        if current == version {
            Ok(())
        } else {
            Err("Clipboard changed. Choose Save Clipboard to save the new item.".into())
        }
    };
    check()?;
    let value = read_board(board, path)?;
    check()?;
    Ok(value)
}

pub unsafe fn version() -> isize {
    let board: *mut AnyObject = msg_send![class(c"NSPasteboard"), generalPasteboard];
    msg_send![board, changeCount]
}

pub unsafe fn read(path: &Path, version: isize) -> Result<Value, String> {
    autoreleasepool(|_| {
        let board: *mut AnyObject = msg_send![class(c"NSPasteboard"), generalPasteboard];
        read_version(board, path, version)
    })
}

pub unsafe fn self_test(path: &Path) -> Result<Value, String> {
    autoreleasepool(|_| {
        let board: Obj = msg_send![class(c"NSPasteboard"), pasteboardWithUniqueName];
        let result = (|| {
            let board_ptr = &*board as *const AnyObject as *mut AnyObject;
            if read_board(board_ptr, path).is_ok() {
                return Err("Expected an empty test board".into());
            }
            let sample = "Test note\n第二行 🖊";
            let _: bool = msg_send![&*board, setString:&*string(sample), forType:&*string("public.utf8-plain-text")];
            let version: isize = msg_send![&*board, changeCount];
            if read_version(board_ptr, path, version)?["text"] != sample
                || read_version(board_ptr, path, version.wrapping_sub(1)).is_ok()
            {
                return Err("Text did not round-trip".into());
            }
            write_png(&screen::synthetic_image()?, path)?;
            let data: Obj = msg_send![class(c"NSData"), dataWithContentsOfFile:&*string(path.to_str().ok_or("Invalid path")?)];
            let _: isize = msg_send![&*board, clearContents];
            let link = "https://example.org/picture";
            let _: bool =
                msg_send![&*board, setString:&*string(link), forType:&*string("public.url")];
            if read_board(board_ptr, path)?["text"] != link {
                return Err("Link did not round-trip".into());
            }
            let original = std::fs::read(path).map_err(|e| e.to_string())?;
            let _: bool = msg_send![&*board, setData:&*data, forType:&*string("public.png")];
            if read_board(board_ptr, path)?["kind"] != "image"
                || std::fs::read(path).map_err(|e| e.to_string())? != original
                || !ocr_file(path)?["text"]
                    .as_str()
                    .unwrap_or("")
                    .contains("Daybook")
            {
                return Err("Clipboard image OCR did not round-trip".into());
            }
            Ok(json!({"result":"PASS isolated clipboard text, link, original PNG and native OCR"}))
        })();
        let _: () = msg_send![&*board, releaseGlobally];
        result
    })
}
