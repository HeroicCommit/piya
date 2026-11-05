use std::io;
use std::time::{Duration, Instant};

use anyhow::Result;
use crossterm::event::{self, Event, KeyCode, KeyModifiers};
use crossterm::terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen};
use crossterm::ExecutableCommand;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem, Paragraph};
use ratatui::DefaultTerminal;

use crate::config::Settings;
use crate::tasks::{TaskQueue, TaskStatus};

struct Dashboard {
    pending: usize,
    in_progress: usize,
    done: usize,
    failed: usize,
    pending_titles: Vec<String>,
    recent_done: Vec<String>,
    last_refresh: Instant,
    message: String,
}

impl Dashboard {
    fn refresh(&mut self, queue: &TaskQueue) -> Result<()> {
        let pending = queue.list(Some(TaskStatus::Pending), 10)?;
        let in_progress = queue.list(Some(TaskStatus::InProgress), 5)?;
        let done = queue.list(Some(TaskStatus::Done), 5)?;
        let failed = queue.list(Some(TaskStatus::Failed), 5)?;

        self.pending = pending.len();
        self.in_progress = in_progress.len();
        self.done = done.len();
        self.failed = failed.len();
        self.pending_titles = pending
            .iter()
            .map(|t| format!("[{}] {}", t.priority, t.title))
            .collect();
        self.recent_done = done
            .iter()
            .map(|t| format!("{:.0}% {}", t.best_accuracy * 100.0, t.title))
            .collect();
        self.last_refresh = Instant::now();
        Ok(())
    }
}

pub fn run(settings: Settings) -> Result<()> {
    let queue = TaskQueue::new(settings.data_dir.join("piya.db"))?;
    let mut dash = Dashboard {
        pending: 0,
        in_progress: 0,
        done: 0,
        failed: 0,
        pending_titles: vec![],
        recent_done: vec![],
        last_refresh: Instant::now(),
        message: "q quit · r run next · R run all pending".into(),
    };
    dash.refresh(&queue)?;

    enable_raw_mode()?;
    io::stdout().execute(EnterAlternateScreen)?;
    let mut terminal = ratatui::init();

    let result = run_loop(&mut terminal, &queue, &mut dash, &settings);

    ratatui::restore();
    disable_raw_mode()?;
    io::stdout().execute(LeaveAlternateScreen)?;
    result
}

fn run_loop(
    terminal: &mut DefaultTerminal,
    queue: &TaskQueue,
    dash: &mut Dashboard,
    settings: &Settings,
) -> Result<()> {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;

    loop {
        terminal.draw(|f| ui(f, dash))?;

        if event::poll(Duration::from_millis(400))? {
            if let Event::Key(key) = event::read()? {
                match key.code {
                    KeyCode::Char('q') | KeyCode::Esc => break,
                    KeyCode::Char('r') if key.modifiers.contains(KeyModifiers::SHIFT) => {
                        dash.message = "Running all pending…".into();
                        terminal.draw(|f| ui(f, dash))?;
                        let s = settings.clone();
                        rt.block_on(async {
                            let mut agent = crate::agent::loop_::AgentLoop::new(s, true)?
                                .with_interactive(false);
                            while agent.run_once().await?.is_some() {}
                            Ok::<(), anyhow::Error>(())
                        })?;
                        dash.message = "All pending processed".into();
                        dash.refresh(queue)?;
                    }
                    KeyCode::Char('r') => {
                        dash.message = "Running next task…".into();
                        terminal.draw(|f| ui(f, dash))?;
                        let s = settings.clone();
                        rt.block_on(async {
                            let mut agent = crate::agent::loop_::AgentLoop::new(s, true)?
                                .with_interactive(false);
                            let _ = agent.run_once().await?;
                            Ok::<(), anyhow::Error>(())
                        })?;
                        dash.message = "Cycle finished".into();
                        dash.refresh(queue)?;
                    }
                    _ => {}
                }
            }
        } else if dash.last_refresh.elapsed() > Duration::from_secs(2) {
            dash.refresh(queue)?;
        }
    }
    Ok(())
}

fn ui(f: &mut ratatui::Frame, dash: &Dashboard) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Length(5),
            Constraint::Min(6),
            Constraint::Length(3),
        ])
        .split(f.area());

    let header = Paragraph::new(Line::from(vec![
        Span::styled("🌸 Piya", Style::default().fg(Color::Magenta).add_modifier(Modifier::BOLD)),
        Span::raw("  — live queue  "),
        Span::styled(
            dash.message.as_str(),
            Style::default().fg(Color::DarkGray),
        ),
    ]))
    .block(Block::default().borders(Borders::ALL).title("Dashboard"));
    f.render_widget(header, chunks[0]);

    let stats = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage(25),
            Constraint::Percentage(25),
            Constraint::Percentage(25),
            Constraint::Percentage(25),
        ])
        .split(chunks[1]);

    stat_box(f, stats[0], "Pending", dash.pending, Color::Yellow);
    stat_box(f, stats[1], "Running", dash.in_progress, Color::Cyan);
    stat_box(f, stats[2], "Done", dash.done, Color::Green);
    stat_box(f, stats[3], "Failed", dash.failed, Color::Red);

    let lists = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
        .split(chunks[2]);

    let pending_items: Vec<ListItem> = if dash.pending_titles.is_empty() {
        vec![ListItem::new("(no pending tasks)")]
    } else {
        dash.pending_titles.iter().map(|t| ListItem::new(t.as_str())).collect()
    };
    let done_items: Vec<ListItem> = if dash.recent_done.is_empty() {
        vec![ListItem::new("(none yet)")]
    } else {
        dash.recent_done.iter().map(|t| ListItem::new(t.as_str())).collect()
    };

    f.render_widget(
        List::new(pending_items).block(Block::default().borders(Borders::ALL).title("Next up")),
        lists[0],
    );
    f.render_widget(
        List::new(done_items).block(Block::default().borders(Borders::ALL).title("Recent done")),
        lists[1],
    );

    let footer = Paragraph::new("Keys: q quit · r run next · R run all pending")
        .style(Style::default().fg(Color::DarkGray))
        .block(Block::default().borders(Borders::ALL));
    f.render_widget(footer, chunks[3]);
}

fn stat_box(f: &mut ratatui::Frame, area: Rect, label: &str, value: usize, color: Color) {
    let p = Paragraph::new(format!("{value}"))
        .style(Style::default().fg(color).add_modifier(Modifier::BOLD))
        .block(Block::default().borders(Borders::ALL).title(label));
    f.render_widget(p, area);
}
