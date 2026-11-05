use std::io::{self, IsTerminal};

use anyhow::Result;
use clap::{Parser, Subcommand};
use tracing_subscriber::EnvFilter;

use piya::agent::expertise;
use piya::agent::loop_::AgentLoop;
use piya::config::Settings;
use piya::output::{print_json, MemoryHitJson};
use piya::tasks::TaskStatus;
use piya::ui;

#[derive(Parser)]
#[command(name = "piya", about = "Continuous AI agent with vector memory and OPPAR loop")]
struct Cli {
    /// Emit JSON (for MCP / OpenClaw integrations)
    #[arg(long, global = true)]
    json: bool,

    /// Colored phase output (default when stdout is a TTY)
    #[arg(long, global = true)]
    interactive: bool,

    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(Subcommand)]
enum Commands {
    /// Interactive menu (default when no subcommand)
    Menu,
    /// Live queue dashboard (TUI)
    Watch,
    /// Menu bar app (macOS) — native UI
    App {
        /// Install Piya.app to /Applications + login item
        #[arg(long)]
        install: bool,
        /// Remove Piya.app and login item
        #[arg(long)]
        uninstall: bool,
    },
    /// Menu bar badge (alias for app)
    Badge {
        /// Install LaunchAgent to start badge at login
        #[arg(long)]
        install: bool,
        /// Remove LaunchAgent
        #[arg(long)]
        uninstall: bool,
    },
    /// Chat panel window (spawned by menu bar; not for direct use)
    #[command(hide = true)]
    ChatPanel,
    /// Open MCP settings panel (macOS)
    Mcp,
    /// Run the agent loop on pending tasks
    Run {
        #[arg(long, short = 'c')]
        continuous: bool,
        #[arg(long)]
        once: bool,
    },
    /// Add a task to the queue
    Add {
        title: String,
        description: String,
        #[arg(long, short = 'p', default_value_t = 0)]
        priority: i32,
        #[arg(long, short = 't', default_value = "")]
        tags: String,
    },
    /// Add a task from a single natural-language brief (OpenClaw-friendly)
    Assign {
        brief: String,
        #[arg(long, short = 'p', default_value_t = 1)]
        priority: i32,
        #[arg(long, short = 't', default_value = "")]
        tags: String,
    },
    /// Get one task by id or id prefix
    Get { id: String },
    /// List tasks in the queue
    List {
        #[arg(long, short = 's')]
        status: Option<String>,
        #[arg(long, short = 'n', default_value_t = 20)]
        limit: i32,
    },
    /// Show agent and memory status
    Status,
    /// Compact vector memory files and prune old versions
    Optimize,
    /// Search vector memory
    Recall {
        query: String,
        #[arg(long, short = 'n', default_value_t = 5)]
        limit: usize,
    },
}

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

fn use_interactive(cli: &Cli) -> bool {
    cli.interactive || (io::stdout().is_terminal() && !cli.json)
}

fn init_tracing() -> Result<()> {
    if !std::env::args().any(|a| a == "--json") {
        tracing_subscriber::fmt()
            .with_env_filter(EnvFilter::from_default_env().add_directive("piya=info".parse()?))
            .init();
    }
    Ok(())
}

/// UI processes must not run under tokio — WKWebView + nested block_on will panic.
fn is_sync_command(cli: &Cli) -> bool {
    match &cli.command {
        #[cfg(target_os = "macos")]
        Some(Commands::ChatPanel) => true,
        #[cfg(target_os = "macos")]
        Some(Commands::Mcp) => true,
        Some(Commands::Watch) => true,
        Some(Commands::App { .. }) | Some(Commands::Badge { .. }) => true,
        None => {
            #[cfg(target_os = "macos")]
            {
                ui::app::launched_from_app_bundle()
            }
            #[cfg(not(target_os = "macos"))]
            {
                false
            }
        }
        _ => false,
    }
}

fn run_sync(cli: &Cli, settings: Settings) -> Result<()> {
    match cli.command {
        #[cfg(all(target_os = "macos", feature = "legacy-ui"))]
        Some(Commands::ChatPanel) => ui::chat::run_panel(settings),
        #[cfg(all(target_os = "macos", not(feature = "legacy-ui")))]
        Some(Commands::ChatPanel) => anyhow::bail!(
            "Legacy wry chat panel is disabled. Rebuild with `--features legacy-ui` or use PiyaSwift.app."
        ),
        #[cfg(all(target_os = "macos", feature = "legacy-ui"))]
        Some(Commands::Mcp) => {
            ui::chat::spawn_or_open_settings(&settings)?;
            println!("Opened MCP settings.");
            Ok(())
        }
        #[cfg(all(target_os = "macos", not(feature = "legacy-ui")))]
        Some(Commands::Mcp) => anyhow::bail!(
            "Legacy wry MCP settings are disabled. Rebuild with `--features legacy-ui` or use PiyaSwift.app."
        ),
        Some(Commands::Watch) => ui::watch::run(settings),
        Some(Commands::App { install, uninstall }) => {
            if uninstall {
                ui::app::uninstall(&settings)
            } else if install {
                ui::app::install(&settings)
            } else {
                ui::badge::run(settings)
            }
        }
        Some(Commands::Badge { install, uninstall }) => {
            if uninstall {
                ui::badge::uninstall_launch_agent(&settings)
            } else if install {
                ui::badge::install_launch_agent(&settings)
            } else {
                ui::badge::run(settings)
            }
        }
        None => {
            #[cfg(target_os = "macos")]
            {
                ui::badge::run(settings)
            }
            #[cfg(not(target_os = "macos"))]
            {
                unreachable!("sync routing only for macOS app bundle")
            }
        }
        _ => unreachable!("sync routing mismatch"),
    }
}

async fn run_async(cli: Cli, settings: Settings) -> Result<()> {
    let interactive = use_interactive(&cli);

    match cli.command {
        None => {
            if cli.json {
                anyhow::bail!("--json is not compatible with interactive menu");
            }
            ui::run_menu(settings).await
        }
        Some(Commands::Menu) => {
            if cli.json {
                anyhow::bail!("--json is not compatible with interactive menu");
            }
            ui::run_menu(settings).await
        }
        Some(Commands::Run { continuous, once }) => {
            let mut agent = AgentLoop::new(settings, true)?.with_interactive(interactive);
            if continuous {
                agent.run_continuous().await?;
            } else if once {
                if let Some(result) = agent.run_once().await? {
                    if cli.json {
                        print_json(&result.task)?;
                    } else {
                        agent.print_cycle_summary(&result);
                    }
                }
            } else {
                while let Some(result) = agent.run_once().await? {
                    if cli.json {
                        print_json(&result.task)?;
                    } else {
                        agent.print_cycle_summary(&result);
                    }
                }
            }
            Ok(())
        }
        Some(Commands::Add {
            title,
            description,
            priority,
            tags,
        }) => {
            let agent = AgentLoop::new(settings, false)?;
            let tags = merge_tags(&tags, &title, &description);
            let task = agent.tasks().add(&title, &description, priority, &tags)?;
            if cli.json {
                print_json(&task)?;
            } else {
                println!("Added task {}… — {}", &task.id[..8], task.title);
            }
            Ok(())
        }
        Some(Commands::Assign {
            brief,
            priority,
            tags,
        }) => {
            let agent = AgentLoop::new(settings, false)?;
            let title: String = brief.chars().take(80).collect();
            let tags = merge_tags(&tags, &title, &brief);
            let task = agent.tasks().add(&title, &brief, priority, &tags)?;
            if cli.json {
                print_json(&task)?;
            } else {
                println!("Assigned task {}… — {}", &task.id[..8], task.title);
            }
            Ok(())
        }
        Some(Commands::Get { id }) => {
            let agent = AgentLoop::new(settings, false)?;
            let task = agent.tasks().get(&id)?;
            if cli.json {
                print_json(&task)?;
            } else {
                println!("{:<12} {}", "ID", task.id);
                println!("{:<12} {}", "STATUS", task.status.as_str());
                println!("{:<12} {}", "TITLE", task.title);
                println!("{:<12} {}", "ACCURACY", task.best_accuracy);
                if let Some(result) = &task.result {
                    println!("{:<12} {}", "RESULT", result);
                }
                if let Some(desc) = piya::schedule::describe_task_schedule(
                    task.run_at.as_deref(),
                    task.recurrence.as_deref(),
                ) {
                    println!("{:<12} {}", "SCHEDULE", desc);
                }
            }
            Ok(())
        }
        Some(Commands::List { status, limit }) => {
            let agent = AgentLoop::new(settings, false)?;
            let filter = match status {
                Some(s) => Some(TaskStatus::parse(&s)?),
                None => None,
            };
            let tasks = agent.tasks().list(filter, limit)?;
            if cli.json {
                print_json(&tasks)?;
            } else {
                println!("{:<10} {:<12} {:>3} {:>5}  {}", "ID", "STATUS", "PRI", "ACC", "TITLE");
                for task in tasks {
                    println!(
                        "{:<10} {:<12} {:>3} {:>5.2}  {}",
                        &task.id[..8],
                        task.status.as_str(),
                        task.priority,
                        task.best_accuracy,
                        task.title
                    );
                }
            }
            Ok(())
        }
        Some(Commands::Status) => {
            let mut agent = AgentLoop::new(settings, false)?;
            if cli.json {
                print_json(&agent.status_report()?)?;
            } else {
                agent.show_status()?;
            }
            Ok(())
        }
        Some(Commands::Optimize) => {
            let mut agent = AgentLoop::new(settings, true)?;
            let memory = agent.memory_mut()?;
            memory.optimize().await?;
            println!("Memory optimized.");
            Ok(())
        }
        Some(Commands::Recall { query, limit }) => {
            let mut agent = AgentLoop::new(settings, true)?;
            let memory = agent.memory_mut()?;
            let hits = memory.recall(&query, piya::memory::RecallOpts::search(limit)).await?;
            if cli.json {
                let json_hits: Vec<MemoryHitJson> = hits.iter().map(MemoryHitJson::from).collect();
                print_json(&json_hits)?;
            } else if hits.is_empty() {
                println!("No memories found.");
            } else {
                for (i, hit) in hits.iter().enumerate() {
                    println!(
                        "{}. [{}] score={:.3} acc={:.2}\n  {}\n",
                        i + 1,
                        hit.kind.as_str(),
                        hit.score,
                        hit.accuracy,
                        hit.text
                    );
                }
            }
            Ok(())
        }
        _ => unreachable!("async routing mismatch"),
    }
}

fn main() -> Result<()> {
    init_tracing()?;
    let cli = Cli::parse();
    let settings = Settings::load()?;

    if is_sync_command(&cli) {
        run_sync(&cli, settings)
    } else {
        tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .thread_name("piya-cli")
            .build()?
            .block_on(run_async(cli, settings))
    }
}
