use std::io::{self, Write};

use anyhow::Result;
use serde::Serialize;

use crate::memory::MemoryHit;
use crate::tasks::Task;

#[derive(Serialize)]
pub struct StatusReport {
    pub pending: usize,
    pub in_progress: usize,
    pub done: usize,
    pub failed: usize,
    pub vector_memories: Option<usize>,
    pub workspace: String,
    pub next_tasks: Vec<Task>,
}

#[derive(Serialize)]
pub struct MemoryHitJson {
    pub kind: String,
    pub score: f64,
    pub accuracy: f64,
    pub text: String,
}

impl From<&MemoryHit> for MemoryHitJson {
    fn from(hit: &MemoryHit) -> Self {
        Self {
            kind: hit.kind.as_str().to_string(),
            score: hit.score,
            accuracy: hit.accuracy,
            text: hit.text.clone(),
        }
    }
}

pub fn print_json<T: Serialize>(value: &T) -> Result<()> {
    let mut stdout = io::stdout().lock();
    serde_json::to_writer_pretty(&mut stdout, value)?;
    stdout.write_all(b"\n")?;
    Ok(())
}
