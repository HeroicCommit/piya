use std::path::Path;
use std::process::Command;

use anyhow::{Context, Result};

pub fn prompt_text(title: &str, message: &str) -> Option<String> {
    let script = format!(
        r#"set r to display dialog {} with title {} default answer "" buttons {{"Cancel", "Assign"}} default button "Assign"
if button returned of r is "Assign" then return text returned of r"#,
        apple_string(message),
        apple_string(title),
    );
    run_osascript(&script).ok().filter(|s| !s.is_empty())
}

pub fn choose_yes_no(title: &str, message: &str) -> bool {
    let script = format!(
        r#"set r to display dialog {} with title {} buttons {{"Deny", "Allow"}} default button "Deny" cancel button "Deny"
return button returned of r"#,
        apple_string(message),
        apple_string(title),
    );
    run_osascript(&script)
        .map(|s| s.trim() == "Allow")
        .unwrap_or(false)
}

pub fn alert(title: &str, message: &str) {
    let script = format!(
        r#"display alert {} message {} as informational"#,
        apple_string(title),
        apple_string(message),
    );
    let _ = run_osascript(&script);
}

pub fn open_path(path: &Path) -> Result<()> {
    Command::new("open").arg(path).status().context("open path")?;
    Ok(())
}

fn run_osascript(script: &str) -> Result<String> {
    let output = Command::new("osascript")
        .arg("-e")
        .arg(script)
        .output()
        .context("osascript")?;
    if !output.status.success() {
        anyhow::bail!("dialog cancelled");
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

fn apple_string(s: &str) -> String {
    let escaped = s
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\r', "")
        .replace('\n', "\" & return & \"");
    format!("\"{escaped}\"")
}
