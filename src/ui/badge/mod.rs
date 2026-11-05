#[cfg(all(target_os = "macos", feature = "legacy-ui"))]
mod imp;

#[cfg(all(target_os = "macos", feature = "legacy-ui"))]
pub use imp::{install_launch_agent, run, uninstall_launch_agent};

#[cfg(not(all(target_os = "macos", feature = "legacy-ui")))]
pub fn run(_settings: crate::config::Settings) -> anyhow::Result<()> {
    #[cfg(target_os = "macos")]
    {
        anyhow::bail!(
            "Legacy wry menu bar is disabled in this build. \
             Rebuild with `--features legacy-ui`, or use PiyaSwift.app."
        )
    }
    #[cfg(not(target_os = "macos"))]
    {
        anyhow::bail!("Menu bar badge is macOS only. Use: piya watch")
    }
}

#[cfg(not(all(target_os = "macos", feature = "legacy-ui")))]
pub fn install_launch_agent(_settings: &crate::config::Settings) -> anyhow::Result<()> {
    anyhow::bail!("Menu bar badge requires macOS and `--features legacy-ui`")
}

#[cfg(not(all(target_os = "macos", feature = "legacy-ui")))]
pub fn uninstall_launch_agent(_settings: &crate::config::Settings) -> anyhow::Result<()> {
    anyhow::bail!("Menu bar badge requires macOS and `--features legacy-ui`")
}
