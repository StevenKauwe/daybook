//! Narrow, audited Apple ABI boundary. Signatures follow the installed public SDK headers.
//! No Swift/Objective-C source or shim is built. Objective-C objects remain on the main
//! thread, except explicitly retained immutable SCShareableContent / CGImage snapshots.
#![allow(non_snake_case, unsafe_op_in_unsafe_fn)]
use objc2::{
    encode::{Encode, Encoding, RefEncode},
    msg_send,
    rc::Retained,
    runtime::{AnyClass, AnyObject},
};
use objc2_foundation::NSString;
use std::{
    ffi::{CStr, c_void},
    ptr,
};
pub type Obj = Retained<AnyObject>;
pub fn class(name: &CStr) -> &'static AnyClass {
    AnyClass::get(name).expect("required Apple framework class missing")
}
pub fn string(s: &str) -> Retained<NSString> {
    NSString::from_str(s)
}
pub unsafe fn text(p: *mut AnyObject) -> String {
    if p.is_null() {
        return String::new();
    }
    let s: *const std::ffi::c_char = msg_send![p, UTF8String];
    if s.is_null() {
        String::new()
    } else {
        CStr::from_ptr(s).to_string_lossy().into_owned()
    }
}
pub unsafe fn error(p: *mut AnyObject) -> Option<String> {
    if p.is_null() {
        None
    } else {
        let s: *mut AnyObject = msg_send![p, localizedDescription];
        Some(text(s))
    }
}
pub unsafe fn new(c: &CStr) -> Obj {
    msg_send![class(c), new]
}
pub unsafe fn retain(p: *mut AnyObject) -> Option<Obj> {
    Retained::retain(p)
}
pub fn nil() -> *mut AnyObject {
    ptr::null_mut()
}
#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct Point {
    pub x: f64,
    pub y: f64,
}
#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct Size {
    pub width: f64,
    pub height: f64,
}
#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct Rect {
    pub origin: Point,
    pub size: Size,
}
unsafe impl Encode for Point {
    const ENCODING: Encoding = Encoding::Struct("CGPoint", &[Encoding::Double, Encoding::Double]);
}
unsafe impl Encode for Size {
    const ENCODING: Encoding = Encoding::Struct("CGSize", &[Encoding::Double, Encoding::Double]);
}
unsafe impl Encode for Rect {
    const ENCODING: Encoding = Encoding::Struct("CGRect", &[Point::ENCODING, Size::ENCODING]);
}
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct Time {
    pub value: i64,
    pub scale: i32,
    pub flags: u32,
    pub epoch: i64,
}
unsafe impl Encode for Time {
    const ENCODING: Encoding = Encoding::Struct(
        "CMTime",
        &[
            Encoding::LongLong,
            Encoding::Int,
            Encoding::UInt,
            Encoding::LongLong,
        ],
    );
}
#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct Asbd {
    pub rate: f64,
    pub format: u32,
    pub flags: u32,
    pub bytes_packet: u32,
    pub frames_packet: u32,
    pub bytes_frame: u32,
    pub channels: u32,
    pub bits: u32,
    pub reserved: u32,
}
#[repr(C)]
#[derive(Clone, Copy)]
pub struct AudioBuffer {
    pub channels: u32,
    pub bytes: u32,
    pub data: *mut c_void,
}
#[repr(C)]
pub struct AudioList {
    pub count: u32,
    pub buffers: [AudioBuffer; 1],
}
unsafe impl RefEncode for Asbd {
    const ENCODING_REF: Encoding = Encoding::Pointer(&Encoding::Struct(
        "AudioStreamBasicDescription",
        &[
            Encoding::Double,
            Encoding::UInt,
            Encoding::UInt,
            Encoding::UInt,
            Encoding::UInt,
            Encoding::UInt,
            Encoding::UInt,
            Encoding::UInt,
            Encoding::UInt,
        ],
    ));
}
unsafe impl RefEncode for AudioList {
    const ENCODING_REF: Encoding = Encoding::Pointer(&Encoding::Struct(
        "AudioBufferList",
        &[
            Encoding::UInt,
            Encoding::Array(
                1,
                &Encoding::Struct(
                    "AudioBuffer",
                    &[
                        Encoding::UInt,
                        Encoding::UInt,
                        Encoding::Pointer(&Encoding::Void),
                    ],
                ),
            ),
        ],
    ));
}
#[link(name = "AppKit", kind = "framework")]
unsafe extern "C" {}
#[link(name = "AVFoundation", kind = "framework")]
unsafe extern "C" {}
#[link(name = "AVFAudio", kind = "framework")]
unsafe extern "C" {}
#[link(name = "ScreenCaptureKit", kind = "framework")]
unsafe extern "C" {}
#[link(name = "Vision", kind = "framework")]
unsafe extern "C" {}
#[link(name = "ImageIO", kind = "framework")]
unsafe extern "C" {
    pub fn CGImageSourceCreateWithData(data: *const c_void, options: *const c_void) -> *mut c_void;
    pub fn CGImageSourceCopyPropertiesAtIndex(
        source: *const c_void,
        index: usize,
        options: *const c_void,
    ) -> *mut c_void;
    pub fn CGImageSourceCreateThumbnailAtIndex(
        source: *const c_void,
        index: usize,
        options: *const c_void,
    ) -> *mut c_void;
    pub static kCGImagePropertyPixelWidth: *mut AnyObject;
    pub static kCGImagePropertyPixelHeight: *mut AnyObject;
    pub static kCGImageSourceCreateThumbnailFromImageAlways: *mut AnyObject;
    pub static kCGImageSourceCreateThumbnailWithTransform: *mut AnyObject;
    pub static kCGImageSourceThumbnailMaxPixelSize: *mut AnyObject;
}
#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    pub fn CFRetain(p: *const c_void) -> *const c_void;
    pub fn CFRelease(p: *const c_void);
}
#[link(name = "CoreMedia", kind = "framework")]
unsafe extern "C" {
    pub fn CMSampleBufferGetFormatDescription(p: *const c_void) -> *const c_void;
    pub fn CMAudioFormatDescriptionGetStreamBasicDescription(p: *const c_void) -> *const Asbd;
    pub fn CMSampleBufferGetNumSamples(p: *const c_void) -> isize;
    pub fn CMSampleBufferGetAudioBufferListWithRetainedBlockBuffer(
        p: *const c_void,
        needed: *mut usize,
        list: *mut AudioList,
        size: usize,
        allocator: *const c_void,
        memory: *const c_void,
        flags: u32,
        block: *mut *const c_void,
    ) -> i32;
}
#[link(name = "CoreGraphics", kind = "framework")]
unsafe extern "C" {
    pub fn CGPreflightScreenCaptureAccess() -> bool;
    pub fn CGColorSpaceCreateDeviceRGB() -> *mut c_void;
    pub fn CGBitmapContextCreate(
        data: *mut c_void,
        w: usize,
        h: usize,
        bits: usize,
        row: usize,
        space: *const c_void,
        info: u32,
    ) -> *mut c_void;
    pub fn CGBitmapContextCreateImage(ctx: *const c_void) -> *mut c_void;
    pub fn CGContextDrawImage(ctx: *mut c_void, rect: Rect, image: *const c_void);
    pub fn CGContextSetRGBFillColor(ctx: *mut c_void, r: f64, g: f64, b: f64, a: f64);
    pub fn CGContextFillRect(ctx: *mut c_void, rect: Rect);
}
unsafe extern "C" {
    pub fn dispatch_queue_create(
        label: *const std::ffi::c_char,
        attr: *const c_void,
    ) -> *mut c_void;
    pub fn dispatch_release(p: *mut c_void);
}
pub struct Cf(pub *mut c_void);
impl Drop for Cf {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe { CFRelease(self.0) }
        }
    }
}
/// CGImage is immutable and Core Graphics permits use across threads. Owns one retain.
pub struct Image(pub Cf);
unsafe impl Send for Image {}
impl Image {
    pub unsafe fn retained(p: *mut c_void) -> Option<Self> {
        if p.is_null() {
            None
        } else {
            CFRetain(p);
            Some(Self(Cf(p)))
        }
    }
}
/// Only instantiated for immutable SCShareableContent snapshots in completion handlers.
pub struct Shareable(pub Obj);
unsafe impl Send for Shareable {}

#[repr(C)]
pub struct CGContextOpaque {
    _private: [u8; 0],
}
unsafe impl RefEncode for CGContextOpaque {
    const ENCODING_REF: Encoding = Encoding::Pointer(&Encoding::Struct("CGContext", &[]));
}
#[repr(C)]
pub struct CGImageOpaque {
    _private: [u8; 0],
}
unsafe impl RefEncode for CGImageOpaque {
    const ENCODING_REF: Encoding = Encoding::Pointer(&Encoding::Struct("CGImage", &[]));
}
