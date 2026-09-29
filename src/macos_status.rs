//! Native macOS menu-bar item.
//!
//! The status item stays alive when the main window is hidden. Actions are
//! bridged into the egui update loop with tiny atomic flags, so AppKit never
//! mutates `App` state directly.

#![cfg(target_os = "macos")]

use std::cell::Cell;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::OnceLock;

use objc2::rc::autoreleasepool;
use objc2::runtime::{AnyClass, AnyObject, Bool, ClassBuilder, Sel};
use objc2::{class, msg_send, sel};
use objc2_foundation::{NSSize, NSString};

type Id = *mut AnyObject;

const NSSQUARE_STATUS_ITEM_LENGTH: f64 = -2.0;
const ICON_POINTS: f64 = 18.0;
const NSCONTROL_STATE_OFF: isize = 0;
const NSCONTROL_STATE_ON: isize = 1;
const ICON_BYTES: &[u8] = include_bytes!("../resources/tray_template.png");

static OPEN_REQUESTED: AtomicBool = AtomicBool::new(false);
static TOGGLE_GUARD_REQUESTED: AtomicBool = AtomicBool::new(false);
static TOGGLE_MINIMAP_REQUESTED: AtomicBool = AtomicBool::new(false);
static QUIT_REQUESTED: AtomicBool = AtomicBool::new(false);
static EGUI_CTX: OnceLock<eframe::egui::Context> = OnceLock::new();

thread_local! {
    static STATUS_ITEM: Cell<Id> = const { Cell::new(std::ptr::null_mut()) };
    static ACTION_TARGET: Cell<Id> = const { Cell::new(std::ptr::null_mut()) };
    static GUARD_ITEM: Cell<Id> = const { Cell::new(std::ptr::null_mut()) };
    static MINIMAP_ITEM: Cell<Id> = const { Cell::new(std::ptr::null_mut()) };
    static QUIT_ITEM: Cell<Id> = const { Cell::new(std::ptr::null_mut()) };
}

#[derive(Default)]
pub struct Requests {
    pub open: bool,
    pub toggle_guard: bool,
    pub toggle_minimap: bool,
    pub quit: bool,
}

pub fn install(ctx: eframe::egui::Context) {
    let _ = EGUI_CTX.set(ctx);
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

pub fn take_requests() -> Requests {
    Requests {
        open: OPEN_REQUESTED.swap(false, Ordering::AcqRel),
        toggle_guard: TOGGLE_GUARD_REQUESTED.swap(false, Ordering::AcqRel),
        toggle_minimap: TOGGLE_MINIMAP_REQUESTED.swap(false, Ordering::AcqRel),
        quit: QUIT_REQUESTED.swap(false, Ordering::AcqRel),
    }
}

pub fn sync_state(guard_enabled: bool, minimap_locked: bool, can_quit: bool) {
    autoreleasepool(|_| unsafe {
        GUARD_ITEM.with(|slot| set_checked(slot.get(), guard_enabled));
        MINIMAP_ITEM.with(|slot| set_checked(slot.get(), minimap_locked));
        QUIT_ITEM.with(|slot| {
            let item = slot.get();
            if !item.is_null() {
                let _: () = msg_send![item, setEnabled: Bool::new(can_quit)];
            }
        });
    });
}

fn wake() {
    if let Some(ctx) = EGUI_CTX.get() {
        ctx.request_repaint();
    }
}

extern "C" fn open_action(_this: &AnyObject, _cmd: Sel, _sender: *mut AnyObject) {
    OPEN_REQUESTED.store(true, Ordering::Release);
    wake();
    autoreleasepool(|_| unsafe {
        let app: Id = msg_send![class!(NSApplication), sharedApplication];
        let _: () = msg_send![app, activateIgnoringOtherApps: Bool::YES];
    });
}

extern "C" fn guard_action(_this: &AnyObject, _cmd: Sel, _sender: *mut AnyObject) {
    TOGGLE_GUARD_REQUESTED.store(true, Ordering::Release);
    wake();
}

extern "C" fn minimap_action(_this: &AnyObject, _cmd: Sel, _sender: *mut AnyObject) {
    TOGGLE_MINIMAP_REQUESTED.store(true, Ordering::Release);
    wake();
}

extern "C" fn quit_action(_this: &AnyObject, _cmd: Sel, _sender: *mut AnyObject) {
    QUIT_REQUESTED.store(true, Ordering::Release);
    wake();
}

fn action_class() -> &'static AnyClass {
    static CLASS: OnceLock<&'static AnyClass> = OnceLock::new();
    CLASS.get_or_init(|| {
        if let Some(class) = AnyClass::get(c"KeyjitsuStatusTarget") {
            return class;
        }
        let mut builder = ClassBuilder::new(c"KeyjitsuStatusTarget", class!(NSObject))
            .expect("create Keyjitsu status target class");
        unsafe {
            builder.add_method(sel!(keyjitsuOpen:), open_action as extern "C" fn(_, _, _));
            builder.add_method(
                sel!(keyjitsuToggleGuard:),
                guard_action as extern "C" fn(_, _, _),
            );
            builder.add_method(
                sel!(keyjitsuToggleMinimap:),
                minimap_action as extern "C" fn(_, _, _),
            );
            builder.add_method(sel!(keyjitsuQuit:), quit_action as extern "C" fn(_, _, _));
        }
        builder.register()
    })
}

unsafe fn menu_item(title: &str, action: Sel, target: Id) -> Id {
    let title = NSString::from_str(title);
    let empty = NSString::from_str("");
    let item: Id = msg_send![class!(NSMenuItem), alloc];
    let item: Id = msg_send![
        item,
        initWithTitle: &*title,
        action: action,
        keyEquivalent: &*empty
    ];
    let _: () = msg_send![item, setTarget: target];
    item
}

unsafe fn set_checked(item: Id, checked: bool) {
    if item.is_null() {
        return;
    }
    let state = if checked {
        NSCONTROL_STATE_ON
    } else {
        NSCONTROL_STATE_OFF
    };
    let _: () = msg_send![item, setState: state];
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

    let target: Id = msg_send![action_class(), new];
    ACTION_TARGET.with(|slot| slot.set(target));

    let menu_title = NSString::from_str("Keyjitsu");
    let menu: Id = msg_send![class!(NSMenu), alloc];
    let menu: Id = msg_send![menu, initWithTitle: &*menu_title];
    let _: () = msg_send![menu, setAutoenablesItems: Bool::NO];

    let open = menu_item("Open Keyjitsu", sel!(keyjitsuOpen:), target);
    let guard = menu_item("Guard", sel!(keyjitsuToggleGuard:), target);
    let minimap = menu_item("Minimap", sel!(keyjitsuToggleMinimap:), target);
    let quit = menu_item("Quit Keyjitsu", sel!(keyjitsuQuit:), target);
    let separator_a: Id = msg_send![class!(NSMenuItem), separatorItem];
    let separator_b: Id = msg_send![class!(NSMenuItem), separatorItem];

    let _: () = msg_send![menu, addItem: open];
    let _: () = msg_send![menu, addItem: separator_a];
    let _: () = msg_send![menu, addItem: minimap];
    let _: () = msg_send![menu, addItem: guard];
    let _: () = msg_send![menu, addItem: separator_b];
    let _: () = msg_send![menu, addItem: quit];

    GUARD_ITEM.with(|slot| slot.set(guard));
    MINIMAP_ITEM.with(|slot| slot.set(minimap));
    QUIT_ITEM.with(|slot| slot.set(quit));

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
    let _: () = msg_send![item, setMenu: menu];
    let _: () = msg_send![item, setVisible: Bool::YES];
    // NSStatusBar does not keep the returned item alive for us. A raw pointer
    // in STATUS_ITEM is only an address, not ownership, so explicitly retain
    // this process-lifetime object or macOS removes the icon again.
    let _: Id = msg_send![item, retain];
    item
}
