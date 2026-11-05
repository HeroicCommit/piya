#[cfg(target_os = "macos")]
mod imp;

#[cfg(target_os = "macos")]
pub use imp::{register_plist, unregister_label, APP_LABEL};

#[cfg(not(target_os = "macos"))]
pub const APP_LABEL: &str = "com.piya.app";

#[cfg(not(target_os = "macos"))]
pub fn register_plist(_plist_path: &std::path::Path) -> anyhow::Result<()> {
    Ok(())
}

#[cfg(not(target_os = "macos"))]
pub fn unregister_label(_label: &str) -> anyhow::Result<()> {
    Ok(())
}
