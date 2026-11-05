use crate::chat_service::{ChatTurn, chat_reply};
use crate::config::Settings;
use crate::ui::assign::AssignOutcome;
use super::ChatMessage;

pub fn reply(
    settings: &Settings,
    history: &[ChatMessage],
    user_text: &str,
) -> AssignOutcome {
    let turns: Vec<ChatTurn> = history
        .iter()
        .map(|m| ChatTurn {
            role: m.role.clone(),
            text: m.text.clone(),
        })
        .collect();

    match chat_reply(settings, &turns, user_text) {
        Ok(text) => AssignOutcome {
            reply: text,
            task: None,
            run_worker: false,
        },
        Err(err) => AssignOutcome {
            reply: format!("Sorry, I couldn't respond: {err}"),
            task: None,
            run_worker: false,
        },
    }
}
