//! Standalone chat window process (separate from the menu bar event loop).

use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use tao::event::{Event, StartCause};
use tao::event_loop::{ControlFlow, EventLoopBuilder};
use tao::platform::run_return::EventLoopExtRunReturn;
#[cfg(target_os = "macos")]
use tao::platform::macos::{ActivationPolicy, EventLoopExtMacOS};

use crate::config::Settings;
use crate::tasks::TaskQueue;
use crate::ui::macos;

use super::ChatWindow;

const TICK: Duration = Duration::from_millis(500);

enum PanelEvent {
    Ipc(String),
    ApplyGlass,
    SendComplete(super::SendComplete),
}

struct ChatLock(PathBuf);

impl ChatLock {
    fn try_acquire(data_dir: &PathBuf) -> Result<Option<Self>> {
        let path = data_dir.join("piya-chat.lock");
        for _ in 0..40 {
            if let Ok(raw) = std::fs::read_to_string(&path) {
                if let Ok(pid) = raw.trim().parse::<u32>() {
                    if crate::process_util::is_alive(pid) {
                        return Ok(None);
                    }
                }
                let _ = std::fs::remove_file(&path);
            }
            std::fs::write(&path, std::process::id().to_string())?;
            // Confirm we own the lock (handles spawn races).
            if std::fs::read_to_string(&path)
                .ok()
                .and_then(|s| s.trim().parse::<u32>().ok())
                == Some(std::process::id())
            {
                return Ok(Some(Self(path)));
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        Ok(None)
    }
}

impl Drop for ChatLock {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

pub fn spawn_or_toggle(settings: &Settings) -> Result<()> {
    spawn_panel(settings, false)
}

pub fn spawn_or_open_settings(settings: &Settings) -> Result<()> {
    if signal_running_panel(settings, "open_settings")? {
        return Ok(());
    }
    spawn_panel(settings, true)
}

fn signal_running_panel(settings: &Settings, command: &str) -> Result<bool> {
    let lock = settings.data_dir.join("piya-chat.lock");
    let Ok(raw) = std::fs::read_to_string(&lock) else {
        return Ok(false);
    };
    let Ok(pid) = raw.trim().parse::<u32>() else {
        return Ok(false);
    };
    if !crate::process_util::is_alive(pid) {
        return Ok(false);
    }

    let cmd_path = settings.data_dir.join("piya-chat-cmd.json");
    let body = serde_json::json!({ "type": command });
    std::fs::write(&cmd_path, serde_json::to_string(&body)?)?;
    Ok(true)
}

fn spawn_panel(settings: &Settings, open_settings: bool) -> Result<()> {
    let lock = settings.data_dir.join("piya-chat.lock");
    if let Ok(raw) = std::fs::read_to_string(&lock) {
        if let Ok(pid) = raw.trim().parse::<u32>() {
            if crate::process_util::is_alive(pid) {
                if open_settings {
                    signal_running_panel(settings, "open_settings")?;
                    return Ok(());
                }
                let _ = std::process::Command::new("kill")
                    .arg(pid.to_string())
                    .status();
                return Ok(());
            }
        }
        let _ = std::fs::remove_file(&lock);
    }

    wait_for_chat_exit(&lock);

    let exe = std::env::current_exe().context("locate piya binary")?;
    let mut cmd = std::process::Command::new(exe);
    cmd.arg("chat-panel");
    if open_settings {
        cmd.env("PIYA_OPEN_SETTINGS", "1");
    }
    cmd.spawn().context("spawn chat panel")?;
    Ok(())
}

pub fn run_panel(settings: Settings) -> Result<()> {
    let Some(_lock) = ChatLock::try_acquire(&settings.data_dir)? else {
        return Ok(());
    };
    let queue = TaskQueue::new(settings.data_dir.join("piya.db"))?;

    macos::activate_app();

    let mut event_loop = EventLoopBuilder::<PanelEvent>::with_user_event().build();
    #[cfg(target_os = "macos")]
    event_loop.set_activation_policy(ActivationPolicy::Regular);

    let proxy = event_loop.create_proxy();
    let chat = Rc::new(RefCell::new(ChatWindow::new(
        &settings.data_dir,
        open_settings_from_env(),
    )));
    let settings = Rc::new(settings);
    let queue = Rc::new(queue);
    let mut opened = false;
    let mut glass_applied = false;
    let mut next_tick = Instant::now() + TICK;

    event_loop.run_return(move |event, elwt, control_flow| {
        *control_flow = ControlFlow::WaitUntil(next_tick);

        match event {
            Event::NewEvents(StartCause::Init) => {
                let ipc_proxy = proxy.clone();
                if chat
                    .borrow_mut()
                    .open(elwt, &settings, &queue, move |body| {
                        let _ = ipc_proxy.send_event(PanelEvent::Ipc(body));
                    })
                    .is_err()
                {
                    *control_flow = ControlFlow::Exit;
                    return;
                }
                opened = true;
                let _ = proxy.send_event(PanelEvent::ApplyGlass);
            }

            Event::NewEvents(StartCause::ResumeTimeReached { .. }) => {
                if opened {
                    chat.borrow().keep_on_top();
                    let _ = chat.borrow_mut().drain_commands(&settings.data_dir, &settings);
                }
                next_tick = Instant::now() + TICK;
            }

            Event::UserEvent(PanelEvent::ApplyGlass) => {
                if opened && !glass_applied {
                    glass_applied = chat.borrow().apply_glass();
                }
            }

            Event::UserEvent(PanelEvent::Ipc(body)) => {
                match chat
                    .borrow_mut()
                    .handle_ipc(&body, &settings, &queue)
                {
                    Ok(super::IpcDispatch::ShouldClose) => {
                        *control_flow = ControlFlow::Exit;
                    }
                    Ok(super::IpcDispatch::SpawnSend(req)) => {
                        let proxy = proxy.clone();
                        std::thread::spawn(move || {
                            let result = req.run();
                            let _ = proxy.send_event(PanelEvent::SendComplete(result));
                        });
                    }
                    Ok(super::IpcDispatch::Done) => {
                        if !chat.borrow().is_open() {
                            *control_flow = ControlFlow::Exit;
                        }
                    }
                    Err(_) => {}
                }
            }

            Event::UserEvent(PanelEvent::SendComplete(result)) => {
                let _ = chat.borrow_mut().complete_send(result);
            }

            Event::WindowEvent { window_id, event, .. } => {
                if chat.borrow().owns_window(window_id) {
                    if chat.borrow_mut().handle_window_event(&event) {
                        *control_flow = ControlFlow::Exit;
                    }
                }
            }

            Event::LoopDestroyed => {}

            _ => {}
        }
    });

    Ok(())
}

fn open_settings_from_env() -> bool {
    matches!(
        std::env::var("PIYA_OPEN_SETTINGS").as_deref(),
        Ok("1") | Ok("true") | Ok("TRUE") | Ok("yes") | Ok("YES")
    )
}

fn wait_for_chat_exit(lock: &PathBuf) {
    let deadline = Instant::now() + Duration::from_secs(2);
    while Instant::now() < deadline {
        let blocked = std::fs::read_to_string(lock)
            .ok()
            .and_then(|raw| raw.trim().parse::<u32>().ok())
            .is_some_and(crate::process_util::is_alive);
        if !blocked {
            let _ = std::fs::remove_file(lock);
            return;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}
