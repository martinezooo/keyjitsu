//! macOS menu-bar status item for the GUI app.
//!
//! The artwork is a monochrome template image so AppKit applies the native
//! menu-bar foreground color automatically in light/dark appearances.

#![cfg(target_os = "macos")]

use std::cell::Cell;

use objc2::rc::autoreleasepool;
use objc2::runtime::{AnyObject, Bool};
use objc2::{class, msg_send};
use objc2_foundation::{NSSize, NSString};

type Id = *mut AnyObject;

const NSSQUARE_STATUS_ITEM_LENGTH: f64 = -2.0;
const ICON_POINTS: f64 = 18.0;
const ICON_BYTES: &[u8] = include_bytes!("../resources/tray_template.png");

thread_local! {
    // Keeping the status item pointer for the lifetime of the main thread also
    // makes the ownership intent explicit even though NSStatusBar retains it.
    static STATUS_ITEM: Cell<Id> = const { Cell::new(std::ptr::null_mut()) };
}

pub fn install() {
    STATUS_ITEM.with(|slot| {
        if !slot.get().is_null() {
            return;
        }
        let item = autoreleasepool(|_| unsafe { build_status_item() });
        if !item.is_null() {
            slot.set(item);
        }
    });
}

unsafe fn build_status_item() -> Id {
    let bytes: Id = msg_send![
        class!(NSData),
        dataWithBytes: ICON_BYTES.as_ptr(),
        length: ICON_BYTES.len()
    ];
    if bytes.is_null() {
        return std::ptr::null_mut();
    }

    let image: Id = msg_send![class!(NSImage), alloc];
    let image: Id = msg_send![image, initWithData: bytes];
    if image.is_null() {
        return std::ptr::null_mut();
    }
    let _: () = msg_send![image, setTemplate: Bool::YES];
    let _: () = msg_send![image, setSize: NSSize::new(ICON_POINTS, ICON_POINTS)];

    let bar: Id = msg_send![class!(NSStatusBar), systemStatusBar];
    let item: Id = msg_send![bar, statusItemWithLength: NSSQUARE_STATUS_ITEM_LENGTH];
    if item.is_null() {
        return std::ptr::null_mut();
    }
    let button: Id = msg_send![item, button];
    if !button.is_null() {
        let _: () = msg_send![button, setImage: image];
        let tooltip = NSString::from_str("Keyjitsu");
        let _: () = msg_send![button, setToolTip: &*tooltip];
    }
    item
}
