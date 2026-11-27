use std::path::PathBuf;

use anyhow::{Context, Result};

use crate::providers::{self, Connection};

#[derive(Clone, Debug)]
pub struct Settings {
    /// Active chat provider id (see `providers::PROVIDERS`), e.g. `openai`, `anthropic`, `ollama`.
    pub provider: String,
    pub api_key: String,
    pub base_url: String,
    pub model: String,
    /// Provider used for memory embeddings; empty or `none` disables memory search.
    pub embed_provider: String,
    pub embed_api_key: String,
    pub embed_base_url: String,
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

        let models = ModelSettings::load_from_env();
        let settings = Self {
            provider: models.provider.clone(),
            api_key: models.api_key.clone(),
            base_url: models.base_url.clone(),
            model: models.model.clone(),
            embed_provider: models.embed_provider.clone(),
            embed_api_key: models.embed_api_key.clone(),
            embed_base_url: models.embed_base_url.clone(),
            embedding_model: models.embedding_model.clone(),
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

    pub fn chat_connection(&self) -> Connection {
        Connection {
            provider: self.provider.clone(),
            api_key: self.api_key.clone(),
            base_url: self.base_url.clone(),
        }
    }

    /// `None` when memory embeddings are turned off.
    pub fn embed_connection(&self) -> Option<Connection> {
        let id = self.embed_provider.trim();
        if id.is_empty() || id == "none" {
            return None;
        }
        Some(Connection {
            provider: id.to_string(),
            api_key: self.embed_api_key.clone(),
            base_url: self.embed_base_url.clone(),
        })
    }

    /// The chat connection is usable: known provider, key when required, a model chosen.
    pub fn require_model(&self) -> Result<()> {
        self.chat_connection().validate()?;
        if self.model.trim().is_empty() {
            let label = providers::provider(&self.provider).map(|p| p.label).unwrap_or("this provider");
            anyhow::bail!("Choose a model for {label} in Settings → Models");
        }
        Ok(())
    }

    /// Env vars that hand the resolved model settings to a child process (worker).
    pub fn model_env(&self) -> Vec<(String, String)> {
        let mut env = vec![
            ("PIYA_PROVIDER".to_string(), self.provider.clone()),
            (providers::key_env(&self.provider), self.api_key.clone()),
            (providers::base_url_env(&self.provider), self.base_url.clone()),
            ("PIYA_MODEL".to_string(), self.model.clone()),
            ("PIYA_EMBED_PROVIDER".to_string(), self.embed_provider.clone()),
            ("PIYA_EMBEDDING_MODEL".to_string(), self.embedding_model.clone()),
        ];
        if let Some(e) = self.embed_connection() {
            env.push((providers::key_env(&e.provider), e.api_key));
            env.push((providers::base_url_env(&e.provider), e.base_url));
        }
        env
    }
}

fn path_from_env(key: &str, default: &str) -> PathBuf {
    let raw = std::env::var(key)
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from(default));

    // macOS `.app` bundles launched via LaunchServices / `open -a` run with `cwd = /`.
    // If an inherited env var (e.g. from `./.env` during `piya update --local`) is relative
    // like `./data`, resolving against `/` fails with EROFS (`/data`). Prefer the absolute
    // path saved in `~/Library/Application Support/Piya/.env`, else anchor in App Support.
    let cwd_is_root = std::env::current_dir()
        .map(|d| d == std::path::Path::new("/"))
        .unwrap_or(true);
    if raw.is_relative() && cwd_is_root {
        if let Some(env_path) = app_support_env_file() {
            let saved = read_env_file_map(&env_path);
            if let Some(val) = saved.get(key).map(|s| PathBuf::from(s.trim())) {
                if val.is_absolute() {
                    set_proc_env(key, &val.to_string_lossy());
                    return val;
                }
            }
        }
        if let Some(support) = app_support_dir() {
            let clean = raw.strip_prefix("./").unwrap_or(&raw);
            return support.join(clean);
        }
    }
    raw
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

    for (k, v) in settings.model_env() {
        if !v.trim().is_empty() {
            lines.push(format!("{k}={v}"));
        }
    }

    let path = dir.join(".env");
    std::fs::write(&path, lines.join("\n") + "\n")?;
    restrict_permissions(&path);
    Ok(path)
}

fn absolutize(path: &PathBuf) -> PathBuf {
    path.canonicalize().unwrap_or_else(|_| path.clone())
}

/// Notification + shell keys editable from the Swift Settings UI (App Support `.env`).
#[derive(Clone, Debug, Default)]
pub struct AppEnvSettings {
    pub notify: String,
    pub shell_policy: String,
}

impl AppEnvSettings {
    pub fn load_from_env() -> Self {
        Self {
            notify: std::env::var("PIYA_NOTIFY").unwrap_or_else(|_| "all".into()),
            shell_policy: std::env::var("PIYA_SHELL_POLICY").unwrap_or_else(|_| "ask".into()),
        }
    }

    /// Upsert notify/shell keys into App Support `.env` and refresh process env.
    pub fn save_to_app_support(&self) -> Result<PathBuf> {
        edit_app_support_env(|map| {
            upsert(map, "PIYA_NOTIFY", &self.notify);
            upsert(map, "PIYA_SHELL_POLICY", &self.shell_policy);
        })
    }
}

/// The active model connection plus the memory (embeddings) connection.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ModelSettings {
    pub provider: String,
    pub api_key: String,
    pub base_url: String,
    pub model: String,
    pub embed_provider: String,
    pub embed_api_key: String,
    pub embed_base_url: String,
    pub embedding_model: String,
}

impl ModelSettings {
    /// Resolve from env. Existing installs (only `OPENAI_API_KEY`) keep working as OpenAI.
    pub fn load_from_env() -> Self {
        let env = |k: &str| {
            std::env::var(k)
                .ok()
                .map(|v| v.trim().to_string())
                .filter(|v| !v.is_empty())
        };
        let provider = env("PIYA_PROVIDER")
            .filter(|p| providers::provider(p).is_some())
            .unwrap_or_else(|| "openai".into());
        let info = providers::provider(&provider).expect("known provider");
        let model = env("PIYA_MODEL").unwrap_or_else(|| info.suggested_model.to_string());

        let embed_provider = env("PIYA_EMBED_PROVIDER")
            .filter(|p| p == "none" || providers::provider(p).is_some())
            .unwrap_or_else(|| default_embed_provider(&provider));
        let (embed_api_key, embed_base_url, default_embed_model) = match providers::provider(&embed_provider) {
            Some(e) => (
                providers::saved_key(e.id),
                providers::saved_base_url(e.id),
                e.default_embedding_model.to_string(),
            ),
            None => (String::new(), String::new(), String::new()),
        };
        let embedding_model = env("PIYA_EMBEDDING_MODEL").unwrap_or(default_embed_model);

        Self {
            api_key: providers::saved_key(&provider),
            base_url: providers::saved_base_url(&provider),
            provider,
            model,
            embed_provider,
            embed_api_key,
            embed_base_url,
            embedding_model,
        }
    }

    /// Persist to App Support `.env` (keys stored per provider) and refresh process env.
    pub fn save_to_app_support(&self) -> Result<PathBuf> {
        let chat = providers::provider_or_err(&self.provider)?;
        let embed = match self.embed_provider.trim() {
            "" | "none" => None,
            id => Some(providers::provider_or_err(id)?),
        };
        edit_app_support_env(|map| {
            upsert(map, "PIYA_PROVIDER", chat.id);
            upsert(map, "PIYA_MODEL", &self.model);
            save_connection(map, chat.id, &self.api_key, &self.base_url);
            match embed {
                Some(e) => {
                    upsert(map, "PIYA_EMBED_PROVIDER", e.id);
                    upsert(map, "PIYA_EMBEDDING_MODEL", &self.embedding_model);
                    save_connection(map, e.id, &self.embed_api_key, &self.embed_base_url);
                }
                None => {
                    upsert(map, "PIYA_EMBED_PROVIDER", "none");
                    remove(map, "PIYA_EMBEDDING_MODEL");
                }
            }
        })
    }
}

/// Memory needs an embeddings API. Prefer the chat provider when it has one,
/// otherwise OpenAI when its key is saved, otherwise memory search is off.
fn default_embed_provider(chat_provider: &str) -> String {
    if providers::provider(chat_provider).is_some_and(|p| p.embeddings && chat_provider != "custom") {
        return chat_provider.to_string();
    }
    if !providers::saved_key("openai").is_empty() {
        return "openai".into();
    }
    "none".into()
}

fn save_connection(map: &mut EnvMap, id: &str, api_key: &str, base_url: &str) {
    upsert(map, &providers::key_env(id), api_key);
    let default = providers::provider(id).map(|p| p.default_base_url).unwrap_or("");
    let base = base_url.trim();
    if base.is_empty() || base.trim_end_matches('/') == default.trim_end_matches('/') {
        remove(map, &providers::base_url_env(id));
    } else {
        upsert(map, &providers::base_url_env(id), base);
    }
    if id == "openai" {
        // Migrate legacy keys so a cleared key cannot resurrect from OPENAI_API_KEY.
        remove(map, "OPENAI_API_KEY");
        remove(map, "OPENAI_BASE_URL");
    }
}

type EnvMap = std::collections::BTreeMap<String, Option<String>>;

/// Read App Support `.env`, apply `edit`, write it back (0600), and mirror changes into
/// this process's env so the running app picks them up immediately.
fn edit_app_support_env(edit: impl FnOnce(&mut EnvMap)) -> Result<PathBuf> {
    let dir = app_support_dir().context("app support dir")?;
    std::fs::create_dir_all(&dir)?;
    let path = dir.join(".env");

    let mut map: EnvMap = read_env_file_map(&path)
        .into_iter()
        .map(|(k, v)| (k, Some(v)))
        .collect();
    let before = map.clone();
    edit(&mut map);

    // Preserve data/workspace if already present; otherwise seed from process env.
    for key in ["PIYA_DATA_DIR", "PIYA_WORKSPACE"] {
        if !map.contains_key(key) {
            if let Ok(v) = std::env::var(key) {
                if !v.is_empty() {
                    map.insert(key.into(), Some(v));
                }
            }
        }
    }

    let kept: std::collections::BTreeMap<String, String> = map
        .iter()
        .filter_map(|(k, v)| v.clone().map(|v| (k.clone(), v)))
        .collect();
    write_env_file_map(&path, &kept)?;

    for (k, v) in &map {
        if before.get(k) == Some(v) {
            continue;
        }
        match v {
            Some(v) => set_proc_env(k, v),
            // SAFETY: settings save from UniFFI; process-local env refresh for live app.
            None => unsafe { std::env::remove_var(k) },
        }
    }
    Ok(path)
}

fn remove(map: &mut EnvMap, key: &str) {
    map.insert(key.to_string(), None);
}

fn upsert(map: &mut EnvMap, key: &str, value: &str) {
    map.insert(key.to_string(), Some(value.trim().to_string()));
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
    restrict_permissions(path);
    Ok(())
}

/// API keys live in this file: owner read/write only.
fn restrict_permissions(path: &std::path::Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
    }
}
