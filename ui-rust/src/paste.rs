//! egui receives only text paste events. Handle image paste inside this app's event loop.
use block2::RcBlock;
use eframe::egui;
use objc2::{rc::Retained, runtime::AnyObject};
use objc2_app_kit::{NSEvent, NSEventMask, NSEventModifierFlags as Flags, NSPasteboard};
use objc2_foundation::NSString;
use std::{cell::Cell, ptr::NonNull, rc::Rc};
#[path = "../../src/clipboard_image_types.rs"]
mod formats;

fn board_image(board: &NSPasteboard) -> Option<i64> {
    let version = board.changeCount();
    let types = board.types()?;
    let offers = |name| types.containsObject(&NSString::from_str(name));
    let bitmap = formats::BITMAP_TYPES.iter().any(|name| offers(name));
    let file = || {
        board
            .stringForType(&NSString::from_str("public.file-url"))
            .as_deref()
            .and_then(formats::image_file)
            .is_some()
    };
    ((bitmap || (offers("public.file-url") && file())) && board.changeCount() == version)
        .then_some(version as i64)
}
pub fn image_version() -> Option<i64> {
    board_image(&NSPasteboard::generalPasteboard())
}

fn command_v(flags: Flags, key: &str) -> bool {
    flags.intersection(Flags::Command | Flags::Control | Flags::Option | Flags::Shift)
        == Flags::Command
        && key.eq_ignore_ascii_case("v")
}

pub struct Monitor {
    token: Retained<AnyObject>,
    pub enabled: Rc<Cell<bool>>,
    pub pending: Rc<Cell<Option<i64>>>,
}
impl Monitor {
    pub fn install(ctx: egui::Context) -> Option<Self> {
        let enabled = Rc::new(Cell::new(false));
        let pending = Rc::new(Cell::new(None));
        let (active, capture) = (enabled.clone(), pending.clone());
        let handler = RcBlock::new(move |ptr: NonNull<NSEvent>| {
            // AppKit owns the event for this callback. Return that same pointer for ordinary input.
            let event = unsafe { ptr.as_ref() };
            if active.get()
                && event.modifierFlags().contains(Flags::Command)
                && event
                    .charactersIgnoringModifiers()
                    .is_some_and(|key| command_v(event.modifierFlags(), &key.to_string()))
                && let Some(version) = image_version()
            {
                if !event.isARepeat() {
                    capture.set(Some(version));
                    ctx.request_repaint();
                }
                return std::ptr::null_mut();
            }
            ptr.as_ptr()
        });
        // The block returns either its original event or nil. AppKit retains the block.
        let token = unsafe {
            NSEvent::addLocalMonitorForEventsMatchingMask_handler(NSEventMask::KeyDown, &handler)
        }?;
        Some(Self {
            token,
            enabled,
            pending,
        })
    }
}
impl Drop for Monitor {
    fn drop(&mut self) {
        // This is exactly the token returned by addLocalMonitor, removed once on the UI thread.
        unsafe { NSEvent::removeMonitor(&self.token) };
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn shortcut_stays_local_to_plain_command_v() {
        assert!(command_v(Flags::Command, "v"));
        assert!(command_v(Flags::Command | Flags::CapsLock, "V"));
        assert!(!command_v(Flags::empty(), "v"));
        assert!(!command_v(Flags::Command | Flags::Shift, "v"));
        assert!(!command_v(Flags::Command, "c"));
    }
    #[test]
    #[ignore = "requires the macOS named-pasteboard service"]
    fn named_board_routes_images_to_daybook_and_leaves_text_paste_to_the_editor() {
        let board = NSPasteboard::pasteboardWithUniqueName();
        let s = NSString::from_str;
        assert!(board.setString_forType(&s("plain note"), &s("public.utf8-plain-text")));
        assert_eq!(board_image(&board), None);
        assert!(board.setString_forType(&s("image fixture metadata"), &s("public.png")));
        assert_eq!(board_image(&board), Some(board.changeCount() as i64));
        board.clearContents();
        assert!(board.setString_forType(&s("file:///tmp/a%20b.PNG"), &s("public.file-url")));
        assert_eq!(board_image(&board), Some(board.changeCount() as i64));
        board.clearContents();
        assert!(
            board.setString_forType(&s("https://example.com/picture.png"), &s("public.file-url"))
        );
        assert_eq!(board_image(&board), None);
        // Release only the private board allocated for this test.
        unsafe {
            let _: () = objc2::msg_send![&*board, releaseGlobally];
        }
    }
}
