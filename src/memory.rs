//! Vector memory in SQLite (`memories` table in `piya.db`).
//!
//! Embeddings are stored as little-endian f32 blobs and ranked by brute-force cosine
//! similarity, which takes milliseconds for thousands of notes. Each row records the
//! embeddings model that produced it; when the model changes, rows are re-embedded.
//! Older installs kept memory in LanceDB (`memory.lance`); it is imported once.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result};
use chrono::Utc;
use rusqlite::{params, Connection};
use tokio::sync::OnceCell;
use uuid::Uuid;

use crate::db;
use crate::llm::LlmClient;

/// Embeddings model that built memory before the model was configurable.
const LEGACY_EMBEDDING_ID: &str = "openai:text-embedding-3-small";
const LANCE_IMPORTED_KEY: &str = "memory_lance_imported";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MemoryKind {
    Learning,
    Research,
    Outcome,
    Correction,
    Pattern,
}

impl MemoryKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Learning => "learning",
            Self::Research => "research",
            Self::Outcome => "outcome",
            Self::Correction => "correction",
            Self::Pattern => "pattern",
        }
    }

    pub fn parse(s: &str) -> Self {
        match s {
            "research" => Self::Research,
            "outcome" => Self::Outcome,
            "correction" => Self::Correction,
            "pattern" => Self::Pattern,
            _ => Self::Learning,
        }
    }
}

#[derive(Clone, Debug)]
pub struct MemoryHit {
    pub id: String,
    pub text: String,
    pub kind: MemoryKind,
    pub score: f64,
    pub accuracy: f64,
    pub source: String,
}

/// Options for vector recall. Chat/search should use [`RecallOpts::chat`] /
/// [`RecallOpts::search`] so weak OPPAR noise is filtered out.
#[derive(Clone, Debug)]
pub struct RecallOpts {
    pub limit: usize,
    pub kind: Option<MemoryKind>,
    pub min_accuracy: f64,
    /// Drop hits below this weighted score after boosts (0.0 = no floor).
    pub min_score: f64,
    /// Boost memories saved by the user (`source=user` / tags containing `user`).
    pub prefer_user: bool,
    /// Skip OPPAR task-outcome digests (noisy for conversational Q&A).
    pub exclude_outcomes: bool,
    /// Kept for API compatibility. Writes to the LanceDB column were removed
    /// because ranking never used the counter.
    #[allow(dead_code)]
    pub bump_recall_count: bool,
}

impl Default for RecallOpts {
    fn default() -> Self {
        Self {
            limit: 5,
            kind: None,
            min_accuracy: 0.0,
            min_score: 0.0,
            prefer_user: false,
            exclude_outcomes: false,
            bump_recall_count: false,
        }
    }
}

impl RecallOpts {
    /// Chat auto-context: prefer real user facts, drop weak/noisy hits.
    pub fn chat(limit: usize) -> Self {
        Self {
            limit,
            min_score: 0.62,
            prefer_user: true,
            exclude_outcomes: true,
            bump_recall_count: false,
            ..Default::default()
        }
    }

    /// Explicit recall / MCP search.
    pub fn search(limit: usize) -> Self {
        Self {
            limit,
            min_score: 0.55,
            prefer_user: true,
            exclude_outcomes: true,
            bump_recall_count: false,
            ..Default::default()
        }
    }

    /// Agent Observe — keep outcomes, lower floor so past work still surfaces.
    pub fn observe(limit: usize) -> Self {
        Self {
            limit,
            min_score: 0.50,
            prefer_user: false,
            exclude_outcomes: false,
            bump_recall_count: false,
            ..Default::default()
        }
    }
}

#[derive(Clone)]
pub struct VectorMemory {
    inner: Arc<Inner>,
}

struct Inner {
    data_dir: PathBuf,
    llm: LlmClient,
    ready: OnceCell<()>,
    /// Set when rows were embedded with another model and re-embedding failed.
    stale: std::sync::Mutex<Option<String>>,
}

impl VectorMemory {
    pub fn new(data_dir: impl AsRef<Path>, llm: LlmClient) -> Result<Self> {
        let data_dir = data_dir.as_ref().to_path_buf();
        db::open(&data_dir)?;
        Ok(Self {
            inner: Arc::new(Inner {
                data_dir,
                llm,
                ready: OnceCell::new(),
                stale: std::sync::Mutex::new(None),
            }),
        })
    }

    fn conn(&self) -> Result<Connection> {
        db::open(&self.inner.data_dir)
    }

    /// Import legacy LanceDB memory once, then make sure every row was embedded with
    /// the current embeddings model.
    async fn ensure_ready(&self) -> Result<()> {
        self.inner
            .ready
            .get_or_try_init(|| async {
                if let Err(err) = self.import_lance().await {
                    eprintln!("piya: memory import from memory.lance failed: {err:#}");
                }
                if let Err(err) = self.reembed_stale_rows().await {
                    eprintln!("piya: memory re-embedding failed: {err:#}");
                    *self.inner.stale.lock().unwrap() = Some(format!(
                        "memory was indexed with another embeddings model and re-indexing failed: {err:#}"
                    ));
                }
                Ok::<(), anyhow::Error>(())
            })
            .await?;
        Ok(())
    }

    async fn import_lance(&self) -> Result<()> {
        let conn = self.conn()?;
        if db::get_state(&conn, LANCE_IMPORTED_KEY)?.is_some() {
            return Ok(());
        }
        let lance_dir = self.inner.data_dir.join("memory.lance");
        if !lance_dir.is_dir() {
            db::set_state(&conn, LANCE_IMPORTED_KEY, "none")?;
            return Ok(());
        }
        let model = std::fs::read_to_string(self.inner.data_dir.join("memory-embedding.json"))
            .ok()
            .and_then(|raw| serde_json::from_str::<serde_json::Value>(&raw).ok())
            .and_then(|v| v.get("embedding").and_then(|e| e.as_str()).map(str::to_string))
            .unwrap_or_else(|| LEGACY_EMBEDDING_ID.to_string());
        let rows = crate::lance_import::export_rows(&lance_dir).await?;
        let tx = conn.unchecked_transaction()?;
        for r in &rows {
            tx.execute(
                "INSERT OR IGNORE INTO memories(id, text, kind, task_id, tags, accuracy, source, created_at,
                                               recall_count, embedding, embedding_model)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
                params![
                    r.id, r.text, r.kind, r.task_id, r.tags, r.accuracy, r.source, r.created_at,
                    r.recall_count, db::embedding_to_blob(&r.vector), model
                ],
            )?;
        }
        db::set_state(&tx, LANCE_IMPORTED_KEY, &Utc::now().to_rfc3339())?;
        tx.commit()?;
        let backup = self.inner.data_dir.join("memory.lance.migrated");
        if !backup.exists() {
            let _ = std::fs::rename(&lance_dir, &backup);
        }
        eprintln!("piya: imported {} memories from memory.lance into piya.db", rows.len());
        Ok(())
    }

    async fn reembed_stale_rows(&self) -> Result<()> {
        let Some(model) = self.inner.llm.embedding_id() else {
            return Ok(());
        };
        let stale: Vec<(String, String)> = {
            let conn = self.conn()?;
            let mut stmt = conn.prepare(
                "SELECT id, text FROM memories WHERE embedding IS NULL OR embedding_model != ?1",
            )?;
            let rows = stmt
                .query_map(params![model], |r| Ok((r.get(0)?, r.get(1)?)))?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            rows
        };
        if stale.is_empty() {
            return Ok(());
        }
        eprintln!("piya: re-embedding {} memories with {model}…", stale.len());
        for chunk in stale.chunks(64) {
            let texts: Vec<String> = chunk.iter().map(|(_, t)| t.clone()).collect();
            let vectors = self.inner.llm.embed(&texts).await?;
            anyhow::ensure!(vectors.len() == texts.len(), "embeddings response size mismatch");
            let conn = self.conn()?;
            let tx = conn.unchecked_transaction()?;
            for ((id, _), v) in chunk.iter().zip(vectors) {
                tx.execute(
                    "UPDATE memories SET embedding = ?1, embedding_model = ?2 WHERE id = ?3",
                    params![db::embedding_to_blob(&v), model, id],
                )?;
            }
            tx.commit()?;
        }
        eprintln!("piya: memory re-embedded with {model}");
        Ok(())
    }

    async fn embed_one(&self, text: &str) -> Result<Vec<f32>> {
        self.ensure_ready().await?;
        if let Some(err) = self.inner.stale.lock().unwrap().clone() {
            anyhow::bail!("{err}");
        }
        self.inner
            .llm
            .embed(&[text.to_string()])
            .await?
            .into_iter()
            .next()
            .context("empty embedding response")
    }

    pub async fn remember(
        &self,
        text: &str,
        kind: MemoryKind,
        task_id: Option<&str>,
        tags: &[String],
        accuracy: f64,
    ) -> Result<String> {
        self.remember_with_source(text, kind, task_id, tags, accuracy, "agent")
            .await
    }

    pub async fn remember_with_source(
        &self,
        text: &str,
        kind: MemoryKind,
        task_id: Option<&str>,
        tags: &[String],
        accuracy: f64,
        source: &str,
    ) -> Result<String> {
        let text = text.trim();
        anyhow::ensure!(!text.is_empty(), "memory text is empty");

        let embedding = self.embed_one(text).await?;

        // Skip near-duplicates; if the new note is richer, replace the short one.
        if let Some((dup_id, dup_text, sim)) = self.find_near_duplicate(text, embedding.clone()).await? {
            if sim >= 0.97 && normalize_memory_text(text) == normalize_memory_text(&dup_text) {
                return Ok(dup_id);
            }
            if sim >= 0.92 && text.len() > dup_text.len().saturating_add(20) {
                let _ = self.delete_by_id(&dup_id).await;
            } else if sim >= 0.95 && text.len() <= dup_text.len() {
                return Ok(dup_id);
            }
        }

        let id = Uuid::new_v4().to_string();
        let model = self.inner.llm.embedding_id().unwrap_or_default();
        self.conn()?.execute(
            "INSERT INTO memories(id, text, kind, task_id, tags, accuracy, source, created_at,
                                  recall_count, embedding, embedding_model)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, 0, ?9, ?10)",
            params![
                id,
                text,
                kind.as_str(),
                task_id.unwrap_or(""),
                serde_json::to_string(tags)?,
                accuracy,
                source,
                Utc::now().to_rfc3339(),
                db::embedding_to_blob(&embedding),
                model
            ],
        )?;
        Ok(id)
    }

    /// Find a near-duplicate by embedding similarity. Returns (id, text, similarity).
    async fn find_near_duplicate(&self, text: &str, embedding: Vec<f32>) -> Result<Option<(String, String, f64)>> {
        let opts = RecallOpts {
            limit: 3,
            min_score: 0.90,
            prefer_user: false,
            exclude_outcomes: false,
            bump_recall_count: false,
            ..Default::default()
        };
        match self.recall_with_embedding(text, embedding, opts).await {
            Ok(hits) => Ok(hits.into_iter().next().map(|h| (h.id, h.text, h.score))),
            Err(_) => Ok(None),
        }
    }

    pub async fn recall(&self, query: &str, opts: RecallOpts) -> Result<Vec<MemoryHit>> {
        let query_embedding = self.embed_one(query).await?;
        self.recall_with_embedding(query, query_embedding, opts).await
    }

    async fn recall_with_embedding(
        &self,
        query: &str,
        query_embedding: Vec<f32>,
        opts: RecallOpts,
    ) -> Result<Vec<MemoryHit>> {
        let model = self.inner.llm.embedding_id().unwrap_or_default();
        let conn = self.conn()?;
        let mut sql = String::from(
            "SELECT id, text, kind, accuracy, source, tags, embedding FROM memories
             WHERE embedding IS NOT NULL AND embedding_model = ?1 AND accuracy >= ?2",
        );
        if let Some(k) = opts.kind {
            sql.push_str(&format!(" AND kind = '{}'", k.as_str()));
        }
        if opts.exclude_outcomes {
            sql.push_str(" AND kind != 'outcome'");
        }
        let mut stmt = conn.prepare(&sql)?;
        let q_norm = norm(&query_embedding);
        let rows = stmt.query_map(params![model, opts.min_accuracy], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, f64>(3)?,
                r.get::<_, String>(4)?,
                r.get::<_, String>(5)?,
                r.get::<_, Vec<u8>>(6)?,
            ))
        })?;

        let mut hits = Vec::new();
        for row in rows {
            let (id, text, kind, accuracy, source, tags, blob) = row?;
            let v = db::blob_to_embedding(&blob);
            if v.len() != query_embedding.len() {
                continue;
            }
            let cos = cosine(&query_embedding, q_norm, &v);
            // Same scale as the previous LanceDB ranking: cosine distance ∈ [0, 2] → [0, 1].
            let similarity = ((1.0 + cos) / 2.0).clamp(0.0, 1.0);
            let kind = MemoryKind::parse(&kind);

            let mut score = similarity * (0.5 + accuracy * 0.5);
            score += keyword_boost(query, &text);
            score += fact_signal_boost(&text);
            score += query_context_adjust(query, &text);
            if opts.prefer_user && is_user_memory(&source, &tags) {
                score += 0.08;
            }
            // Prefer denser factual notes over one-line fragments.
            if text.len() > 80 {
                score += 0.02;
            }
            if text.len() > 160 {
                score += 0.02;
            }
            hits.push(MemoryHit { id, text, kind, score, accuracy, source });
        }

        hits.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap_or(std::cmp::Ordering::Equal));

        // Dedup near-identical texts (keep highest score).
        let mut deduped: Vec<MemoryHit> = Vec::new();
        for hit in hits {
            if hit.score < opts.min_score {
                continue;
            }
            let norm_text = normalize_memory_text(&hit.text);
            if deduped.iter().any(|h| {
                normalize_memory_text(&h.text) == norm_text || texts_near_duplicate(&h.text, &hit.text)
            }) {
                continue;
            }
            deduped.push(hit);
            if deduped.len() >= opts.limit {
                break;
            }
        }
        Ok(deduped)
    }

    /// List recent memories for Settings (no embedding / vector search).
    pub async fn list_recent(&self, limit: usize) -> Result<Vec<MemoryEntry>> {
        let _ = self.ensure_ready().await;
        let conn = self.conn()?;
        let mut stmt = conn.prepare(
            "SELECT id, text, kind, tags, created_at, source FROM memories ORDER BY created_at DESC LIMIT ?1",
        )?;
        let rows = stmt
            .query_map(params![limit.clamp(1, 500) as i64], |r| {
                Ok(MemoryEntry {
                    id: r.get(0)?,
                    text: r.get(1)?,
                    kind: r.get(2)?,
                    tags: r.get(3)?,
                    created_at: r.get(4)?,
                    source: r.get(5)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    pub async fn delete_by_id(&self, id: &str) -> Result<()> {
        self.conn()?
            .execute("DELETE FROM memories WHERE id = ?1", params![id])
            .with_context(|| format!("delete memory {id}"))?;
        Ok(())
    }

    /// Kept for the `piya optimize` command: compacts the database file.
    pub async fn optimize(&self) -> Result<()> {
        self.conn()?.execute_batch("PRAGMA wal_checkpoint(TRUNCATE); VACUUM;")?;
        Ok(())
    }

    pub fn format_context(hits: &[MemoryHit]) -> String {
        if hits.is_empty() {
            return "No relevant past memories found.".into();
        }
        let mut lines = vec![
            "Relevant memories from prior work (quote these facts exactly; do not invent details):"
                .to_string(),
        ];
        for (i, hit) in hits.iter().enumerate() {
            let src = if hit.source == "user" { "user" } else { hit.kind.as_str() };
            lines.push(format!(
                "{}. [{src}, accuracy={:.2}] {}",
                i + 1,
                hit.accuracy,
                hit.text
            ));
        }
        lines.join("\n")
    }

    pub fn stats(&self) -> Result<usize> {
        let n: i64 = self.conn()?.query_row("SELECT count(*) FROM memories", [], |r| r.get(0))?;
        Ok(n as usize)
    }
}

fn norm(v: &[f32]) -> f64 {
    v.iter().map(|x| (*x as f64) * (*x as f64)).sum::<f64>().sqrt()
}

fn cosine(a: &[f32], a_norm: f64, b: &[f32]) -> f64 {
    let dot: f64 = a.iter().zip(b).map(|(x, y)| (*x as f64) * (*y as f64)).sum();
    let denom = a_norm * norm(b);
    if denom == 0.0 { 0.0 } else { dot / denom }
}

#[derive(Clone, Debug)]
pub struct MemoryEntry {
    pub id: String,
    pub text: String,
    pub kind: String,
    pub tags: String,
    pub created_at: String,
    pub source: String,
}

fn is_user_memory(source: &str, tags_json: &str) -> bool {
    if source.eq_ignore_ascii_case("user") {
        return true;
    }
    tags_json.to_lowercase().contains("\"user\"")
}

fn normalize_memory_text(text: &str) -> String {
    text.to_lowercase()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn texts_near_duplicate(a: &str, b: &str) -> bool {
    let na = normalize_memory_text(a);
    let nb = normalize_memory_text(b);
    if na == nb {
        return true;
    }
    // One note is a short fragment of the other.
    if na.len() >= 24 && nb.contains(&na) {
        return true;
    }
    if nb.len() >= 24 && na.contains(&nb) {
        return true;
    }
    false
}

/// Prefer concrete inventory facts; demote suggestion/advice paraphrases.
fn fact_signal_boost(text: &str) -> f64 {
    let t = text.to_lowercase();
    let mut boost = 0.0;
    if t.contains("i-")
        || t.contains("vol-")
        || t.contains("instance id")
        || t.contains("volume id")
        || t.contains("ami-")
    {
        boost += 0.12;
    }
    if t.contains("instance type")
        || t.contains("storage")
        || t.contains(" gb")
        || t.contains("gib")
        || t.contains("ebs")
    {
        boost += 0.06;
    }
    if t.contains("suggestion")
        || t.contains("consider ")
        || t.contains("looking to")
        || t.starts_with("suggest ")
        || t.contains("needs to compare")
    {
        boost -= 0.14;
    }
    boost
}

/// Avoid mixing unrelated secrets/credentials into infra inventory answers.
fn query_context_adjust(query: &str, text: &str) -> f64 {
    let q = query.to_lowercase();
    let t = text.to_lowercase();
    let asking_server = q.contains("server")
        || q.contains("instance")
        || q.contains("volume")
        || q.contains("ebs")
        || q.contains("ec2");
    if !asking_server {
        return 0.0;
    }
    if t.contains("secret key")
        || t.contains("access key")
        || t.contains("password")
        || t.contains("api token")
        || t.contains("r2 credentials")
    {
        return -0.25;
    }
    0.0
}

/// Boost exact/keyword matches (instance ids, volume ids, hostnames, shared nouns).
fn keyword_boost(query: &str, text: &str) -> f64 {
    let q_lower = query.to_lowercase();
    let t_lower = text.to_lowercase();
    let mut boost = 0.0;

    // Cloud resource ids and similar tokens.
    for token in q_lower.split(|c: char| !c.is_ascii_alphanumeric() && c != '-' && c != '_') {
        if token.len() < 4 {
            continue;
        }
        let looks_like_id = (token.contains('-')
            && token.chars().any(|c| c.is_ascii_digit()))
            || token.starts_with("i-")
            || token.starts_with("vol-")
            || token.starts_with("ami-")
            || token.starts_with("sg-");
        if looks_like_id && t_lower.contains(token) {
            boost += 0.18;
        }
    }

    let stop = [
        "the", "and", "for", "with", "that", "this", "from", "have", "what", "when",
        "your", "about", "give", "tell", "please", "details", "remember", "recall",
    ];
    let mut overlap = 0usize;
    for token in q_lower.split(|c: char| !c.is_ascii_alphanumeric()) {
        if token.len() < 4 || stop.contains(&token) {
            continue;
        }
        if t_lower.contains(token) {
            overlap += 1;
        }
    }
    boost += (overlap as f64 * 0.025).min(0.12);
    boost.min(0.30)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keyword_boost_prefers_exact_ids() {
        let b = keyword_boost(
            "Cosmetic LMS instance volume",
            "Cosmetic LMS Project has volume id: vol-0522e731c0a8cd3fa",
        );
        assert!(b > 0.0, "expected keyword overlap boost, got {b}");
        let id_boost = keyword_boost(
            "vol-0522e731c0a8cd3fa",
            "Cosmetic LMS Project has volume id: vol-0522e731c0a8cd3fa",
        );
        assert!(id_boost >= 0.18, "expected id boost, got {id_boost}");
    }

    #[test]
    fn near_duplicate_detects_fragments() {
        assert!(texts_near_duplicate(
            "Allure Server on t4g.medium wants to increase CPU cores.",
            "Allure Server on t4g.medium wants to increase CPU cores. Looking at c6g options."
        ));
    }

    #[test]
    fn fact_signal_prefers_ids_over_advice() {
        let fact = fact_signal_boost(
            "Cosmetic LMS Project has volume id: vol-0522e731c0a8cd3fa and instance type m6i.large",
        );
        let advice = fact_signal_boost(
            "Suggestions: Consider instance types that offer more CPU cores than t4g.medium",
        );
        assert!(fact > advice, "fact={fact} advice={advice}");
    }
}

