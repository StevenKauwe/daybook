//! Formats accepted by both the in-window Paste action and the native clipboard reader.
use objc2_foundation::{NSString, NSURL};
use std::path::PathBuf;

pub const BITMAP_TYPES: &[&str] = &[
    "public.png",
    "public.tiff",
    "public.jpeg",
    "com.compuserve.gif",
    "org.webmproject.webp",
    "public.heic",
];

pub fn image_file(value: &NSString) -> Option<PathBuf> {
    let url = NSURL::URLWithString(value)?;
    if !url.isFileURL()
        || url
            .host()
            .is_some_and(|host| !matches!(host.to_string().as_str(), "" | "localhost"))
    {
        return None;
    }
    let path = PathBuf::from(url.path()?.to_string());
    let ext = path.extension()?.to_str()?.to_ascii_lowercase();
    (path.is_absolute()
        && matches!(
            ext.as_str(),
            "png" | "jpg" | "jpeg" | "tif" | "tiff" | "gif" | "webp" | "heic" | "heif"
        ))
    .then_some(path)
}
