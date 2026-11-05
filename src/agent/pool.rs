//! Parallel task workers.
//!
//! Each worker is an [`AgentLoop`] on its own OS thread with its own runtime, so a
//! worker blocked on a long shell command never stalls the others. Workers share the
//! SQLite queue and claim tasks atomically ([`crate::tasks::TaskQueue::claim_next`]).

use anyhow::Result;
use tracing::{error, info};
use uuid::Uuid;

use super::loop_::AgentLoop;
use crate::config::Settings;

/// Run `workers` loops until the queue is drained (`continuous = false`) or forever.
pub fn run_pool(settings: Settings, workers: usize, interactive: bool, continuous: bool) -> Result<()> {
    let workers = workers.max(1);
    info!("Starting {workers} worker(s)");
    let pid = std::process::id();
    let mut handles = Vec::new();
    for n in 1..=workers {
        let settings = settings.clone();
        let worker_id = format!("{pid}-w{n}-{}", &Uuid::new_v4().to_string()[..8]);
        let handle = std::thread::Builder::new()
            .name(format!("piya-worker-{n}"))
            .spawn(move || -> Result<usize> {
                let rt = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()?;
                rt.block_on(async move {
                    let mut agent = AgentLoop::new(settings, true)?
                        .with_worker_id(worker_id)
                        .with_interactive(interactive);
                    if continuous {
                        agent.run_continuous().await?;
                        return Ok(0);
                    }
                    let mut done = 0usize;
                    loop {
                        match agent.run_once().await {
                            Ok(Some(result)) => {
                                done += 1;
                                agent.print_cycle_summary(&result);
                            }
                            Ok(None) => break,
                            Err(err) => error!("task attempt failed: {err:#}"),
                        }
                    }
                    Ok(done)
                })
            })?;
        handles.push(handle);
    }

    let mut total = 0usize;
    let mut failed = false;
    for handle in handles {
        match handle.join() {
            Ok(Ok(n)) => total += n,
            Ok(Err(err)) => {
                error!("worker stopped: {err:#}");
                failed = true;
            }
            Err(_) => {
                error!("worker thread panicked");
                failed = true;
            }
        }
    }
    info!("All workers finished ({total} task(s) processed)");
    if failed {
        anyhow::bail!("one or more workers stopped with an error");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tasks::{TaskQueue, TaskStatus};

    #[test]
    fn pool_drains_queue_without_double_runs() {
        std::env::set_var("PIYA_NOTIFY", "off");
        let dir = std::env::temp_dir().join(format!("piya-pool-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let settings = Settings {
            openai_api_key: "test-key".into(),
            openai_base_url: "http://127.0.0.1:9/v1".into(),
            model: "m".into(),
            embedding_model: "e".into(),
            workspace: dir.join("ws"),
            data_dir: dir.clone(),
            poll_interval_sec: 1,
            max_cycles: 0,
            min_accuracy: 0.7,
            max_retries: 3,
            workers: 3,
        };
        settings.ensure_dirs().unwrap();
        let queue = TaskQueue::new(dir.join("piya.db")).unwrap();
        // Reminder tasks complete without calling the LLM.
        for i in 0..12 {
            queue
                .add(&format!("Remind me {i}"), &format!("Remind me about item {i}"), 0, "")
                .unwrap();
        }
        run_pool(settings, 3, false, false).unwrap();
        let done = queue.list(Some(TaskStatus::Done), 100).unwrap();
        assert_eq!(done.len(), 12, "all tasks done");
        assert!(done.iter().all(|t| t.attempts == 1), "each task ran exactly once");
        let _ = std::fs::remove_dir_all(dir);
    }
}
