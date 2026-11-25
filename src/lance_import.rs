//! One-time reader for the legacy LanceDB memory store (`data/memory.lance`).
//!
//! Used only to import old memories into SQLite. Delete this module (and the
//! `lancedb` / `arrow-*` dependencies) once installs have migrated.

use std::path::Path;

use anyhow::{anyhow, Context, Result};
use arrow_array::{Array, FixedSizeListArray, Float32Array, Float64Array, Int64Array, RecordBatch, StringArray};
use futures::TryStreamExt;
use lancedb::query::{ExecutableQuery, QueryBase};

pub struct LegacyMemory {
    pub id: String,
    pub text: String,
    pub kind: String,
    pub task_id: String,
    pub tags: String,
    pub accuracy: f64,
    pub source: String,
    pub created_at: String,
    pub recall_count: i64,
    pub vector: Vec<f32>,
}

pub async fn export_rows(lance_dir: &Path) -> Result<Vec<LegacyMemory>> {
    let uri = lance_dir.to_str().ok_or_else(|| anyhow!("memory.lance path is not UTF-8"))?;
    let db = lancedb::connect(uri).execute().await.context("connect LanceDB")?;
    let names = db.table_names().execute().await?;
    if !names.iter().any(|n| n == "memories") {
        return Ok(Vec::new());
    }
    let table = db.open_table("memories").execute().await.context("open memories table")?;
    let total = table.count_rows(None).await.unwrap_or(0);
    if total == 0 {
        return Ok(Vec::new());
    }
    let batches: Vec<RecordBatch> = table
        .query()
        .limit(total)
        .execute()
        .await
        .context("read memories")?
        .try_collect()
        .await
        .context("collect memories")?;

    let mut out = Vec::with_capacity(total);
    for b in batches {
        let s = |name: &str| -> Option<StringArray> {
            b.column_by_name(name)?.as_any().downcast_ref::<StringArray>().cloned()
        };
        let ids = s("id").ok_or_else(|| anyhow!("memories table has no id column"))?;
        let texts = s("text").ok_or_else(|| anyhow!("memories table has no text column"))?;
        let kinds = s("kind");
        let task_ids = s("task_id");
        let tags = s("tags");
        let sources = s("source");
        let created = s("created_at");
        let accuracy = b
            .column_by_name("accuracy")
            .and_then(|c| c.as_any().downcast_ref::<Float64Array>().cloned());
        let recalls = b
            .column_by_name("recall_count")
            .and_then(|c| c.as_any().downcast_ref::<Int64Array>().cloned());
        let vectors = b
            .column_by_name("vector")
            .and_then(|c| c.as_any().downcast_ref::<FixedSizeListArray>().cloned())
            .ok_or_else(|| anyhow!("memories table has no vector column"))?;

        let get = |a: &Option<StringArray>, i: usize| -> String {
            a.as_ref().filter(|a| !a.is_null(i)).map(|a| a.value(i).to_string()).unwrap_or_default()
        };
        for i in 0..b.num_rows() {
            let v = vectors.value(i);
            let floats = v
                .as_any()
                .downcast_ref::<Float32Array>()
                .map(|f| f.values().to_vec())
                .unwrap_or_default();
            out.push(LegacyMemory {
                id: ids.value(i).to_string(),
                text: texts.value(i).to_string(),
                kind: { let k = get(&kinds, i); if k.is_empty() { "learning".into() } else { k } },
                task_id: get(&task_ids, i),
                tags: { let t = get(&tags, i); if t.is_empty() { "[]".into() } else { t } },
                accuracy: accuracy.as_ref().map(|a| a.value(i)).unwrap_or(0.5),
                source: get(&sources, i),
                created_at: get(&created, i),
                recall_count: recalls.as_ref().map(|a| a.value(i)).unwrap_or(0),
                vector: floats,
            });
        }
    }
    Ok(out)
}
