use mac_notification_sys::{Notification, set_application};

/// Prefer the SwiftUI app id so banners attribute to the menu-bar app the user runs.
const BUNDLE_ID: &str = "com.piya.swiftui";

pub fn request_permission() {
    let _ = set_application(BUNDLE_ID);
}

pub fn send_popup(title: &str, subtitle: &str, body: &str) {
    if !popup_enabled() {
        return;
    }
    let _ = set_application(BUNDLE_ID);
    let mut binding = Notification::new();
    let mut n = binding.title(title).message(body);
    if !subtitle.is_empty() {
        n = n.subtitle(subtitle);
    }
    let _ = n.send();
}

fn popup_enabled() -> bool {
    match std::env::var("PIYA_NOTIFY")
        .unwrap_or_default()
        .trim()
        .to_lowercase()
        .as_str()
    {
        "0" | "false" | "off" | "none" | "badge" => false,
        "popup" | "notification" | "notifications" | "banner" | "all" | "" => true,
        _ => true,
    }
}
