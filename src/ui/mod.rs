pub mod assign;
pub mod badge;
pub mod app;
#[cfg(all(target_os = "macos", feature = "legacy-ui"))]
pub mod chat;
#[cfg(target_os = "macos")]
pub mod macos;
pub mod menu;
pub mod term;
pub mod watch;

pub use menu::run_menu;
