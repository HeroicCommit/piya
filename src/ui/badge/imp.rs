use std::cell::RefCell;
use std::io::IsTerminal;
use std::path::PathBuf;
use std::process::Command;
use std::rc::Rc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use tao::event::{Event, StartCause};
use tao::event_loop::{ControlFlow, EventLoopBuilder, EventLoopProxy};
use tao::platform::macos::{ActivationPolicy, EventLoopExtMacOS};
use tao::platform::run_return::EventLoopExtRunReturn;
use tray_icon::menu::{AboutMetadata, Menu, MenuEvent, MenuId, MenuItem, PredefinedMenuItem};
use tray_icon::{Icon, TrayIcon, TrayIconBuilder, TrayIconEvent};

use crate::config::Settings;
use crate::tasks::{TaskQueue, TaskStatus};
use crate::ui::chat;
use crate::ui::macos;

const TICK: Duration = Duration::from_millis(500);

enum UserEvent {
    Tray(()),
    Menu(MenuEvent),
    OpenChat,
    OpenMcpSettings,
}

struct BadgeState {
    settings: Settings,
    queue: TaskQueue,
    tray: Option<TrayIcon>,
    assign_id: MenuId,
    run_next_id: MenuId,
    run_all_id: MenuId,
    queue_id: MenuId,
    workspace_id: MenuId,
    project_id: MenuId,
    mcp_settings_id: MenuId,
    quit_id: MenuId,
    flash_until: Option<Instant>,
    flash_title: String,
    last_signal_mtime: Option<std::time::SystemTime>,
    last_notified: Option<String>,
    proxy: EventLoopProxy<UserEvent>,
    _lock: LockGuard,
}

pub fn run(settings: Settings) -> Result<()> {
    let lock_path = lock_file(&settings);
    if let Some(existing) = read_lock_pid(&lock_path) {
        if crate::process_util::is_alive(existing) {
            anyhow::bail!("Piya is already running (pid {existing}). Quit from the menu bar first.");
        }
        let _ = std::fs::remove_file(&lock_path);
    }

    std::fs::write(&lock_path, std::process::id().to_string())?;
    let lock = LockGuard(lock_path);
    terminate_other_menubar_instances(std::process::id());

    let queue = TaskQueue::new(settings.data_dir.join("piya.db"))?;
    let assign_id = MenuId::new("assign");
    let run_next_id = MenuId::new("run-next");
    let run_all_id = MenuId::new("run-all");
    let queue_id = MenuId::new("queue");
    let workspace_id = MenuId::new("workspace");
    let project_id = MenuId::new("project");
    let mcp_settings_id = MenuId::new("mcp-settings");
    let quit_id = MenuId::new("quit");

    let tray_menu = Menu::new();
    tray_menu.append(&MenuItem::with_id(
        assign_id.clone(),
        "Chat with Piya…",
        true,
        None,
    ))?;
    tray_menu.append(&MenuItem::with_id(
        mcp_settings_id.clone(),
        "MCP Settings…",
        true,
        None,
    ))?;
    tray_menu.append(&PredefinedMenuItem::separator())?;
    tray_menu.append(&MenuItem::with_id(
        run_next_id.clone(),
        "Run Next Task",
        true,
        None,
    ))?;
    tray_menu.append(&MenuItem::with_id(
        run_all_id.clone(),
        "Run All Pending",
        true,
        None,
    ))?;
    tray_menu.append(&PredefinedMenuItem::separator())?;
    tray_menu.append(&MenuItem::with_id(
        queue_id.clone(),
        "View Queue…",
        true,
        None,
    ))?;
    tray_menu.append(&MenuItem::with_id(
        workspace_id.clone(),
        "Open Workspace",
        true,
        None,
    ))?;
    tray_menu.append(&MenuItem::with_id(
        project_id.clone(),
        "Open Project Folder",
        true,
        None,
    ))?;
    tray_menu.append(&PredefinedMenuItem::separator())?;
    tray_menu.append(&PredefinedMenuItem::about(
        Some("Piya".into()),
        Some(AboutMetadata {
            name: Some("Piya".into()),
            version: Some(env!("CARGO_PKG_VERSION").into()),
            copyright: Some("SEO · Dev · Security · PM".into()),
            ..Default::default()
        }),
    ))?;
    tray_menu.append(&PredefinedMenuItem::separator())?;
    tray_menu.append(&MenuItem::with_id(quit_id.clone(), "Quit Piya", true, None))?;

    let mut event_loop = EventLoopBuilder::<UserEvent>::with_user_event().build();
    event_loop.set_activation_policy(ActivationPolicy::Accessory);

    let tray_proxy = event_loop.create_proxy();
    let menu_proxy = event_loop.create_proxy();
    let state_proxy = event_loop.create_proxy();

    let state = Rc::new(RefCell::new(BadgeState {
        settings: settings.clone(),
        queue,
        tray: None,
        assign_id,
        run_next_id,
        run_all_id,
        queue_id,
        workspace_id,
        project_id,
        mcp_settings_id,
        quit_id,
        flash_until: None,
        flash_title: String::new(),
        last_signal_mtime: None,
        last_notified: crate::notify::load_last_notified(),
        proxy: state_proxy,
        _lock: lock,
    }));

    TrayIconEvent::set_event_handler(Some(move |_event| {
        let _ = tray_proxy.send_event(UserEvent::Tray(()));
    }));

    MenuEvent::set_event_handler(Some(move |event| {
        let _ = menu_proxy.send_event(UserEvent::Menu(event));
    }));

    let state_for_loop = Rc::clone(&state);
    let mut next_tick = Instant::now() + TICK;
    let mut exiting = false;

    log_status("Piya is running in the menu bar.");

    event_loop.run_return(move |event, _elwt, control_flow| {
        if exiting {
            *control_flow = ControlFlow::Exit;
            return;
        }
        *control_flow = ControlFlow::WaitUntil(next_tick);

        match event {
            Event::NewEvents(StartCause::Init) => {
                crate::notify::request_permission();

                let icon = piya_icon();
                let tray = TrayIconBuilder::new()
                    .with_menu(Box::new(tray_menu.clone()))
                    .with_icon(icon)
                    .with_icon_as_template(false)
                    .with_title("0")
                    .with_tooltip("Piya")
                    .build()
                    .expect("create menu bar icon");

                wake_macos_runloop();

                let mut s = state_for_loop.borrow_mut();
                s.tray = Some(tray);
                let _ = s.refresh();
            }

            Event::NewEvents(StartCause::ResumeTimeReached { .. }) => {
                let mut s = state_for_loop.borrow_mut();
                let _ = s.read_signal();
                let _ = s.refresh();
                let settings = s.settings.clone();
                let queue = TaskQueue::new(settings.data_dir.join("piya.db"));
                drop(s);
                if let Ok(q) = queue {
                    let _ = crate::worker::maybe_auto_run(&settings, &q);
                }
                next_tick = Instant::now() + TICK;
            }

            Event::UserEvent(UserEvent::Menu(menu_event)) => {
                handle_menu_event(&state_for_loop, &menu_event.id, &mut exiting, control_flow);
            }

            Event::UserEvent(UserEvent::OpenChat) => {
                let s = state_for_loop.borrow();
                let settings = s.settings.clone();
                drop(s);
                if let Err(err) = chat::spawn_or_toggle(&settings) {
                    eprintln!("Piya chat failed: {err:#}");
                    macos::alert("Piya Chat", &format!("Could not open chat:\n{err:#}"));
                }
            }

            Event::UserEvent(UserEvent::OpenMcpSettings) => {
                let s = state_for_loop.borrow();
                let settings = s.settings.clone();
                drop(s);
                if let Err(err) = chat::spawn_or_open_settings(&settings) {
                    eprintln!("Piya MCP settings failed: {err:#}");
                    macos::alert("Piya MCP", &format!("Could not open settings:\n{err:#}"));
                }
            }

            Event::UserEvent(UserEvent::Tray(_)) => {}

            _ => {}
        }
    });

    Ok(())
}

fn handle_menu_event(
    state: &Rc<RefCell<BadgeState>>,
    id: &MenuId,
    exiting: &mut bool,
    control_flow: &mut ControlFlow,
) {
    let mut s = state.borrow_mut();

    if id == &s.quit_id {
        s.tray.take();
        *exiting = true;
        *control_flow = ControlFlow::Exit;
        return;
    }

    if id == &s.assign_id {
        let proxy = s.proxy.clone();
        drop(s);
        let _ = proxy.send_event(UserEvent::OpenChat);
        return;
    }

    if id == &s.mcp_settings_id {
        let proxy = s.proxy.clone();
        drop(s);
        let _ = proxy.send_event(UserEvent::OpenMcpSettings);
        return;
    }

    if id == &s.queue_id {
        let summary = format_queue_summary(&s.queue);
        drop(s);
        macos::alert("Piya Queue", &summary);
        return;
    }

    if id == &s.workspace_id {
        let path = s.settings.workspace.clone();
        drop(s);
        let _ = macos::open_path(&path);
        return;
    }

    if id == &s.project_id {
        let path = project_root(&s.settings);
        drop(s);
        let _ = macos::open_path(&path);
        return;
    }

    if id == &s.run_next_id {
        let settings = s.settings.clone();
        drop(s);
        let _ = crate::worker::spawn_worker(&settings, &["run", "--once"]);
        return;
    }

    if id == &s.run_all_id {
        let settings = s.settings.clone();
        drop(s);
        let _ = crate::worker::spawn_worker(&settings, &["run", "--continuous"]);
    }
}

fn format_queue_summary(queue: &TaskQueue) -> String {
    let pending = queue.list(Some(TaskStatus::Pending), 12).unwrap_or_default();
    let running = queue.list(Some(TaskStatus::InProgress), 3).unwrap_or_default();
    let done = queue.list(Some(TaskStatus::Done), 3).unwrap_or_default();

    let mut lines = vec![format!("{} pending", pending.len())];
    for t in &pending {
        let mut line = format!("• {}", t.title);
        if let Some(desc) =
            crate::schedule::describe_task_schedule(t.run_at.as_deref(), t.recurrence.as_deref())
        {
            line.push_str(&format!(" ({desc})"));
        }
        lines.push(line);
    }
    if !running.is_empty() {
        lines.push(String::new());
        lines.push("Running".into());
        for t in &running {
            lines.push(format!("▶ {}", t.title));
        }
    }
    if !done.is_empty() {
        lines.push(String::new());
        lines.push("Recent".into());
        for t in &done {
            lines.push(format!(
                "✓ {:.0}% {}",
                t.best_accuracy * 100.0,
                t.title
            ));
        }
    }
    if pending.is_empty() && running.is_empty() {
        lines.push(String::new());
        lines.push("No tasks yet — open Chat with Piya to get started.".into());
    }
    lines.join("\n")
}

impl BadgeState {
    fn refresh(&mut self) -> Result<()> {
        let pending = self.queue.count_due_pending()?;
        let scheduled = self.queue.count_scheduled()?;
        let running = self.queue.count(Some(TaskStatus::InProgress))?;

        let title = if let Some(until) = self.flash_until {
            if Instant::now() < until {
                self.flash_title.clone()
            } else {
                self.flash_until = None;
                badge_title(pending, scheduled, running)
            }
        } else {
            badge_title(pending, scheduled, running)
        };

        let tooltip = if scheduled > 0 {
            format!("Piya — {pending} due, {scheduled} scheduled, {running} running")
        } else if running > 0 {
            format!("Piya — {pending} due, {running} running")
        } else {
            format!("Piya — {pending} due")
        };

        if let Some(tray) = &self.tray {
            let _ = tray.set_title(Some(&title));
            let _ = tray.set_tooltip(Some(&tooltip));
        }
        Ok(())
    }

    fn read_signal(&mut self) -> Result<()> {
        let path = self.settings.data_dir.join("badge-signal.json");
        let Ok(meta) = std::fs::metadata(&path) else {
            return Ok(());
        };
        let mtime = meta.modified().ok();
        if mtime == self.last_signal_mtime {
            return Ok(());
        }
        self.last_signal_mtime = mtime;

        let Ok(raw) = std::fs::read_to_string(&path) else {
            return Ok(());
        };
        if self.last_notified.as_deref() == Some(raw.as_str()) {
            return Ok(());
        }

        let Ok(value) = serde_json::from_str::<serde_json::Value>(&raw) else {
            return Ok(());
        };

        let event = value.get("event").and_then(|v| v.as_str()).unwrap_or("");
        let title = value
            .get("title")
            .and_then(|v| v.as_str())
            .unwrap_or("Piya");
        let detail = value.get("detail").and_then(|v| v.as_str()).unwrap_or("");

        self.flash_title = match event {
            "finished" if detail.starts_with("Completed") => "✓".into(),
            "finished" if detail.starts_with("Failed") => "✗".into(),
            "started" => "▶".into(),
            _ => "!".into(),
        };
        self.flash_until = Some(Instant::now() + Duration::from_secs(3));

        // One Notification Center popup when a task finishes (not on start).
        if event == "finished" {
            let stale = crate::notify::signal_age_secs(&raw).is_some_and(|age| age > 90);
            if !stale {
                crate::notify::send_popup("Piya", title, detail);
                self.last_notified = Some(raw.clone());
                crate::notify::save_last_notified(&raw);
            }
        }

        Ok(())
    }
}

fn wake_macos_runloop() {
    use objc2_core_foundation::CFRunLoop;
    if let Some(rl) = CFRunLoop::main() {
        rl.wake_up();
    }
}

fn badge_title(pending: usize, scheduled: usize, running: usize) -> String {
    if running > 0 {
        if pending > 0 {
            format!("▶{pending}")
        } else {
            "▶".into()
        }
    } else if scheduled > 0 && pending == 0 {
        format!("⏰{scheduled}")
    } else {
        pending.to_string()
    }
}

fn piya_icon() -> Icon {
    let size = 22u32;
    let mut rgba = vec![0u8; (size * size * 4) as usize];
    let cx = size as f32 / 2.0 - 0.5;
    let cy = size as f32 / 2.0 - 0.5;
    let outer = size as f32 * 0.42;
    let inner = size as f32 * 0.22;
    for y in 0..size {
        for x in 0..size {
            let dx = x as f32 - cx;
            let dy = y as f32 - cy;
            let d = (dx * dx + dy * dy).sqrt();
            let idx = ((y * size + x) * 4) as usize;
            if d <= outer {
                let edge = ((outer - d) / 1.2).clamp(0.0, 1.0);
                let (r, g, b) = if d <= inner {
                    (249, 168, 212)
                } else {
                    (236, 72, 153)
                };
                let a = (edge * 255.0) as u8;
                rgba[idx..idx + 4].copy_from_slice(&[r, g, b, a]);
            }
        }
    }
    Icon::from_rgba(rgba, size, size).expect("icon")
}

fn project_root(settings: &Settings) -> PathBuf {
    settings
        .data_dir
        .parent()
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| settings.data_dir.clone())
}

fn log_status(msg: &str) {
    if std::io::stderr().is_terminal() {
        eprintln!("{msg}");
    }
}

fn terminate_other_menubar_instances(current_pid: u32) {
    // Only quit stray terminal badge daemons — not workers (`piya run …`).
    let Ok(out) = Command::new("pgrep").args(["-f", "piya badge"]).output() else {
        return;
    };
    for line in String::from_utf8_lossy(&out.stdout).lines() {
        if let Ok(pid) = line.trim().parse::<u32>() {
            if pid != current_pid {
                let _ = Command::new("kill")
                    .arg(pid.to_string())
                    .stdout(std::process::Stdio::null())
                    .stderr(std::process::Stdio::null())
                    .status();
            }
        }
    }
}

fn lock_file(settings: &Settings) -> PathBuf {
    settings.data_dir.join("piya-badge.lock")
}

fn read_lock_pid(path: &PathBuf) -> Option<u32> {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|s| s.trim().parse().ok())
}

struct LockGuard(PathBuf);

impl Drop for LockGuard {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

pub fn install_launch_agent(settings: &Settings) -> Result<()> {
    let app = PathBuf::from("/Applications/Piya.app");
    if app.join("Contents/MacOS/piya").is_file() {
        crate::ui::app::install(settings)
    } else {
        install_raw_agent(settings)
    }
}

fn install_raw_agent(settings: &Settings) -> Result<()> {
    crate::config::write_app_support_config(settings)?;
    let exe = std::env::current_exe().context("current exe")?;
    let plist_dir = dirs_home()?.join("Library/LaunchAgents");
    std::fs::create_dir_all(&plist_dir)?;
    let plist_path = plist_dir.join(format!("{}.plist", crate::launch_agent::APP_LABEL));

    let data_dir = settings.data_dir.canonicalize().unwrap_or_else(|_| settings.data_dir.clone());
    let workspace = settings
        .workspace
        .canonicalize()
        .unwrap_or_else(|_| settings.workspace.clone());

    let plist = format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key><string>{}</string>
  <key>ProgramArguments</key>
  <array>
    <string>{}</string>
  </array>
  <key>EnvironmentVariables</key>
  <dict>
    <key>PIYA_DATA_DIR</key><string>{}</string>
    <key>PIYA_WORKSPACE</key><string>{}</string>
    <key>PIYA_NOTIFY</key><string>all</string>
  </dict>
  <key>RunAtLoad</key><true/>
  <key>KeepAlive</key><false/>
</dict>
</plist>"#,
        crate::launch_agent::APP_LABEL,
        exe.display(),
        data_dir.display(),
        workspace.display(),
    );

    std::fs::write(&plist_path, plist)?;
    crate::launch_agent::register_plist(&plist_path)?;
    println!("Piya will start at login.");
    Ok(())
}

pub fn uninstall_launch_agent(_settings: &Settings) -> Result<()> {
    crate::launch_agent::unregister_label(crate::launch_agent::APP_LABEL)?;
    crate::launch_agent::unregister_label("com.piya.badge")?;
    println!("Removed login item");
    Ok(())
}

fn dirs_home() -> Result<PathBuf> {
    std::env::var("HOME")
        .map(PathBuf::from)
        .context("HOME not set")
}