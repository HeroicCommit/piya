use anyhow::Result;
use serde_json::{json, Value};
use tracing::{info, warn};

use crate::agent::executor::Executor;
use crate::agent::expertise;
use crate::agent::planner::Planner;
use crate::agent::researcher::Researcher;
use crate::agent::reviewer::Reviewer;
use crate::config::Settings;
use crate::llm::LlmClient;
use crate::memory::{MemoryKind, VectorMemory};
use crate::tasks::{Task, TaskQueue, TaskStatus};

pub struct CycleResult {
    pub task: Task,
    pub accuracy: f64,
    pub passed: bool,
    pub attempts: i32,
    /// When set, deliver this as a reminder banner instead of a task-finished notice.
    pub reminder_body: Option<String>,
}

pub struct AgentLoop {
    settings: Settings,
    tasks: TaskQueue,
    llm: Option<LlmClient>,
    memory: Option<VectorMemory>,
    planner: Option<Planner>,
    researcher: Option<Researcher>,
    executor: Option<Executor>,
    reviewer: Option<Reviewer>,
    interactive: bool,
}

impl AgentLoop {
    pub fn new(settings: Settings, require_llm: bool) -> Result<Self> {
        let tasks = TaskQueue::new(settings.data_dir.join("piya.db"))?;
        let mut agent = Self {
            settings,
            tasks,
            llm: None,
            memory: None,
            planner: None,
            researcher: None,
            executor: None,
            reviewer: None,
            interactive: false,
        };
        if require_llm {
            agent.ensure_llm()?;
        }
        Ok(agent)
    }

    pub fn with_interactive(mut self, interactive: bool) -> Self {
        self.interactive = interactive;
        self
    }

    fn phase(&self, icon: &str, label: &str, detail: &str) {
        if self.interactive {
            crate::ui::term::phase(icon, label, detail);
        } else {
            info!("{icon} {label} {detail}");
        }
    }

    fn phase_warn(&self, msg: &str) {
        if self.interactive {
            crate::ui::term::warn(msg);
        } else {
            warn!("{msg}");
        }
    }

    fn phase_stop(&self, msg: &str) {
        if self.interactive {
            crate::ui::term::stop(msg);
        } else {
            warn!("{msg}");
        }
    }

    fn ensure_llm(&mut self) -> Result<()> {
        if self.llm.is_some() {
            return Ok(());
        }
        let llm = LlmClient::new(&self.settings)?;
        self.memory = Some(VectorMemory::new(&self.settings.data_dir, llm.clone())?);
        self.planner = Some(Planner::new(llm.clone()));
        self.researcher = Some(Researcher::new(llm.clone()));
        self.executor = Some(Executor::new(llm.clone(), self.settings.workspace.clone())?);
        self.reviewer = Some(Reviewer::new(llm.clone()));
        self.llm = Some(llm);
        Ok(())
    }

    pub async fn run_once(&mut self) -> Result<Option<CycleResult>> {
        self.ensure_llm()?;
        let Some(task) = self.tasks.next_pending()? else {
            info!("No pending tasks. Add one with: piya add \"title\" \"description\"");
            return Ok(None);
        };
        Ok(Some(self.run_task_cycle(task).await?))
    }

    pub async fn run_continuous(&mut self) -> Result<()> {
        info!(
            "Piya agent starting — poll {}s, min accuracy {:.2}",
            self.settings.poll_interval_sec, self.settings.min_accuracy
        );
        let mut cycles = 0u64;

        loop {
            if let Some(result) = self.run_once().await? {
                cycles += 1;
                self.print_cycle_summary(&result);
                if self.settings.max_cycles > 0 && cycles >= self.settings.max_cycles {
                    info!("Max cycles reached. Stopping.");
                    break;
                }
                // Immediately pick up the next due task if any.
                continue;
            }

            // Wake for the next scheduled task, but never sleep longer than poll_interval.
            let mut sleep_secs = self.settings.poll_interval_sec;
            if let Ok(Some(until)) = self.tasks.seconds_until_next_scheduled() {
                sleep_secs = sleep_secs.min(until.max(1));
            }
            info!("Idle — next poll in {sleep_secs}s");
            tokio::time::sleep(std::time::Duration::from_secs(sleep_secs)).await;
        }
        Ok(())
    }

    async fn run_task_cycle(&mut self, task: Task) -> Result<CycleResult> {
        if crate::reminder::is_reminder_task(&task) {
            return self.complete_reminder(task);
        }

        self.tasks
            .update_status(&task.id, TaskStatus::InProgress, None, None)?;
        let attempts = self.tasks.increment_attempts(&task.id)?;
        let task = self.tasks.get(&task.id)?;

        info!("Task: {} (attempt {}/{})", task.title, attempts, self.settings.max_retries);
        crate::notify::task_started(&task.title);

        let domains = expertise::detect_domains(&task.title, &task.description);
        let domain_labels: Vec<_> = domains.iter().map(|d| d.label()).collect();
        self.phase("🎯", "DOMAINS", &domain_labels.join(", "));

        let mut plan = json!({});
        let mut research = json!({});
        let mut execution = json!({});
        let mut review = json!({});

        let planner = self.planner.as_ref().expect("planner");
        let researcher = self.researcher.as_ref().expect("researcher");
        let executor = self.executor.as_ref().expect("executor");
        let reviewer = self.reviewer.as_ref().expect("reviewer");
        let memory = self.memory.as_ref().expect("memory");

            let mut last_accuracy = 0.0_f64;
            let mut stagnant_retries = 0_u32;

            for attempt in 1..=self.settings.max_retries {
            let query = format!("{}\n{}", task.title, task.description);
            let hits = memory.recall(&query, crate::memory::RecallOpts::observe(6)).await?;
            let memory_context = VectorMemory::format_context(&hits);
            self.tasks.log_cycle(&task.id, "observe", None, Some(&memory_context), None)?;
            self.phase("👁", "OBSERVE", &format!("recalled {} memories", hits.len()));

            plan = if attempt == 1 {
                planner
                    .plan(&task.title, &task.description, &memory_context, &domains)
                    .await?
            } else {
                planner
                    .replan(
                        &task.title,
                        &task.description,
                        &memory_context,
                        &review,
                        &plan,
                        &domains,
                    )
                    .await?
            };
            self.tasks.log_cycle(
                &task.id,
                "plan",
                None,
                Some(&serde_json::to_string_pretty(&plan)?),
                None,
            )?;
            self.phase(
                "📋",
                "PLAN",
                plan.get("goal").and_then(|v| v.as_str()).unwrap_or(""),
            );

            let queries: Vec<String> = plan
                .get("research_queries")
                .and_then(|v| v.as_array())
                .map(|arr| {
                    arr.iter()
                        .filter_map(|v| v.as_str().map(str::to_string))
                        .collect()
                })
                .unwrap_or_default();

            let research_context = if queries.is_empty() {
                research = json!({"summary": "No external research needed.", "confidence": 1.0});
                self.phase("🔍", "PURSUE", "skipped (no queries)");
                Researcher::format_for_prompt(&research)
            } else {
                research = researcher.research(&task.description, &queries, &domains).await?;
                let ctx = Researcher::format_for_prompt(&research);
                self.tasks.log_cycle(&task.id, "pursue", None, Some(&ctx), None)?;
                self.phase(
                    "🔍",
                    "PURSUE",
                    &format!(
                        "confidence={}",
                        research.get("confidence").map(|v| v.to_string()).unwrap_or_else(|| "?".into())
                    ),
                );
                ctx
            };

            execution = executor
                .execute(&task.title, &plan, &research_context, &memory_context, &domains)
                .await?;
            let act_log = serde_json::to_string_pretty(&execution)?;
            self.tasks.log_cycle(
                &task.id,
                "act",
                None,
                Some(&act_log.chars().take(8000).collect::<String>()),
                None,
            )?;
            self.phase(
                "⚡",
                "ACT",
                execution
                    .get("summary")
                    .and_then(|v| v.as_str())
                    .unwrap_or(""),
            );

            review = reviewer
                .review(&task.title, &plan, &execution, &research_context, &domains)
                .await?;
            let accuracy = review.get("accuracy").and_then(|v| v.as_f64()).unwrap_or(0.0);
            let passed = review.get("passed").and_then(|v| v.as_bool()).unwrap_or(false);
            self.tasks.log_cycle(
                &task.id,
                "review",
                None,
                Some(&serde_json::to_string_pretty(&review)?),
                Some(accuracy),
            )?;
            self.phase(
                "✅",
                "REVIEW",
                &format!("accuracy={accuracy:.2} passed={passed}"),
            );

            self.store_learnings(&task, &plan, &research, &execution, &review, accuracy)
                .await?;
            self.tasks.log_cycle(
                &task.id,
                "remember",
                None,
                Some(&format!("accuracy={accuracy:.2}")),
                None,
            )?;

            if passed || accuracy >= self.settings.min_accuracy {
                break;
            }

            let research_confidence = research
                .get("confidence")
                .and_then(|v| v.as_f64())
                .unwrap_or(1.0);
            let research_empty = research
                .get("key_facts")
                .and_then(|v| v.as_array())
                .map(|a| a.is_empty())
                .unwrap_or(true);

            // Research is broken or unavailable — retrying won't help.
            if research_confidence <= 0.25 && research_empty && !queries.is_empty() {
                self.phase_stop("Stopping retries: web research returned no usable results");
                break;
            }

            // Accuracy plateaued — avoid spinning on the same score.
            if attempt > 1 && accuracy <= last_accuracy + 0.02 {
                stagnant_retries += 1;
                if stagnant_retries >= 2 {
                    self.phase_stop(&format!(
                        "Stopping retries: accuracy stuck at {:.2} (no improvement)",
                        accuracy
                    ));
                    break;
                }
            } else {
                stagnant_retries = 0;
            }
            last_accuracy = accuracy;

            if !review.get("should_retry").and_then(|v| v.as_bool()).unwrap_or(false) {
                break;
            }
            self.phase_warn(&format!(
                "Retrying with improved plan… (attempt {attempt}/{})",
                self.settings.max_retries
            ));
        }

        let accuracy = review.get("accuracy").and_then(|v| v.as_f64()).unwrap_or(0.0);
        let passed =
            accuracy >= self.settings.min_accuracy || review.get("passed").and_then(|v| v.as_bool()).unwrap_or(false);

        let (status, result_text) = if passed {
            (
                TaskStatus::Done,
                execution
                    .get("summary")
                    .and_then(|v| v.as_str())
                    .unwrap_or("Completed")
                    .to_string(),
            )
        } else if attempts >= self.settings.max_retries as i32 {
            (
                TaskStatus::Failed,
                format!(
                    "Failed after {attempts} attempts. Last gaps: {:?}",
                    review.get("gaps")
                ),
            )
        } else {
            (
                TaskStatus::Pending,
                format!("Needs retry (accuracy {accuracy:.2})"),
            )
        };

        let updated = self
            .tasks
            .update_status(&task.id, status, Some(&result_text), Some(accuracy))?;

        if matches!(updated.status, TaskStatus::Done | TaskStatus::Failed)
            && updated.recurrence.is_some()
        {
            let _ = self.tasks.reschedule_if_recurring(&task.id)?;
        }

        Ok(CycleResult {
            task: updated,
            accuracy,
            passed,
            attempts,
            reminder_body: None,
        })
    }

    /// Deliver a timed reminder as a Notification Center banner — no LLM cycle.
    fn complete_reminder(&mut self, task: Task) -> Result<CycleResult> {
        let body = crate::reminder::reminder_body(&task);
        self.tasks
            .update_status(&task.id, TaskStatus::InProgress, None, None)?;
        let attempts = self.tasks.increment_attempts(&task.id)?;
        info!("Reminder: {} → notify now", task.title);
        self.phase("🔔", "REMIND", &body);

        let updated = self.tasks.update_status(
            &task.id,
            TaskStatus::Done,
            Some(&format!("Reminder sent: {body}")),
            Some(1.0),
        )?;

        if updated.recurrence.is_some() {
            let _ = self.tasks.reschedule_if_recurring(&task.id)?;
        }

        Ok(CycleResult {
            task: updated,
            accuracy: 1.0,
            passed: true,
            attempts,
            reminder_body: Some(body),
        })
    }

    async fn store_learnings(
        &self,
        task: &Task,
        plan: &Value,
        research: &Value,
        execution: &Value,
        review: &Value,
        accuracy: f64,
    ) -> Result<()> {
        let memory = self.memory.as_ref().expect("memory");
        let mut tags: Vec<String> = if task.tags.is_empty() {
            vec![]
        } else {
            task.tags.split(',').map(|s| s.trim().to_string()).collect()
        };
        for d in expertise::detect_domains(&task.title, &task.description) {
            let t = d.as_str().to_string();
            if !tags.contains(&t) {
                tags.push(t);
            }
        }

        if let Some(items) = review.get("learnings").and_then(|v| v.as_array()) {
            for item in items {
                let Some(text) = item.get("text").and_then(|v| v.as_str()) else {
                    continue;
                };
                let kind = MemoryKind::parse(
                    item.get("kind").and_then(|v| v.as_str()).unwrap_or("learning"),
                );
                memory
                    .remember(text, kind, Some(&task.id), &tags, accuracy)
                    .await?;
            }
        }

        let outcome = format!(
            "Task '{}' — accuracy {accuracy:.2}. Goal: {}. Result: {}. Gaps: {:?}",
            task.title,
            plan.get("goal").and_then(|v| v.as_str()).unwrap_or(""),
            execution.get("summary").and_then(|v| v.as_str()).unwrap_or(""),
            review.get("gaps")
        );
        memory
            .remember(
                &outcome,
                MemoryKind::Outcome,
                Some(&task.id),
                &[],
                accuracy,
            )
            .await?;

        if let Some(facts) = research.get("key_facts").and_then(|v| v.as_array()) {
            let joined: Vec<&str> = facts
                .iter()
                .filter_map(|v| v.as_str())
                .take(5)
                .collect();
            if !joined.is_empty() {
                let text = format!("Research for '{}': {}", task.title, joined.join("; "));
                let conf = research
                    .get("confidence")
                    .and_then(|v| v.as_f64())
                    .unwrap_or(0.5);
                memory
                    .remember(&text, MemoryKind::Research, Some(&task.id), &[], conf)
                    .await?;
            }
        }

        if let Some(corrections) = review.get("corrections").and_then(|v| v.as_array()) {
            for correction in corrections {
                if let Some(text) = correction.as_str() {
                    memory
                        .remember(
                            text,
                            MemoryKind::Correction,
                            Some(&task.id),
                            &[],
                            (accuracy - 0.1).max(0.3),
                        )
                        .await?;
                }
            }
        }

        Ok(())
    }

    pub fn print_cycle_summary(&self, result: &CycleResult) {
        if let Some(body) = &result.reminder_body {
            crate::notify::user_alert("Piya", body);
            if self.interactive {
                crate::ui::term::box_result(
                    "Reminder sent",
                    &[
                        ("Task", result.task.title.clone()),
                        ("Message", body.clone()),
                    ],
                );
            } else {
                info!("Reminder sent — task={} body={}", result.task.title, body);
            }
            return;
        }

        let status = result.task.status.as_str();
        crate::notify::task_finished(&result.task.title, status, result.accuracy);

        if self.interactive {
            crate::ui::term::box_result(
                "Cycle complete",
                &[
                    ("Task", result.task.title.clone()),
                    ("Status", status.to_string()),
                    ("Accuracy", format!("{:.0}%", result.accuracy * 100.0)),
                    ("Attempts", result.attempts.to_string()),
                ],
            );
        } else {
            info!(
                "Cycle complete — task={} status={} accuracy={:.2} attempts={}",
                result.task.title, status, result.accuracy, result.attempts
            );
        }
    }

    pub fn status_report(&mut self) -> Result<crate::output::StatusReport> {
        let pending = self.tasks.list(Some(TaskStatus::Pending), 50)?;
        let in_progress = self.tasks.list(Some(TaskStatus::InProgress), 50)?;
        let done = self.tasks.list(Some(TaskStatus::Done), 10)?;
        let failed = self.tasks.list(Some(TaskStatus::Failed), 10)?;

        let mem_count = self
            .memory
            .as_ref()
            .map(|m| m.stats())
            .transpose()?;

        Ok(crate::output::StatusReport {
            pending: pending.len(),
            in_progress: in_progress.len(),
            done: done.len(),
            failed: failed.len(),
            vector_memories: mem_count,
            workspace: self.settings.workspace.display().to_string(),
            next_tasks: pending.into_iter().take(5).collect(),
        })
    }

    pub fn show_status(&mut self) -> Result<()> {
        let report = self.status_report()?;
        let mem_count = report
            .vector_memories
            .map(|c| c.to_string())
            .unwrap_or_else(|| "—".into());

        println!("Piya Status");
        println!("  Pending tasks:      {}", report.pending);
        println!("  In progress:        {}", report.in_progress);
        println!("  Completed (recent): {}", report.done);
        println!("  Failed (recent):    {}", report.failed);
        println!("  Vector memories:    {mem_count}");
        println!("  Workspace:          {}", report.workspace);

        if !report.next_tasks.is_empty() {
            println!("\nNext up:");
            for task in &report.next_tasks {
                println!("  • [{}] {}", task.priority, task.title);
            }
        }
        Ok(())
    }

    pub fn tasks(&self) -> &TaskQueue {
        &self.tasks
    }

    pub fn memory_mut(&mut self) -> Result<&mut VectorMemory> {
        self.ensure_llm()?;
        Ok(self.memory.as_mut().expect("memory"))
    }
}
