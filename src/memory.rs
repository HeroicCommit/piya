//! Embedded vector memory backed by LanceDB (local directory under `data/memory.lance`).
//!
//! On first open, rows from the legacy SQLite `memory.db` are migrated automatically.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use anyhow::{anyhow, Context, Result};
use arrow_array::{
    Array, ArrayRef, FixedSizeListArray, Float32Array, Float64Array, Int64Array, RecordBatch,
    RecordBatchIterator, RecordBatchReader, StringArray,
};
use arrow_schema::{DataType, Field, Schema};
use chrono::Utc;
use futures::TryStreamExt;
use lancedb::query::{ExecutableQuery, QueryBase};
use lancedb::table::OptimizeAction;
use lancedb::{connect, DistanceType, Table};
use rusqlite::Connection;
use tokio::sync::OnceCell;
use uuid::Uuid;

use crate::llm::LlmClient;

/// Default OpenAI `text-embedding-3-small` dimensionality.
const DEFAULT_DIM: i32 = 1536;
const TABLE_NAME: &str = "memories";

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
    inner: Arc<VectorMemoryInner>,
}

struct VectorMemoryInner {
    lance_dir: PathBuf,
    legacy_sqlite: PathBuf,
    llm: LlmClient,
    dim: i32,
    table: OnceCell<Table>,
    writes_since_optimize: AtomicUsize,
}

impl VectorMemory {
    pub fn new(data_dir: impl AsRef<Path>, llm: LlmClient) -> Result<Self> {
        let data_dir = data_dir.as_ref();
        let lance_dir = data_dir.join("memory.lance");
        std::fs::create_dir_all(&lance_dir)
            .with_context(|| format!("create {}", lance_dir.display()))?;
        Ok(Self {
            inner: Arc::new(VectorMemoryInner {
                lance_dir,
                legacy_sqlite: data_dir.join("memory.db"),
                llm,
                dim: DEFAULT_DIM,
                table: OnceCell::new(),
                writes_since_optimize: AtomicUsize::new(0),
            }),
        })
    }

    async fn table(&self) -> Result<&Table> {
        self.inner
            .table
            .get_or_try_init(|| async { self.open_or_create().await })
            .await
    }

    async fn open_or_create(&self) -> Result<Table> {
        let uri = self
            .inner
            .lance_dir
            .to_str()
            .ok_or_else(|| anyhow!("memory.lance path is not valid UTF-8"))?;
        let db = connect(uri).execute().await.context("connect LanceDB")?;

        let names = db.table_names().execute().await.context("list LanceDB tables")?;
        let table = if names.iter().any(|n| n == TABLE_NAME) {
            db.open_table(TABLE_NAME)
                .execute()
                .await
                .context("open memories table")?
        } else {
            let schema = memories_schema(self.inner.dim);
            let empty = RecordBatch::new_empty(schema.clone());
            let batches: Box<dyn RecordBatchReader + Send> =
                Box::new(RecordBatchIterator::new(vec![Ok(empty)], schema));
            db.create_table(TABLE_NAME, batches)
                .execute()
                .await
                .context("create memories table")?
        };

        let count = table.count_rows(None).await.unwrap_or(0);
        if count == 0 && self.inner.legacy_sqlite.is_file() {
            if let Err(err) =
                migrate_sqlite_into(&table, &self.inner.legacy_sqlite, self.inner.dim).await
            {
                eprintln!("piya: LanceDB migration from memory.db skipped: {err}");
            } else {
                let bak = self.inner.legacy_sqlite.with_extension("db.bak");
                let _ = std::fs::rename(&self.inner.legacy_sqlite, &bak);
                eprintln!(
                    "piya: migrated vector memory from SQLite → LanceDB ({})",
                    self.inner.lance_dir.display()
                );
            }
        }

        // Every add/delete creates a new file + version; compact on open so the
        // directory does not grow without bound.
        run_optimize(&table).await;

        Ok(table)
    }

    /// Compact small files and prune old table versions (best-effort).
    pub async fn optimize(&self) -> Result<()> {
        let table = self.table().await?;
        self.inner.writes_since_optimize.store(0, Ordering::Relaxed);
        table
            .optimize(OptimizeAction::All)
            .await
            .context("optimize memory table")?;
        Ok(())
    }

    /// Compact after a batch of writes so long-running daemons stay small too.
    async fn note_write(&self, table: &Table) {
        let n = self.inner.writes_since_optimize.fetch_add(1, Ordering::Relaxed) + 1;
        if n >= OPTIMIZE_EVERY_WRITES {
            self.inner.writes_since_optimize.store(0, Ordering::Relaxed);
            run_optimize(table).await;
        }
    }

    async fn embed_one(&self, text: &str) -> Result<Vec<f32>> {
        let embedding = self
            .inner
            .llm
            .embed(&[text.to_string()])
            .await?
            .into_iter()
            .next()
            .context("empty embedding response")?;
        anyhow::ensure!(
            embedding.len() == self.inner.dim as usize,
            "embedding dim {} != expected {}",
            embedding.len(),
            self.inner.dim
        );
        Ok(embedding)
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
        anyhow::ensure!(!text.is_empty(), "memory text cannot be empty");

        let embedding = self.embed_one(text).await?;

        // Skip near-duplicates; if the new note is richer, replace the short one.
        if let Some((dup_id, dup_text, sim)) =
            self.find_near_duplicate(text, embedding.clone()).await?
        {
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
        let table = self.table().await?;
        let batch = memory_batch(
            self.inner.dim,
            &[MemoryRow {
                id: id.clone(),
                text: text.to_string(),
                kind: kind.as_str().to_string(),
                task_id: task_id.unwrap_or("").to_string(),
                tags: serde_json::to_string(tags)?,
                accuracy,
                source: source.to_string(),
                created_at: Utc::now().to_rfc3339(),
                recall_count: 0,
                vector: embedding,
            }],
        )?;
        let schema = batch.schema();
        let reader: Box<dyn RecordBatchReader + Send> =
            Box::new(RecordBatchIterator::new(vec![Ok(batch)], schema));
        table
            .add(reader)
            .execute()
            .await
            .context("LanceDB add memory")?;
        self.note_write(table).await;
        Ok(id)
    }

    /// Find a near-duplicate by embedding similarity. Returns (id, text, similarity).
    async fn find_near_duplicate(
        &self,
        text: &str,
        embedding: Vec<f32>,
    ) -> Result<Option<(String, String, f64)>> {
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
        self.recall_with_embedding(query, query_embedding, opts)
            .await
    }

    async fn recall_with_embedding(
        &self,
        query: &str,
        query_embedding: Vec<f32>,
        opts: RecallOpts,
    ) -> Result<Vec<MemoryHit>> {
        let table = self.table().await?;
        let mut filter = format!("accuracy >= {}", opts.min_accuracy);
        if let Some(k) = opts.kind {
            filter = format!("{filter} AND kind = '{}'", k.as_str());
        }
        if opts.exclude_outcomes {
            filter = format!("{filter} AND kind != 'outcome'");
        }

        // Over-fetch so filters / dedup / score floor still yield `limit` good hits.
        let fetch = opts.limit.saturating_mul(5).max(opts.limit).max(16);
        let batches = table
            .vector_search(query_embedding)
            .context("vector_search")?
            .distance_type(DistanceType::Cosine)
            .only_if(filter)
            .limit(fetch)
            .execute()
            .await
            .context("execute vector search")?
            .try_collect::<Vec<_>>()
            .await
            .context("collect vector search")?;

        let mut hits = Vec::new();
        for batch in batches {
            let ids = column_utf8(&batch, "id")?;
            let texts = column_utf8(&batch, "text")?;
            let kinds = column_utf8(&batch, "kind")?;
            let accuracies = column_f64(&batch, "accuracy")?;
            let sources = column_utf8(&batch, "source").unwrap_or_default();
            let tags_col = column_utf8(&batch, "tags").unwrap_or_default();
            let distances =
                column_f32(&batch, "_distance").unwrap_or_else(|_| vec![0.0; batch.num_rows()]);

            for i in 0..batch.num_rows() {
                let distance = distances.get(i).copied().unwrap_or(0.0) as f64;
                // Cosine distance ∈ [0, 2] → similarity ∈ [0, 1]
                let similarity = (1.0 - distance / 2.0).clamp(0.0, 1.0);
                let accuracy = accuracies.get(i).copied().unwrap_or(0.5);
                let text = texts.get(i).cloned().unwrap_or_default();
                let source = sources.get(i).cloned().unwrap_or_default();
                let tags = tags_col.get(i).cloned().unwrap_or_default();
                let kind =
                    MemoryKind::parse(kinds.get(i).map(String::as_str).unwrap_or("learning"));

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

                hits.push(MemoryHit {
                    id: ids.get(i).cloned().unwrap_or_default(),
                    text,
                    kind,
                    score,
                    accuracy,
                    source,
                });
            }
        }

        hits.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
        });

        // Dedup near-identical texts (keep highest score).
        let mut deduped: Vec<MemoryHit> = Vec::new();
        for hit in hits {
            if hit.score < opts.min_score {
                continue;
            }
            let norm = normalize_memory_text(&hit.text);
            if deduped.iter().any(|h| {
                let other = normalize_memory_text(&h.text);
                other == norm || texts_near_duplicate(&h.text, &hit.text)
            }) {
                continue;
            }
            deduped.push(hit);
            if deduped.len() >= opts.limit {
                break;
            }
        }

        // Ranking never uses recall_count; skip per-hit LanceDB writes.
        Ok(deduped)
    }

    /// List recent memories for Settings (no embedding / vector search).
    pub async fn list_recent(&self, limit: usize) -> Result<Vec<MemoryEntry>> {
        let table = self.table().await?;
        let limit = limit.clamp(1, 500);
        let batches = table
            .query()
            .select(lancedb::query::Select::columns(&[
                "id",
                "text",
                "kind",
                "tags",
                "created_at",
                "source",
            ]))
            .limit(limit)
            .execute()
            .await
            .context("list memories")?
            .try_collect::<Vec<_>>()
            .await
            .context("collect memories")?;

        let mut entries = Vec::new();
        for batch in batches {
            let ids = column_utf8(&batch, "id")?;
            let texts = column_utf8(&batch, "text")?;
            let kinds = column_utf8(&batch, "kind")?;
            let tags = column_utf8(&batch, "tags").unwrap_or_default();
            let created = column_utf8(&batch, "created_at").unwrap_or_default();
            let sources = column_utf8(&batch, "source").unwrap_or_default();
            for i in 0..batch.num_rows() {
                entries.push(MemoryEntry {
                    id: ids.get(i).cloned().unwrap_or_default(),
                    text: texts.get(i).cloned().unwrap_or_default(),
                    kind: kinds.get(i).cloned().unwrap_or_default(),
                    tags: tags.get(i).cloned().unwrap_or_default(),
                    created_at: created.get(i).cloned().unwrap_or_default(),
                    source: sources.get(i).cloned().unwrap_or_default(),
                });
            }
        }
        // Newest first when created_at is RFC3339-sortable.
        entries.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        entries.truncate(limit);
        Ok(entries)
    }

    pub async fn delete_by_id(&self, id: &str) -> Result<()> {
        let table = self.table().await?;
        let predicate = format!("id = '{}'", sql_escape(id));
        table
            .delete(&predicate)
            .await
            .with_context(|| format!("delete memory {id}"))?;
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
        let dir = self.inner.lance_dir.clone();
        crate::runtime::block_on(async move {
            let uri = dir
                .to_str()
                .ok_or_else(|| anyhow!("memory.lance path is not valid UTF-8"))?;
            let db = connect(uri).execute().await.context("connect LanceDB")?;
            let names = db.table_names().execute().await?;
            if !names.iter().any(|n| n == TABLE_NAME) {
                return Ok(0);
            }
            let table = db.open_table(TABLE_NAME).execute().await?;
            Ok(table.count_rows(None).await?)
        })
    }
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

struct MemoryRow {
    id: String,
    text: String,
    kind: String,
    task_id: String,
    tags: String,
    accuracy: f64,
    source: String,
    created_at: String,
    recall_count: i64,
    vector: Vec<f32>,
}

/// Compact + prune after this many writes in one process.
const OPTIMIZE_EVERY_WRITES: usize = 50;

async fn run_optimize(table: &Table) {
    if let Err(err) = table.optimize(OptimizeAction::All).await {
        // Another Piya process may be committing concurrently; try again later.
        eprintln!("piya: memory optimize skipped: {err}");
    }
}

fn memories_schema(dim: i32) -> Arc<Schema> {
    Arc::new(Schema::new(vec![
        Field::new("id", DataType::Utf8, false),
        Field::new("text", DataType::Utf8, false),
        Field::new("kind", DataType::Utf8, false),
        Field::new("task_id", DataType::Utf8, false),
        Field::new("tags", DataType::Utf8, false),
        Field::new("accuracy", DataType::Float64, false),
        Field::new("source", DataType::Utf8, false),
        Field::new("created_at", DataType::Utf8, false),
        Field::new("recall_count", DataType::Int64, false),
        Field::new(
            "vector",
            DataType::FixedSizeList(Arc::new(Field::new("item", DataType::Float32, true)), dim),
            false,
        ),
    ]))
}

fn memory_batch(dim: i32, rows: &[MemoryRow]) -> Result<RecordBatch> {
    let schema = memories_schema(dim);
    let n = rows.len();
    let mut flat = Vec::with_capacity(n * dim as usize);
    for row in rows {
        anyhow::ensure!(
            row.vector.len() == dim as usize,
            "row {} has dim {}, expected {dim}",
            row.id,
            row.vector.len()
        );
        flat.extend_from_slice(&row.vector);
    }
    let values = Float32Array::from(flat);
    let vector = FixedSizeListArray::new(
        Arc::new(Field::new("item", DataType::Float32, true)),
        dim,
        Arc::new(values),
        None,
    );

    Ok(RecordBatch::try_new(
        schema,
        vec![
            Arc::new(StringArray::from(
                rows.iter().map(|r| r.id.as_str()).collect::<Vec<_>>(),
            )) as ArrayRef,
            Arc::new(StringArray::from(
                rows.iter().map(|r| r.text.as_str()).collect::<Vec<_>>(),
            )) as ArrayRef,
            Arc::new(StringArray::from(
                rows.iter().map(|r| r.kind.as_str()).collect::<Vec<_>>(),
            )) as ArrayRef,
            Arc::new(StringArray::from(
                rows.iter().map(|r| r.task_id.as_str()).collect::<Vec<_>>(),
            )) as ArrayRef,
            Arc::new(StringArray::from(
                rows.iter().map(|r| r.tags.as_str()).collect::<Vec<_>>(),
            )) as ArrayRef,
            Arc::new(Float64Array::from(
                rows.iter().map(|r| r.accuracy).collect::<Vec<_>>(),
            )) as ArrayRef,
            Arc::new(StringArray::from(
                rows.iter().map(|r| r.source.as_str()).collect::<Vec<_>>(),
            )) as ArrayRef,
            Arc::new(StringArray::from(
                rows.iter().map(|r| r.created_at.as_str()).collect::<Vec<_>>(),
            )) as ArrayRef,
            Arc::new(Int64Array::from(
                rows.iter().map(|r| r.recall_count).collect::<Vec<_>>(),
            )) as ArrayRef,
            Arc::new(vector) as ArrayRef,
        ],
    )?)
}

async fn migrate_sqlite_into(table: &Table, sqlite_path: &Path, expected_dim: i32) -> Result<usize> {
    // Load SQLite fully first — rusqlite types are !Send and must not cross awaits.
    let migrated = load_legacy_rows(sqlite_path, expected_dim)?;
    if migrated.is_empty() {
        return Ok(0);
    }

    let mut total = 0;
    for chunk in migrated.chunks(64) {
        let batch = memory_batch(expected_dim, chunk)?;
        let schema = batch.schema();
        let reader: Box<dyn RecordBatchReader + Send> =
            Box::new(RecordBatchIterator::new(vec![Ok(batch)], schema));
        table.add(reader).execute().await.context("migrate add")?;
        total += chunk.len();
    }
    Ok(total)
}

fn load_legacy_rows(sqlite_path: &Path, expected_dim: i32) -> Result<Vec<MemoryRow>> {
    let conn = Connection::open(sqlite_path).context("open legacy memory.db")?;
    let mut stmt = conn.prepare(
        "SELECT id, text, kind, task_id, tags, accuracy, source, created_at, recall_count, embedding FROM memories",
    )?;
    let rows = stmt.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
            row.get::<_, String>(3)?,
            row.get::<_, String>(4)?,
            row.get::<_, f64>(5)?,
            row.get::<_, String>(6)?,
            row.get::<_, String>(7)?,
            row.get::<_, i64>(8)?,
            row.get::<_, Vec<u8>>(9)?,
        ))
    })?;

    let mut migrated = Vec::new();
    for row in rows {
        let (id, text, kind, task_id, tags, accuracy, source, created_at, recall_count, blob) =
            row?;
        let vector = blob_to_embedding(&blob)?;
        if vector.len() != expected_dim as usize {
            eprintln!(
                "piya: skip memory {id}: embedding dim {} != {expected_dim}",
                vector.len()
            );
            continue;
        }
        migrated.push(MemoryRow {
            id,
            text,
            kind,
            task_id,
            tags,
            accuracy,
            source,
            created_at,
            recall_count,
            vector,
        });
    }
    Ok(migrated)
}

fn blob_to_embedding(blob: &[u8]) -> Result<Vec<f32>> {
    anyhow::ensure!(blob.len() % 4 == 0, "invalid embedding blob length");
    Ok(blob
        .chunks_exact(4)
        .map(|chunk| f32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]))
        .collect())
}

fn sql_escape(s: &str) -> String {
    s.replace('\'', "''")
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

fn column_utf8(batch: &RecordBatch, name: &str) -> Result<Vec<String>> {
    let col = batch
        .column_by_name(name)
        .with_context(|| format!("missing column {name}"))?;
    let arr = col
        .as_any()
        .downcast_ref::<StringArray>()
        .with_context(|| format!("column {name} is not Utf8"))?;
    Ok((0..arr.len())
        .map(|i| {
            if arr.is_null(i) {
                String::new()
            } else {
                arr.value(i).to_string()
            }
        })
        .collect())
}

fn column_f64(batch: &RecordBatch, name: &str) -> Result<Vec<f64>> {
    let col = batch
        .column_by_name(name)
        .with_context(|| format!("missing column {name}"))?;
    let arr = col
        .as_any()
        .downcast_ref::<Float64Array>()
        .with_context(|| format!("column {name} is not Float64"))?;
    Ok((0..arr.len())
        .map(|i| if arr.is_null(i) { 0.5 } else { arr.value(i) })
        .collect())
}

fn column_f32(batch: &RecordBatch, name: &str) -> Result<Vec<f32>> {
    let col = batch
        .column_by_name(name)
        .with_context(|| format!("missing column {name}"))?;
    let arr = col
        .as_any()
        .downcast_ref::<Float32Array>()
        .with_context(|| format!("column {name} is not Float32"))?;
    Ok((0..arr.len())
        .map(|i| if arr.is_null(i) { 0.0 } else { arr.value(i) })
        .collect())
}
