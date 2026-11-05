//! Bring Piya to the foreground before showing a window (required for WKWebView).
#![allow(deprecated, unexpected_cfgs)]

use objc::{class, msg_send, sel, sel_impl};

pub fn activate_app() {
    unsafe {
        let app: cocoa::base::id = msg_send![class!(NSApplication), sharedApplication];
        let _: () = msg_send![app, activateIgnoringOtherApps: cocoa::base::YES];
    }
}
