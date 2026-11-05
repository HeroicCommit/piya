use std::path::PathBuf;

use anyhow::{Context, Result};

#[derive(Clone, Debug)]
pub struct Settings {
    pub openai_api_key: String,
    pub openai_base_url: String,
    pub model: String,
    pub embedding_model: String,
    pub workspace: PathBuf,
    pub data_dir: PathBuf,
    pub poll_interval_sec: u64,
    pub max_cycles: u64,
    pub min_accuracy: f64,
    pub max_retries: u32,
    /// Parallel task workers for `piya run` (`PIYA_WORKERS`, default 3, max 8).
    pub workers: usize,
}

impl Settings {
    pub fn load() -> Result<Self> {
        load_env_files();

        let settings = Self {
            openai_api_key: std::env::var("OPENAI_API_KEY").unwrap_or_default(),
            openai_base_url: std::env::var("OPENAI_BASE_URL")
                .unwrap_or_else(|_| "https://api.openai.com/v1".into()),
            model: std::env::var("PIYA_MODEL").unwrap_or_else(|_| "gpt-4o-mini".into()),
            embedding_model: std::env::var("PIYA_EMBEDDING_MODEL")
                .unwrap_or_else(|_| "text-embedding-3-small".into()),
            workspace: path_from_env("PIYA_WORKSPACE", "./workspace"),
            data_dir: path_from_env("PIYA_DATA_DIR", "./data"),
            poll_interval_sec: parse_env("PIYA_POLL_INTERVAL_SEC", 30)?,
            max_cycles: parse_env("PIYA_MAX_CYCLES", 0)?,
            min_accuracy: parse_env("PIYA_MIN_ACCURACY", 0.7)?,
            max_retries: parse_env("PIYA_MAX_RETRIES", 3)?,
            workers: parse_env::<usize>("PIYA_WORKERS", 3)?.clamp(1, 8),
        };

        settings.ensure_dirs()?;
        Ok(settings)
    }

    pub fn ensure_dirs(&self) -> Result<()> {
        std::fs::create_dir_all(&self.workspace)
            .with_context(|| format!("create workspace {}", self.workspace.display()))?;
        std::fs::create_dir_all(&self.data_dir)
            .with_context(|| format!("create data dir {}", self.data_dir.display()))?;
        Ok(())
    }

    pub fn require_api_key(&self) -> Result<()> {
        if self.openai_api_key.is_empty() {
            anyhow::bail!("OPENAI_API_KEY is required. Copy .env.example to .env and set your key.");
        }
        Ok(())
    }
}

fn path_from_env(key: &str, default: &str) -> PathBuf {
    std::env::var(key)
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from(default))
}

fn parse_env<T: std::str::FromStr>(key: &str, default: T) -> Result<T>
where
    T::Err: std::error::Error + Send + Sync + 'static,
{
    match std::env::var(key) {
        Ok(v) => v.parse().map_err(Into::into),
        Err(_) => Ok(default),
    }
}

fn load_env_files() {
    let _ = dotenvy::dotenv();
    if let Some(path) = app_support_env_file() {
        let _ = dotenvy::from_filename(path);
    }
}

pub fn app_support_dir() -> Option<PathBuf> {
    std::env::var("HOME")
        .ok()
        .map(|h| PathBuf::from(h).join("Library/Application Support/Piya"))
}

fn app_support_env_file() -> Option<PathBuf> {
    app_support_dir().map(|d| d.join(".env")).filter(|p| p.is_file())
}

pub fn write_app_support_config(settings: &Settings) -> Result<PathBuf> {
    let dir = app_support_dir().context("app support dir")?;
    std::fs::create_dir_all(&dir)?;

    let data_dir = absolutize(&settings.data_dir);
    let workspace = absolutize(&settings.workspace);

    let mut lines = vec![
        format!("PIYA_DATA_DIR={}", data_dir.display()),
        format!("PIYA_WORKSPACE={}", workspace.display()),
        "PIYA_NOTIFY=all".into(),
    ];

    if let Ok(key) = std::env::var("OPENAI_API_KEY") {
        if !key.is_empty() {
            lines.push(format!("OPENAI_API_KEY={key}"));
        }
    }
    if let Ok(v) = std::env::var("OPENAI_BASE_URL") {
        if !v.is_empty() {
            lines.push(format!("OPENAI_BASE_URL={v}"));
        }
    }
    if let Ok(v) = std::env::var("PIYA_MODEL") {
        if !v.is_empty() {
            lines.push(format!("PIYA_MODEL={v}"));
        }
    }

    let path = dir.join(".env");
    std::fs::write(&path, lines.join("\n") + "\n")?;
    Ok(path)
}

fn absolutize(path: &PathBuf) -> PathBuf {
    path.canonicalize().unwrap_or_else(|_| path.clone())
}

/// Keys editable from the Swift Settings UI (App Support `.env`).
#[derive(Clone, Debug, Default)]
pub struct AppEnvSettings {
    pub openai_api_key: String,
    pub openai_base_url: String,
    pub model: String,
    pub notify: String,
    pub shell_policy: String,
}

impl AppEnvSettings {
    pub fn load_from_env() -> Self {
        Self {
            openai_api_key: std::env::var("OPENAI_API_KEY").unwrap_or_default(),
            openai_base_url: std::env::var("OPENAI_BASE_URL")
                .unwrap_or_else(|_| "https://api.openai.com/v1".into()),
            model: std::env::var("PIYA_MODEL").unwrap_or_else(|_| "gpt-4o-mini".into()),
            notify: std::env::var("PIYA_NOTIFY").unwrap_or_else(|_| "all".into()),
            shell_policy: std::env::var("PIYA_SHELL_POLICY").unwrap_or_else(|_| "ask".into()),
        }
    }

    /// Upsert selected keys into App Support `.env` and refresh process env.
    pub fn save_to_app_support(&self) -> Result<PathBuf> {
        let dir = app_support_dir().context("app support dir")?;
        std::fs::create_dir_all(&dir)?;
        let path = dir.join(".env");

        let mut map = read_env_file_map(&path);
        upsert(&mut map, "OPENAI_API_KEY", &self.openai_api_key);
        upsert(&mut map, "OPENAI_BASE_URL", &self.openai_base_url);
        upsert(&mut map, "PIYA_MODEL", &self.model);
        upsert(&mut map, "PIYA_NOTIFY", &self.notify);
        upsert(&mut map, "PIYA_SHELL_POLICY", &self.shell_policy);

        // Preserve data/workspace if already present; otherwise seed from process env.
        if !map.contains_key("PIYA_DATA_DIR") {
            if let Ok(v) = std::env::var("PIYA_DATA_DIR") {
                if !v.is_empty() {
                    map.insert("PIYA_DATA_DIR".into(), v);
                }
            }
        }
        if !map.contains_key("PIYA_WORKSPACE") {
            if let Ok(v) = std::env::var("PIYA_WORKSPACE") {
                if !v.is_empty() {
                    map.insert("PIYA_WORKSPACE".into(), v);
                }
            }
        }

        write_env_file_map(&path, &map)?;

        // Apply immediately for this process (Swift app stays running).
        set_proc_env("OPENAI_API_KEY", &self.openai_api_key);
        set_proc_env("OPENAI_BASE_URL", &self.openai_base_url);
        set_proc_env("PIYA_MODEL", &self.model);
        set_proc_env("PIYA_NOTIFY", &self.notify);
        set_proc_env("PIYA_SHELL_POLICY", &self.shell_policy);

        Ok(path)
    }
}

fn upsert(map: &mut std::collections::BTreeMap<String, String>, key: &str, value: &str) {
    map.insert(key.to_string(), value.trim().to_string());
}

fn set_proc_env(key: &str, value: &str) {
    // SAFETY: settings save from UniFFI; process-local env refresh for live app.
    unsafe { std::env::set_var(key, value.trim()) };
}

fn read_env_file_map(path: &PathBuf) -> std::collections::BTreeMap<String, String> {
    let mut map = std::collections::BTreeMap::new();
    let Ok(raw) = std::fs::read_to_string(path) else {
        return map;
    };
    for line in raw.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some((k, v)) = line.split_once('=') {
            map.insert(k.trim().to_string(), v.to_string());
        }
    }
    map
}

fn write_env_file_map(
    path: &PathBuf,
    map: &std::collections::BTreeMap<String, String>,
) -> Result<()> {
    let mut lines = Vec::new();
    for (k, v) in map {
        lines.push(format!("{k}={v}"));
    }
    std::fs::write(path, lines.join("\n") + "\n")
        .with_context(|| format!("write {}", path.display()))?;
    Ok(())
}
