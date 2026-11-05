use anyhow::Result;
use colored::Colorize;
use inquire::{Confirm, Select, Text};

use crate::agent::expertise;
use crate::agent::loop_::AgentLoop;
use crate::config::Settings;
use crate::tasks::TaskStatus;
use crate::ui::term;

fn merge_tags(user_tags: &str, title: &str, description: &str) -> String {
    let mut tags: Vec<String> = user_tags
        .split(',')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();
    for d in expertise::detect_domains(title, description) {
        let t = d.as_str().to_string();
        if !tags.contains(&t) {
            tags.push(t);
        }
    }
    tags.join(",")
}

pub async fn run_menu(settings: Settings) -> Result<()> {
    loop {
        term::banner();
        let choice = Select::new(
            "What would you like to do?",
            vec![
                "Assign task (natural language)",
                "Add task (title + description)",
                "Run next task",
                "Run all pending tasks",
                "Watch queue (live dashboard)",
                "Menu bar badge (pending count)",
                "List tasks",
                "Status",
                "Search memory",
                "Quit",
            ],
        )
        .prompt()?;

        match choice {
            "Assign task (natural language)" => {
                let brief = Text::new("Describe the task (include workspace/ output path):")
                    .with_help_message("e.g. SEO audit for example.com → workspace/audit.md")
                    .prompt()?;
                let run_now = Confirm::new("Run immediately?")
                    .with_default(true)
                    .prompt()?;
                let agent = AgentLoop::new(settings.clone(), false)?.with_interactive(true);
                let title: String = brief.chars().take(80).collect();
                let tags = merge_tags("", &title, &brief);
                let task = agent.tasks().add(&title, &brief, 1, &tags)?;
                term::success(&format!("Queued {} — {}", &task.id[..8], task.title));
                if let Some(desc) = crate::schedule::describe_task_schedule(
                    task.run_at.as_deref(),
                    task.recurrence.as_deref(),
                ) {
                    println!("  {desc}");
                }
                let scheduled = task
                    .run_at
                    .as_deref()
                    .and_then(|raw| chrono::DateTime::parse_from_rfc3339(raw).ok())
                    .is_some_and(|dt| dt.with_timezone(&chrono::Utc) > chrono::Utc::now())
                    || task.recurrence.is_some();
                if run_now && !scheduled {
                    run_interactive_once(settings.clone()).await?;
                }
            }
            "Add task (title + description)" => {
                let title = Text::new("Title:").prompt()?;
                let description = Text::new("Description:").prompt()?;
                let agent = AgentLoop::new(settings.clone(), false)?;
                let tags = merge_tags("", &title, &description);
                let task = agent.tasks().add(&title, &description, 1, &tags)?;
                term::success(&format!("Added {} — {}", &task.id[..8], task.title));
            }
            "Run next task" => run_interactive_once(settings.clone()).await?,
            "Run all pending tasks" => run_interactive_all(settings.clone()).await?,
            "Watch queue (live dashboard)" => {
                crate::ui::watch::run(settings.clone())?;
            }
            "Menu bar badge (pending count)" => {
                println!("Starting menu bar badge… (Ctrl+C in terminal to detach is not supported — run `piya badge` in background or use --install)");
                crate::ui::badge::run(settings.clone())?;
            }
            "List tasks" => {
                let agent = AgentLoop::new(settings.clone(), false)?;
                let pending = agent.tasks().list(Some(TaskStatus::Pending), 20)?;
                let done = agent.tasks().list(Some(TaskStatus::Done), 5)?;
                println!("\n{}", "Pending".bright_magenta().bold());
                if pending.is_empty() {
                    println!("  (none)");
                } else {
                    for t in &pending {
                        println!("  [{}] {} — {}", t.priority, &t.id[..8], t.title);
                    }
                }
                println!("\n{}", "Recent done".bright_green().bold());
                for t in &done {
                    println!("  {:.0}% {} — {}", t.best_accuracy * 100.0, &t.id[..8], t.title);
                }
                wait_enter()?;
            }
            "Status" => {
                let mut agent = AgentLoop::new(settings.clone(), false)?;
                agent.show_status()?;
                wait_enter()?;
            }
            "Search memory" => {
                let query = Text::new("Search query:").prompt()?;
                let mut agent = AgentLoop::new(settings.clone(), true)?.with_interactive(true);
                let memory = agent.memory_mut()?;
                let hits = memory.recall(&query, crate::memory::RecallOpts::search(5)).await?;
                if hits.is_empty() {
                    println!("No memories found.");
                } else {
                    for (i, hit) in hits.iter().enumerate() {
                        println!(
                            "\n{}. [{}] {:.0}% — {}",
                            i + 1,
                            hit.kind.as_str(),
                            hit.score * 100.0,
                            hit.text
                        );
                    }
                }
                wait_enter()?;
            }
            _ => break,
        }
    }
    Ok(())
}

async fn run_interactive_once(settings: Settings) -> Result<()> {
    let mut agent = AgentLoop::new(settings, true)?.with_interactive(true);
    if let Some(result) = agent.run_once().await? {
        agent.print_cycle_summary(&result);
    } else {
        println!("No pending tasks.");
    }
    wait_enter()?;
    Ok(())
}

async fn run_interactive_all(settings: Settings) -> Result<()> {
    let mut agent = AgentLoop::new(settings, true)?.with_interactive(true);
    let mut count = 0;
    while let Some(result) = agent.run_once().await? {
        count += 1;
        agent.print_cycle_summary(&result);
    }
    if count == 0 {
        println!("No pending tasks.");
    } else {
        term::success(&format!("Completed {count} task(s)"));
    }
    wait_enter()?;
    Ok(())
}

fn wait_enter() -> Result<()> {
    let _ = Text::new("Press Enter to continue…")
        .with_default("")
        .prompt()?;
    Ok(())
}
