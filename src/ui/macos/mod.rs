#[cfg(target_os = "macos")]
mod activate;
#[cfg(target_os = "macos")]
mod dialog;

#[cfg(target_os = "macos")]
pub use activate::activate_app;
#[cfg(target_os = "macos")]
pub use dialog::{alert, choose_yes_no, open_path, prompt_text};

#[cfg(not(target_os = "macos"))]
pub fn prompt_text(_title: &str, _message: &str) -> Option<String> {
    None
}

#[cfg(not(target_os = "macos"))]
pub fn choose_yes_no(_title: &str, _message: &str) -> bool {
    false
}

#[cfg(not(target_os = "macos"))]
pub fn alert(_title: &str, _message: &str) {}

#[cfg(not(target_os = "macos"))]
pub fn open_path(_path: &std::path::Path) -> anyhow::Result<()> {
    anyhow::bail!("open path is macOS only")
}
