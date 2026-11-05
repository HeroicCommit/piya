//! macOS Spotlight-style floating panel configuration.

use objc::{sel, sel_impl};
use tao::platform::macos::WindowExtMacOS;
use tao::window::Window;

#[link(name = "CoreGraphics", kind = "framework")]
extern "C" {
    fn CGWindowLevelForKey(key: i32) -> i32;
}

// kCGPopUpMenuWindowLevelKey → level 101 on macOS.
// Above normal/maximized app windows; below Spotlight, screenshot overlays, and
// other system UI that uses shielding/maximum levels.
const K_CG_POPUP_MENU_WINDOW_LEVEL_KEY: i32 = 11;

// CanJoinAllSpaces | Stationary | IgnoresCycle | FullScreenAuxiliary
const COLLECTION_BEHAVIOR: usize = 1 | 16 | 64 | 256;

fn panel_level() -> i64 {
    unsafe { CGWindowLevelForKey(K_CG_POPUP_MENU_WINDOW_LEVEL_KEY) as i64 }
}

pub fn apply(window: &Window) {
    configure(window);
    center(window);
    present(window);
}

pub fn center(window: &Window) {
    unsafe {
        let ns_window = window.ns_window() as cocoa::base::id;
        let _: () = objc::msg_send![ns_window, center];
    }
}

pub fn focus(window: &Window) {
    configure(window);
    center(window);
    present(window);
}

/// Re-assert z-order without stealing key focus from Spotlight / screenshot tools.
pub fn raise(window: &Window) {
    configure(window);
    order_front(window, false);
}

fn configure(window: &Window) {
    unsafe {
        let ns_window = window.ns_window() as cocoa::base::id;
        let _: () = objc::msg_send![ns_window, setLevel: panel_level()];
        let _: () = objc::msg_send![ns_window, setCollectionBehavior: COLLECTION_BEHAVIOR];
        let _: () = objc::msg_send![ns_window, setHasShadow: cocoa::base::YES];
        let _: () = objc::msg_send![ns_window, setHidesOnDeactivate: cocoa::base::NO];
        let _: () = objc::msg_send![ns_window, setCanHide: cocoa::base::NO];
    }
}

fn present(window: &Window) {
    order_front(window, true);
}

fn order_front(window: &Window, make_key: bool) {
    unsafe {
        let ns_window = window.ns_window() as cocoa::base::id;
        let _: () = objc::msg_send![ns_window, orderFrontRegardless];
        if make_key {
            let _: () = objc::msg_send![ns_window, makeKeyWindow];
        }
    }
}
