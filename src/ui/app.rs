use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result};

use crate::config::Settings;
use crate::launch_agent;

pub fn install(settings: &Settings) -> Result<()> {
    let app = locate_app_bundle()?;
    let dest = PathBuf::from("/Applications/Piya.app");

    remove_running_instance(&settings.data_dir);

    if dest.exists() {
        std::fs::remove_dir_all(&dest).context("remove old Piya.app")?;
    }

    crate::updater::copy_bundle_preserving_metadata(&app, &dest)?;
    let config_path = crate::config::write_app_support_config(settings)?;
    install_login_item(settings, &dest)?;

    // `install_login_item` registers a LaunchAgent with `RunAtLoad = true`, which
    // already starts the app on `launchctl bootstrap`. Only call `open -a` as a fallback
    // if no instance came up.
    std::thread::sleep(std::time::Duration::from_millis(300));
    if !is_installed_app_running() {
        Command::new("open")
            .arg("-a")
            .arg(&dest)
            .status()
            .context("launch Piya")?;
    }

    println!("Installed Piya to /Applications/Piya.app");
    println!("Config: {}", config_path.display());
    println!("Login item registered (starts at sign-in).");
    Ok(())
}

pub fn update_from_local(settings: &Settings, force: bool) -> Result<()> {
    let source = locate_app_bundle()?;
    let dest = PathBuf::from(crate::updater::INSTALLED_APP_PATH);
    let staged = crate::updater::stage_local_bundle(&source, &dest)?;
    crate::updater::apply_staged_update_cli(settings, &staged, &dest, force)?;
    println!(
        "Updated {} in place (config, data, and login item untouched).",
        dest.display()
    );
    Ok(())
}

pub fn uninstall(_settings: &Settings) -> Result<()> {
    launch_agent::unregister_label(launch_agent::APP_LABEL)?;
    launch_agent::unregister_label("com.piya.badge")?;
    let dest = PathBuf::from("/Applications/Piya.app");
    if dest.exists() {
        std::fs::remove_dir_all(&dest).context("remove Piya.app")?;
    }
    println!("Removed Piya from Applications");
    Ok(())
}

fn locate_app_bundle() -> Result<PathBuf> {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let candidates = [
        PathBuf::from("dist/build.noindex/PiyaSwift.app"),
        root.join("dist/build.noindex/PiyaSwift.app"),
        PathBuf::from("dist/PiyaSwift.app"),
        root.join("dist/PiyaSwift.app"),
        PathBuf::from("dist/build.noindex/Piya.app"),
        root.join("dist/build.noindex/Piya.app"),
        PathBuf::from("dist/Piya.app"),
        root.join("dist/Piya.app"),
    ];
    for path in candidates {
        if app_executable(&path).is_some() {
            return Ok(path);
        }
    }
    anyhow::bail!("PiyaSwift.app not found. Run: ./scripts/build-swift-app.sh");
}

fn app_executable(app: &Path) -> Option<PathBuf> {
    for name in ["Piya", "piya"] {
        let bin = app.join("Contents/MacOS").join(name);
        if bin.is_file() {
            return Some(bin);
        }
    }
    None
}

fn install_login_item(settings: &Settings, app: &Path) -> Result<()> {
    let plist_dir = home()?.join("Library/LaunchAgents");
    std::fs::create_dir_all(&plist_dir)?;
    let plist_path = plist_dir.join(format!("{}.plist", launch_agent::APP_LABEL));

    let binary = app_executable(app).context("app bundle has no MacOS executable")?;
    let data_dir = settings.data_dir.canonicalize().unwrap_or_else(|_| settings.data_dir.clone());
    let workspace = settings
        .workspace
        .canonicalize()
        .unwrap_or_else(|_| settings.workspace.clone());

    let plist = format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key><string>{}</string>
  <key>ProgramArguments</key>
  <array>
    <string>{}</string>
  </array>
  <key>EnvironmentVariables</key>
  <dict>
    <key>PIYA_DATA_DIR</key><string>{}</string>
    <key>PIYA_WORKSPACE</key><string>{}</string>
    <key>PIYA_NOTIFY</key><string>all</string>
  </dict>
  <key>RunAtLoad</key><true/>
  <key>KeepAlive</key><false/>
</dict>
</plist>"#,
        launch_agent::APP_LABEL,
        binary.display(),
        data_dir.display(),
        workspace.display(),
    );

    std::fs::write(&plist_path, plist)?;
    launch_agent::register_plist(&plist_path)
}

fn remove_running_instance(data_dir: &Path) {
    let lock = data_dir.join("piya-badge.lock");
    if lock.exists() {
        if let Ok(pid) = std::fs::read_to_string(&lock) {
            if let Ok(pid) = pid.trim().parse::<u32>() {
                let _ = Command::new("kill").arg(pid.to_string()).status();
            }
        }
        let _ = std::fs::remove_file(lock);
    }
    let _ = Command::new("pkill")
        .args(["-f", "/Applications/Piya.app/Contents/MacOS/"])
        .status();
}

fn is_installed_app_running() -> bool {
    Command::new("pgrep")
        .args(["-f", "/Applications/Piya.app/Contents/MacOS/"])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

fn home() -> Result<PathBuf> {
    std::env::var("HOME").map(PathBuf::from).context("HOME not set")
}

pub fn launched_from_app_bundle() -> bool {
    std::env::current_exe()
        .map(|p| p.to_string_lossy().contains(".app/Contents/MacOS/"))
        .unwrap_or(false)
}
