//! Media and document attachments for Chat (`data/attachments/<session>/`).
//!
//! Supported inputs:
//! - Images (`png`, `jpg`, `jpeg`, `webp`, `gif`) → sent as vision blocks to capable models
//! - PDFs (`pdf`) → extracted text (via Swift PDFKit or UTF-8 fallback) + workspace copy
//! - Audio (`m4a`, `mp3`, `wav`, `aac`, `caf`, `ogg`) → Apple Speech transcript + workspace copy
//! - Text / CSV / JSON / code (`document`) → UTF-8 text excerpt + workspace copy

use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use base64::Engine;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::config::Settings;
use crate::llm::ImageInput;

const MAX_FILE_BYTES: u64 = 50 * 1024 * 1024;
const MAX_IMAGE_BYTES: u64 = 20 * 1024 * 1024;
/// Stored in SQLite `attachments.text` (for FTS5 search and prompt excerpts).
const MAX_STORED_TEXT_CHARS: usize = 60_000;
/// Included inline in a single chat prompt; full file is always in `workspace/attachments/`.
const MAX_PROMPT_TEXT_CHARS: usize = 16_000;

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChatAttachment {
    pub id: String,
    pub name: String,
    /// Relative to `data_dir` when staged inside `data/`, or absolute.
    pub path: String,
    #[serde(default)]
    pub mime: String,
    /// `image` | `pdf` | `audio` | `document` | `other`
    #[serde(default)]
    pub kind: String,
    /// Extracted PDF text, audio transcript, or UTF-8 document text.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
}

impl ChatAttachment {
    pub fn resolve_path(&self, data_dir: &Path) -> PathBuf {
        let p = Path::new(&self.path);
        if p.is_absolute() {
            p.to_path_buf()
        } else {
            data_dir.join(p)
        }
    }

    pub fn workspace_rel_name(&self) -> String {
        let short = &self.id[..self.id.len().min(8)];
        let safe = sanitize_filename(&self.name);
        if short.is_empty() {
            format!("attachments/{safe}")
        } else {
            format!("attachments/{short}_{safe}")
        }
    }
}

/// Copy `source_path` into `data/attachments/<session_id>/` and `workspace/attachments/`,
/// classify its media type, and capture extracted text / transcript.
pub fn stage_attachment(
    settings: &Settings,
    session_id: &str,
    source_path: &Path,
    display_name: Option<&str>,
    extracted_text: Option<&str>,
) -> Result<ChatAttachment> {
    let meta = std::fs::metadata(source_path)
        .with_context(|| format!("cannot read attachment {}", source_path.display()))?;
    if !meta.is_file() {
        bail!("{} is not a regular file", source_path.display());
    }
    if meta.len() > MAX_FILE_BYTES {
        bail!(
            "attachment is {:.1} MB (max 50 MB)",
            meta.len() as f64 / (1024.0 * 1024.0)
        );
    }

    let raw_name = display_name
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .or_else(|| source_path.file_name().and_then(|n| n.to_str()))
        .unwrap_or("attachment");
    let safe_name = sanitize_filename(raw_name);
    let (kind, mime) = classify_filename(&safe_name);

    if kind == "image" && meta.len() > MAX_IMAGE_BYTES {
        bail!(
            "image `{safe_name}` is {:.1} MB (max 20 MB)",
            meta.len() as f64 / (1024.0 * 1024.0)
        );
    }

    let id = Uuid::new_v4().to_string();
    let short = &id[..8];
    let session_bucket = if session_id.trim().is_empty() {
        "unsorted".to_string()
    } else {
        sanitize_filename(session_id)
    };

    let rel_dir = PathBuf::from("attachments").join(session_bucket);
    let abs_dir = settings.data_dir.join(&rel_dir);
    std::fs::create_dir_all(&abs_dir)
        .with_context(|| format!("create {}", abs_dir.display()))?;

    let stored_file = format!("{short}_{safe_name}");
    let rel_path = rel_dir.join(&stored_file);
    let abs_path = settings.data_dir.join(&rel_path);
    std::fs::copy(source_path, &abs_path)
        .with_context(|| format!("copy attachment to {}", abs_path.display()))?;

    // Mirror into workspace/attachments/ so shell tools and background tasks can inspect it.
    let ws_dir = settings.workspace.join("attachments");
    if std::fs::create_dir_all(&ws_dir).is_ok() {
        let _ = std::fs::copy(source_path, ws_dir.join(&stored_file));
    }

    let provided = extracted_text
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| clip_chars(s, MAX_STORED_TEXT_CHARS));

    let text = match provided {
        Some(t) => Some(t),
        None if kind == "document" => read_utf8_excerpt(&abs_path, MAX_STORED_TEXT_CHARS),
        None => None,
    };

    Ok(ChatAttachment {
        id,
        name: raw_name.to_string(),
        path: rel_path.to_string_lossy().into_owned(),
        mime,
        kind,
        text,
    })
}

/// Load an image attachment as `(media_type, base64)` for vision models.
pub fn load_image_input(att: &ChatAttachment, data_dir: &Path) -> Result<ImageInput> {
    let full = att.resolve_path(data_dir);
    let bytes = std::fs::read(&full)
        .with_context(|| format!("read image attachment {}", full.display()))?;
    let media_type = if att.mime.starts_with("image/") {
        att.mime.clone()
    } else {
        let (_, m) = classify_filename(&att.name);
        if m.starts_with("image/") {
            m
        } else {
            "image/png".to_string()
        }
    };
    let base64_data = base64::engine::general_purpose::STANDARD.encode(&bytes);
    Ok(ImageInput {
        media_type,
        base64_data,
    })
}

/// Build the textual context block for one attachment so the model sees transcripts,
/// extracted PDF/document text, and the exact path inside `workspace/` for tool access.
pub fn format_for_prompt(att: &ChatAttachment, workspace: &Path) -> String {
    let ws_path = workspace.join(att.workspace_rel_name());
    let ws_hint = if ws_path.is_file() {
        format!("workspace file: `{}`", ws_path.display())
    } else {
        format!("name: `{}`", att.name)
    };

    match att.kind.as_str() {
        "audio" => match att.text.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
            Some(transcript) => format!(
                "[Attached audio `{}` ({ws_hint}) — spoken transcript]:\n{}",
                att.name,
                clip_for_prompt(transcript)
            ),
            None => format!(
                "[Attached audio `{}` ({ws_hint}) — no speech transcript was detected]",
                att.name
            ),
        },
        "pdf" | "document" => match att.text.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
            Some(body) => format!(
                "[Attached {} `{}` ({ws_hint}) — extracted text]:\n{}",
                att.kind,
                att.name,
                clip_for_prompt(body)
            ),
            None => format!(
                "[Attached {} `{}` ({ws_hint}) — no selectable text extracted; inspect the file with tools if needed]",
                att.kind, att.name
            ),
        },
        "image" => format!("[Attached image `{}` ({ws_hint})]", att.name),
        _ => format!(
            "[Attached file `{}` ({}, {ws_hint})]",
            att.name,
            if att.mime.is_empty() { "binary" } else { &att.mime }
        ),
    }
}

fn clip_for_prompt(s: &str) -> String {
    let count = s.chars().count();
    if count <= MAX_PROMPT_TEXT_CHARS {
        s.to_string()
    } else {
        let head: String = s.chars().take(MAX_PROMPT_TEXT_CHARS).collect();
        format!(
            "{head}\n…[truncated at {MAX_PROMPT_TEXT_CHARS} of {count} chars — read the full workspace file with `piya_run_command` if needed]"
        )
    }
}

fn clip_chars(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        s.chars().take(max).collect()
    }
}

fn read_utf8_excerpt(path: &Path, max_chars: usize) -> Option<String> {
    let bytes = std::fs::read(path).ok()?;
    // Reject obvious binary files (NUL byte in the first 4 KB).
    if bytes.iter().take(4096).any(|&b| b == 0) {
        return None;
    }
    let s = String::from_utf8_lossy(&bytes);
    let trimmed = s.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(clip_chars(trimmed, max_chars))
    }
}

pub fn classify_filename(name: &str) -> (String, String) {
    let ext = Path::new(name)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    match ext.as_str() {
        "png" => ("image".into(), "image/png".into()),
        "jpg" | "jpeg" => ("image".into(), "image/jpeg".into()),
        "webp" => ("image".into(), "image/webp".into()),
        "gif" => ("image".into(), "image/gif".into()),
        "pdf" => ("pdf".into(), "application/pdf".into()),
        "m4a" => ("audio".into(), "audio/mp4".into()),
        "mp3" => ("audio".into(), "audio/mpeg".into()),
        "wav" => ("audio".into(), "audio/wav".into()),
        "aac" => ("audio".into(), "audio/aac".into()),
        "caf" => ("audio".into(), "audio/x-caf".into()),
        "ogg" => ("audio".into(), "audio/ogg".into()),
        "csv" => ("document".into(), "text/csv".into()),
        "tsv" => ("document".into(), "text/tab-separated-values".into()),
        "json" | "jsonl" => ("document".into(), "application/json".into()),
        "md" | "markdown" => ("document".into(), "text/markdown".into()),
        "html" | "htm" => ("document".into(), "text/html".into()),
        "xml" => ("document".into(), "application/xml".into()),
        "txt" | "log" | "yaml" | "yml" | "toml" | "ini" | "env" | "rs" | "py" | "js" | "ts"
        | "tsx" | "jsx" | "swift" | "sh" | "zsh" | "sql" | "go" | "java" | "c" | "cpp" | "h"
        | "css" => ("document".into(), "text/plain".into()),
        _ => ("other".into(), "application/octet-stream".into()),
    }
}

pub fn sanitize_filename(raw: &str) -> String {
    let cleaned: String = raw
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_') {
                c
            } else {
                '_'
            }
        })
        .collect();
    let trimmed = cleaned.trim_matches(|c| c == '.' || c == '_');
    if trimmed.is_empty() {
        "attachment".into()
    } else {
        trimmed.chars().take(100).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_settings(root: &Path) -> Settings {
        Settings {
            provider: "openai".into(),
            api_key: "test".into(),
            base_url: "https://api.openai.com/v1".into(),
            model: "gpt-4o-mini".into(),
            embed_provider: "none".into(),
            embed_api_key: String::new(),
            embed_base_url: String::new(),
            embedding_model: String::new(),
            workspace: root.join("workspace"),
            data_dir: root.join("data"),
            poll_interval_sec: 30,
            max_cycles: 0,
            min_accuracy: 0.7,
            max_retries: 3,
            workers: 1,
        }
    }

    #[test]
    fn stages_document_and_extracts_utf8_text() {
        let dir = std::env::temp_dir().join(format!("piya-att-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let src = dir.join("sales report.csv");
        std::fs::write(&src, "month,revenue\nJan,4200\n").unwrap();
        let settings = test_settings(&dir);

        let att = stage_attachment(&settings, "sess-1", &src, None, None).unwrap();
        assert_eq!(att.kind, "document");
        assert_eq!(att.mime, "text/csv");
        assert!(att.text.as_deref().unwrap().contains("Jan,4200"));
        assert!(att.resolve_path(&settings.data_dir).is_file());
        assert!(settings.workspace.join(att.workspace_rel_name()).is_file());

        let prompt = format_for_prompt(&att, &settings.workspace);
        assert!(prompt.contains("Jan,4200") && prompt.contains("workspace file:"));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn stages_image_and_encodes_base64() {
        let dir = std::env::temp_dir().join(format!("piya-img-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let src = dir.join("shot.png");
        let png_stub = b"\x89PNG\r\n\x1a\nfake";
        std::fs::write(&src, png_stub).unwrap();
        let settings = test_settings(&dir);

        let att = stage_attachment(&settings, "sess-1", &src, None, None).unwrap();
        assert_eq!(att.kind, "image");
        assert!(att.text.is_none());
        let img = load_image_input(&att, &settings.data_dir).unwrap();
        assert_eq!(img.media_type, "image/png");
        let decoded = base64::engine::general_purpose::STANDARD
            .decode(&img.base64_data)
            .unwrap();
        assert_eq!(decoded, png_stub);
        let _ = std::fs::remove_dir_all(dir);
    }
}
