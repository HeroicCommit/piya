mod popup;
mod signal;

pub use popup::{request_permission, send_popup};
pub use signal::{
    load_last_notified, save_last_notified, signal_age_secs, task_finished, task_started, user_alert,
};
