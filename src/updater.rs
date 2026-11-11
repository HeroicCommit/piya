//! Self-update & atomic `.app` bundle replacement for Piya on macOS.
//!
//! Updating replaces `/Applications/Piya.app` atomically in place without touching
//! `~/Library/Application Support/Piya/.env`, `data/`, `workspace/`, or LaunchAgents.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use anyhow::{anyhow, bail, Context, Result};
use reqwest::header::{ACCEPT, USER_AGENT};
use semver::Version;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::config::Settings;
use crate::tasks::{TaskQueue, TaskStatus};
use crate::worker;

pub const DEFAULT_GITHUB_REPO: &str = "HeroicCommit/piya";
pub const INSTALLED_APP_PATH: &str = "/Applications/Piya.app";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UpdateCheck {
    pub current_version: String,
    pub latest_version: String,
    pub update_available: bool,
    pub release_url: String,
    pub release_notes: String,
    pub published_at: String,
    pub asset_name: Option<String>,
    pub asset_url: Option<String>,
    pub expected_sha256: Option<String>,
}

#[derive(Debug, Deserialize)]
struct GithubRelease {
    tag_name: String,
    #[serde(default)]
    html_url: String,
    #[serde(default)]
    body: Option<String>,
    #[serde(default)]
    published_at: Option<String>,
    #[serde(default)]
    prerelease: bool,
    #[serde(default)]
    draft: bool,
    #[serde(default)]
    assets: Vec<GithubAsset>,
}

#[derive(Debug, Clone, Deserialize)]
struct GithubAsset {
    name: String,
    browser_download_url: String,
    #[serde(default)]
    digest: Option<String>,
}

pub fn current_version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

fn github_repo() -> String {
    std::env::var("PIYA_UPDATE_REPO")
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| DEFAULT_GITHUB_REPO.to_string())
}

fn latest_release_api_url() -> String {
    if let Ok(url) = std::env::var("PIYA_UPDATE_API_URL") {
        let trimmed = url.trim();
        if !trimmed.is_empty() {
            return trimmed.to_string();
        }
    }
    format!(
        "https://api.github.com/repos/{}/releases/latest",
        github_repo()
    )
}

fn http_client(timeout: Duration) -> Result<reqwest::Client> {
    reqwest::Client::builder()
        .timeout(timeout)
        .connect_timeout(Duration::from_secs(15))
        .build()
        .context("build HTTP client")
}

/// Normalize a git tag such as `"v0.2.1"` into a `semver::Version`.
pub fn parse_version_tag(raw: &str) -> Result<Version> {
    let trimmed = raw.trim().trim_start_matches(['v', 'V']);
    Version::parse(trimmed).with_context(|| format!("invalid semantic version `{raw}`"))
}

pub fn is_newer_version(current: &str, candidate: &str) -> Result<bool> {
    let cur = parse_version_tag(current)?;
    let cand = parse_version_tag(candidate)?;
    Ok(cand > cur)
}

/// Select the macOS `.zip` release asset matching the current CPU architecture.
fn select_macos_asset(assets: &[GithubAsset]) -> Option<&GithubAsset> {
    let arch = std::env::consts::ARCH; // "aarch64" or "x86_64"
    let preferred_tokens: &[&str] = match arch {
        "aarch64" => &["arm64", "aarch64", "universal"],
        "x86_64" => &["x86_64", "amd64", "universal"],
        _ => &["universal"],
    };

    let zip_assets: Vec<&GithubAsset> = assets
        .iter()
        .filter(|a| a.name.to_ascii_lowercase().ends_with(".zip"))
        .collect();

    for token in preferred_tokens {
        if let Some(found) = zip_assets
            .iter()
            .copied()
            .find(|a| a.name.to_ascii_lowercase().contains(token))
        {
            return Some(found);
        }
    }

    zip_assets
        .iter()
        .copied()
        .find(|a| a.name.to_ascii_lowercase().contains("piya"))
        .or_else(|| zip_assets.first().copied())
}

/// Extract the hex SHA-256 for `asset_name` from a `SHA256SUMS` text body.
pub fn parse_sha256sums(sums_text: &str, asset_name: &str) -> Option<String> {
    for line in sums_text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let mut parts = line.split_whitespace();
        let Some(hash) = parts.next() else { continue };
        let Some(file_part) = parts.next() else {
            // Single-hash file fallback when only one checksum is present.
            if hash.len() == 64 && hash.chars().all(|c| c.is_ascii_hexdigit()) {
                return Some(hash.to_ascii_lowercase());
            }
            continue;
        };
        let clean_name = file_part.trim_start_matches('*');
        let basename = Path::new(clean_name)
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or(clean_name);
        if basename == asset_name
            && hash.len() == 64
            && hash.chars().all(|c| c.is_ascii_hexdigit())
        {
            return Some(hash.to_ascii_lowercase());
        }
    }
    None
}

fn parse_github_digest(digest: Option<&str>) -> Option<String> {
    let raw = digest?.trim();
    let hex = raw.strip_prefix("sha256:").unwrap_or(raw);
    if hex.len() == 64 && hex.chars().all(|c| c.is_ascii_hexdigit()) {
        Some(hex.to_ascii_lowercase())
    } else {
        None
    }
}

/// Query GitHub Releases for the latest published version of Piya.
pub fn check_for_update() -> Result<UpdateCheck> {
    let api_url = latest_release_api_url();
    crate::runtime::block_on(async move { check_for_update_async(&api_url).await })
}

async fn check_for_update_async(api_url: &str) -> Result<UpdateCheck> {
    let current = current_version().to_string();
    let client = http_client(Duration::from_secs(30))?;
    let ua = format!("piya/{current}");

    let resp = client
        .get(api_url)
        .header(USER_AGENT, &ua)
        .header(ACCEPT, "application/vnd.github+json")
        .send()
        .await
        .context("request latest GitHub release")?;

    // If the repository has no published releases yet, report up-to-date cleanly.
    if resp.status() == reqwest::StatusCode::NOT_FOUND {
        return Ok(UpdateCheck {
            current_version: current.clone(),
            latest_version: current,
            update_available: false,
            release_url: format!("https://github.com/{}/releases", github_repo()),
            release_notes: String::new(),
            published_at: String::new(),
            asset_name: None,
            asset_url: None,
            expected_sha256: None,
        });
    }

    if !resp.status().is_success() {
        bail!(
            "GitHub release check returned HTTP {}",
            resp.status().as_u16()
        );
    }

    let rel: GithubRelease = resp
        .json()
        .await
        .context("parse GitHub release response")?;

    build_update_check(&client, &ua, &current, rel).await
}

async fn build_update_check(
    client: &reqwest::Client,
    ua: &str,
    current: &str,
    rel: GithubRelease,
) -> Result<UpdateCheck> {
    if rel.draft || rel.prerelease {
        return Ok(UpdateCheck {
            current_version: current.to_string(),
            latest_version: current.to_string(),
            update_available: false,
            release_url: rel.html_url,
            release_notes: String::new(),
            published_at: rel.published_at.unwrap_or_default(),
            asset_name: None,
            asset_url: None,
            expected_sha256: None,
        });
    }

    let latest_ver = parse_version_tag(&rel.tag_name)?.to_string();
    let update_available = is_newer_version(current, &latest_ver)?;
    let chosen = select_macos_asset(&rel.assets).cloned();

    let mut expected_sha256 = chosen
        .as_ref()
        .and_then(|a| parse_github_digest(a.digest.as_deref()));

    if expected_sha256.is_none() {
        if let Some(ref asset) = chosen {
            if let Some(sums_asset) = rel.assets.iter().find(|a| {
                let lower = a.name.to_ascii_lowercase();
                lower == "sha256sums"
                    || lower == "sha256sums.txt"
                    || lower == format!("{}.sha256", asset.name.to_ascii_lowercase())
            }) {
                if let Ok(resp) = client
                    .get(&sums_asset.browser_download_url)
                    .header(USER_AGENT, ua)
                    .send()
                    .await
                {
                    if resp.status().is_success() {
                        if let Ok(text) = resp.text().await {
                            expected_sha256 = parse_sha256sums(&text, &asset.name);
                        }
                    }
                }
            }
        }
    }

    Ok(UpdateCheck {
        current_version: current.to_string(),
        latest_version: latest_ver,
        update_available,
        release_url: rel.html_url,
        release_notes: rel.body.unwrap_or_default().trim().to_string(),
        published_at: rel.published_at.unwrap_or_default(),
        asset_name: chosen.as_ref().map(|a| a.name.clone()),
        asset_url: chosen.map(|a| a.browser_download_url),
        expected_sha256,
    })
}

pub fn sha256_file(path: &Path) -> Result<String> {
    let bytes = std::fs::read(path).with_context(|| format!("read {}", path.display()))?;
    let mut hasher = Sha256::new();
    hasher.update(&bytes);
    Ok(format!("{:x}", hasher.finalize()))
}

pub fn verify_sha256(path: &Path, expected_hex: &str) -> Result<()> {
    let actual = sha256_file(path)?;
    let expected = expected_hex.trim().to_ascii_lowercase();
    if actual != expected {
        bail!(
            "SHA-256 mismatch for {}: expected {expected}, got {actual}",
            path.display()
        );
    }
    Ok(())
}

/// Validate that a directory is a runnable Piya macOS `.app` bundle.
pub fn validate_app_bundle(bundle: &Path) -> Result<()> {
    let plist = bundle.join("Contents/Info.plist");
    if !plist.is_file() {
        bail!("invalid app bundle (missing Info.plist): {}", bundle.display());
    }
    let has_exec = ["Piya", "piya"]
        .iter()
        .any(|name| bundle.join("Contents/MacOS").join(name).is_file());
    if !has_exec {
        bail!(
            "invalid app bundle (missing Contents/MacOS executable): {}",
            bundle.display()
        );
    }

    #[cfg(target_os = "macos")]
    {
        let status = Command::new("/usr/bin/codesign")
            .args(["--verify", "--deep"])
            .arg(bundle)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .context("run codesign --verify")?;
        if !status.success() {
            bail!(
                "code signature verification failed on {}",
                bundle.display()
            );
        }
    }
    Ok(())
}

fn unregister_launch_services(bundle: &Path) {
    #[cfg(target_os = "macos")]
    {
        let lsreg = "/System/Library/Frameworks/CoreServices.framework/Frameworks/LaunchServices.framework/Support/lsregister";
        if Path::new(lsreg).is_file() {
            let _ = Command::new(lsreg)
                .arg("-u")
                .arg(bundle)
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status();
        }
    }
}

/// Copy an `.app` bundle preserving macOS symlinks, permissions, and code signatures.
pub fn copy_bundle_preserving_metadata(src: &Path, dst: &Path) -> Result<()> {
    if dst.exists() {
        std::fs::remove_dir_all(dst)
            .with_context(|| format!("clean staging destination {}", dst.display()))?;
    }
    #[cfg(target_os = "macos")]
    {
        let status = Command::new("/usr/bin/ditto")
            .arg(src)
            .arg(dst)
            .status()
            .context("run ditto to stage app bundle")?;
        if !status.success() {
            bail!("ditto failed copying {} -> {}", src.display(), dst.display());
        }
        return Ok(());
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (src, dst);
        bail!("macOS app bundle staging is only supported on macOS");
    }
}

/// Atomically replace `dest_app` with `staged_app` (must reside on the same filesystem),
/// rolling back from a temporary backup if the rename fails.
pub fn swap_bundle_atomic(staged_app: &Path, dest_app: &Path) -> Result<()> {
    if !staged_app.exists() {
        bail!("staged bundle does not exist: {}", staged_app.display());
    }
    let parent = dest_app
        .parent()
        .ok_or_else(|| anyhow!("invalid destination path {}", dest_app.display()))?;
    let backup = parent.join(".Piya.app.backup");
    if backup.exists() {
        let _ = std::fs::remove_dir_all(&backup);
    }

    let had_existing = dest_app.exists();
    if had_existing {
        std::fs::rename(dest_app, &backup).with_context(|| {
            format!(
                "move existing {} to temporary backup {}",
                dest_app.display(),
                backup.display()
            )
        })?;
    }

    if let Err(err) = std::fs::rename(staged_app, dest_app) {
        if had_existing && backup.exists() {
            let _ = std::fs::rename(&backup, dest_app);
        }
        return Err(err).with_context(|| {
            format!(
                "promote staged bundle {} into {}",
                staged_app.display(),
                dest_app.display()
            )
        });
    }

    if backup.exists() {
        let _ = std::fs::remove_dir_all(&backup);
    }
    Ok(())
}

fn find_extracted_app(dir: &Path) -> Result<PathBuf> {
    for candidate in ["Piya.app", "PiyaSwift.app"] {
        let p = dir.join(candidate);
        if p.is_dir() {
            return Ok(p);
        }
    }
    for entry in std::fs::read_dir(dir).context("read extracted update archive")? {
        let entry = entry?;
        let path = entry.path();
        if path.is_dir()
            && path
                .extension()
                .and_then(|e| e.to_str())
                .map(|e| e.eq_ignore_ascii_case("app"))
                .unwrap_or(false)
        {
            return Ok(path);
        }
    }
    bail!("archive did not contain a .app bundle")
}

/// Download the release zip, verify SHA-256 (when published), extract, verify code signature,
/// and stage into `/Applications/.Piya-update.noindex/Piya.app` on the target filesystem.
pub fn download_and_stage_release(check: &UpdateCheck, dest_app: &Path) -> Result<PathBuf> {
    let asset_url = check
        .asset_url
        .clone()
        .ok_or_else(|| anyhow!("release v{} has no macOS .zip asset", check.latest_version))?;
    let expected_sha = check.expected_sha256.clone();
    let parent = dest_app
        .parent()
        .ok_or_else(|| anyhow!("invalid destination {}", dest_app.display()))?;
    let work_dir = parent.join(".Piya-update.noindex");
    if work_dir.exists() {
        std::fs::remove_dir_all(&work_dir)
            .with_context(|| format!("clean {}", work_dir.display()))?;
    }
    std::fs::create_dir_all(&work_dir)
        .with_context(|| format!("create {} (check /Applications permissions)", work_dir.display()))?;

    let zip_path = work_dir.join("Piya-update.zip");
    let zip_clone = zip_path.clone();
    crate::runtime::block_on(async move {
        let client = http_client(Duration::from_secs(300))?;
        let resp = client
            .get(&asset_url)
            .header(USER_AGENT, format!("piya/{}", current_version()))
            .send()
            .await
            .context("download release archive")?;
        if !resp.status().is_success() {
            bail!("download failed with HTTP {}", resp.status().as_u16());
        }
        let bytes = resp.bytes().await.context("read release archive body")?;
        std::fs::write(&zip_clone, &bytes)
            .with_context(|| format!("write {}", zip_clone.display()))?;
        Ok::<(), anyhow::Error>(())
    })?;

    if let Some(ref sha) = expected_sha {
        if let Err(err) = verify_sha256(&zip_path, sha) {
            let _ = std::fs::remove_dir_all(&work_dir);
            return Err(err);
        }
    }

    let extract_dir = work_dir.join("extracted");
    std::fs::create_dir_all(&extract_dir)?;
    let status = Command::new("/usr/bin/ditto")
        .args(["-x", "-k"])
        .arg(&zip_path)
        .arg(&extract_dir)
        .status()
        .context("extract update zip with ditto")?;
    if !status.success() {
        let _ = std::fs::remove_dir_all(&work_dir);
        bail!("failed to extract update archive");
    }

    let extracted_app = match find_extracted_app(&extract_dir) {
        Ok(p) => p,
        Err(e) => {
            let _ = std::fs::remove_dir_all(&work_dir);
            return Err(e);
        }
    };

    if let Err(e) = validate_app_bundle(&extracted_app) {
        let _ = std::fs::remove_dir_all(&work_dir);
        return Err(e);
    }

    let staged_app = work_dir.join("Piya.app");
    if extracted_app != staged_app {
        std::fs::rename(&extracted_app, &staged_app)?;
    }
    let _ = std::fs::remove_file(&zip_path);
    let _ = std::fs::remove_dir_all(&extract_dir);
    unregister_launch_services(&staged_app);
    Ok(staged_app)
}

/// Stage a local build bundle (e.g. `dist/build.noindex/PiyaSwift.app`) next to `dest_app`.
pub fn stage_local_bundle(source_app: &Path, dest_app: &Path) -> Result<PathBuf> {
    validate_app_bundle(source_app)?;
    let parent = dest_app
        .parent()
        .ok_or_else(|| anyhow!("invalid destination {}", dest_app.display()))?;
    let work_dir = parent.join(".Piya-update.noindex");
    if work_dir.exists() {
        std::fs::remove_dir_all(&work_dir)
            .with_context(|| format!("clean {}", work_dir.display()))?;
    }
    std::fs::create_dir_all(&work_dir)
        .with_context(|| format!("create {}", work_dir.display()))?;

    let staged_app = work_dir.join("Piya.app");
    copy_bundle_preserving_metadata(source_app, &staged_app)?;
    validate_app_bundle(&staged_app)?;
    unregister_launch_services(&staged_app);
    Ok(staged_app)
}

/// Wait for any `in_progress` tasks to complete before swapping binaries.
pub fn wait_for_active_tasks(settings: &Settings, timeout: Duration, force: bool) -> Result<()> {
    let db_path = settings.data_dir.join("piya.db");
    if !db_path.exists() {
        return Ok(());
    }
    let queue = TaskQueue::new(&db_path)?;
    let deadline = Instant::now() + timeout;
    loop {
        let _ = queue.reclaim_stale(90);
        let active = queue.count(Some(TaskStatus::InProgress)).unwrap_or(0);
        if active == 0 {
            return Ok(());
        }
        if force {
            return Ok(());
        }
        if Instant::now() >= deadline {
            bail!(
                "{active} background task(s) still running; wait for completion or pass --force"
            );
        }
        std::thread::sleep(Duration::from_secs(1));
    }
}

fn stop_idle_worker(data_dir: &Path) {
    let lock = worker::worker_lock_path(data_dir);
    if let Ok(pid_str) = std::fs::read_to_string(&lock) {
        if let Ok(pid) = pid_str.trim().parse::<u32>() {
            // AppKit/GCD masks SIGTERM across exec; send SIGINT first, then SIGKILL fallback.
            let _ = Command::new("kill")
                .args(["-INT", &pid.to_string()])
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status();
            std::thread::sleep(Duration::from_millis(200));
            let _ = Command::new("kill")
                .args(["-9", &pid.to_string()])
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status();
        }
        let _ = std::fs::remove_file(lock);
    }
    let _ = Command::new("pkill")
        .args(["-INT", "-f", "/Applications/Piya.app/Contents/Helpers/piya run"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
}

fn is_gui_running() -> bool {
    Command::new("pgrep")
        .args(["-f", "/Applications/Piya.app/Contents/MacOS/"])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

fn stop_gui_and_wait() {
    let _ = Command::new("pkill")
        .args(["-f", "/Applications/Piya.app/Contents/MacOS/"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
    let deadline = Instant::now() + Duration::from_secs(4);
    while is_gui_running() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(150));
    }
}

/// Apply an already-staged bundle from the CLI (external to the GUI process).
pub fn apply_staged_update_cli(
    settings: &Settings,
    staged_app: &Path,
    dest_app: &Path,
    force: bool,
) -> Result<()> {
    wait_for_active_tasks(settings, Duration::from_secs(45), force)?;
    stop_idle_worker(&settings.data_dir);

    let was_running = is_gui_running();
    if was_running {
        stop_gui_and_wait();
    }

    let work_dir = staged_app.parent().map(Path::to_path_buf);
    swap_bundle_atomic(staged_app, dest_app)?;
    if let Some(dir) = work_dir {
        if dir.ends_with(".Piya-update.noindex") {
            let _ = std::fs::remove_dir_all(dir);
        }
    }

    if was_running {
        Command::new("open")
            .arg("-a")
            .arg(dest_app)
            .status()
            .context("relaunch updated Piya.app")?;
    }
    Ok(())
}

/// Download, verify, stage, and schedule an atomic swap + relaunch from inside the running GUI.
///
/// When this returns `Ok(version)`, the caller should immediately terminate the GUI process
/// (`NSApplication.shared.terminate(nil)`) so the detached helper completes the swap and reopens Piya.
pub fn prepare_gui_self_update() -> Result<String> {
    let settings = Settings::load()?;
    let db_path = settings.data_dir.join("piya.db");
    if db_path.exists() {
        let queue = TaskQueue::new(&db_path)?;
        let _ = queue.reclaim_stale(90);
        let active = queue.count(Some(TaskStatus::InProgress)).unwrap_or(0);
        if active > 0 {
            bail!(
                "{active} task(s) currently running — please wait for them to finish before updating"
            );
        }
    }

    let check = check_for_update()?;
    if !check.update_available {
        bail!("Piya is already up to date ({})", check.current_version);
    }

    let dest_app = PathBuf::from(INSTALLED_APP_PATH);
    let staged_app = download_and_stage_release(&check, &dest_app)?;
    let work_dir = staged_app
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("/Applications/.Piya-update.noindex"));
    let backup_app = PathBuf::from("/Applications/.Piya.app.backup");

    stop_idle_worker(&settings.data_dir);

    let my_pid = std::process::id().to_string();
    let script = r#"
pid="$1"
staged="$2"
dest="$3"
backup="$4"
workdir="$5"

i=0
while kill -0 "$pid" 2>/dev/null && [ "$i" -lt 25 ]; do
  [ "$i" -eq 6 ] && kill "$pid" 2>/dev/null || true
  sleep 0.2
  i=$((i + 1))
done

rm -rf "$backup"
if [ -d "$dest" ]; then
  mv "$dest" "$backup" || exit 1
fi
if mv "$staged" "$dest"; then
  rm -rf "$backup" "$workdir"
  open -a "$dest"
else
  [ -d "$backup" ] && mv "$backup" "$dest"
  open -a "$dest"
  exit 1
fi
"#;

    let mut cmd = Command::new("/bin/sh");
    cmd.arg("-c")
        .arg(script)
        .arg("piya-self-updater")
        .arg(&my_pid)
        .arg(&staged_app)
        .arg(&dest_app)
        .arg(&backup_app)
        .arg(&work_dir)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());

    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        cmd.process_group(0);
    }

    cmd.spawn().context("spawn self-update swap helper")?;
    Ok(check.latest_version)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_tags_and_compares_versions() {
        assert!(is_newer_version("0.2.0", "v0.2.1").unwrap());
        assert!(is_newer_version("0.2.0", "0.3.0").unwrap());
        assert!(!is_newer_version("0.2.0", "v0.2.0").unwrap());
        assert!(!is_newer_version("0.2.1", "v0.2.0").unwrap());
    }

    #[test]
    fn parses_sha256sums_formats() {
        let hash = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
        let sums = format!(
            "# release checksums\n{hash}  Piya-macos-arm64.zip\nffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff *other.zip\n"
        );
        assert_eq!(
            parse_sha256sums(&sums, "Piya-macos-arm64.zip").as_deref(),
            Some(hash)
        );
        assert_eq!(parse_sha256sums(&sums, "missing.zip"), None);
    }

    #[test]
    fn verifies_sha256_and_rejects_tampered_file() {
        let dir = std::env::temp_dir().join(format!("piya-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("payload.zip");
        std::fs::write(&file, b"hello piya release").unwrap();
        let digest = sha256_file(&file).unwrap();
        verify_sha256(&file, &digest).unwrap();
        let bad = "0000000000000000000000000000000000000000000000000000000000000000";
        assert!(verify_sha256(&file, bad).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn atomic_swap_replaces_existing_bundle_and_cleans_backup() {
        let dir = std::env::temp_dir().join(format!("piya-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let dest = dir.join("Piya.app");
        let staged = dir.join("Piya.app.staged");
        std::fs::create_dir_all(&dest).unwrap();
        std::fs::write(dest.join("version.txt"), "0.2.0").unwrap();
        std::fs::create_dir_all(&staged).unwrap();
        std::fs::write(staged.join("version.txt"), "0.2.1").unwrap();

        swap_bundle_atomic(&staged, &dest).unwrap();

        assert_eq!(
            std::fs::read_to_string(dest.join("version.txt")).unwrap(),
            "0.2.1"
        );
        assert!(!staged.exists());
        assert!(!dir.join(".Piya.app.backup").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn selects_arch_appropriate_zip_asset() {
        let assets = vec![
            GithubAsset {
                name: "SHA256SUMS".into(),
                browser_download_url: "https://example.com/SHA256SUMS".into(),
                digest: None,
            },
            GithubAsset {
                name: "Piya-macos-arm64.zip".into(),
                browser_download_url: "https://example.com/arm64.zip".into(),
                digest: Some(
                    "sha256:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"
                        .into(),
                ),
            },
        ];
        let picked = select_macos_asset(&assets).unwrap();
        assert_eq!(picked.name, "Piya-macos-arm64.zip");
        assert!(parse_github_digest(picked.digest.as_deref()).is_some());
    }
}
