//! health-core — shared types and contracts for every Health crate.
//!
//! Design rules (all crates follow them):
//! * Health NEVER sends a system prompt of its own. `ChatRequest::system` is only ever the
//!   user's own optional text (`Config::custom_system_prompt`), `None` by default.
//! * Crates talk to each other through the traits in [`tool`], [`backend`] and the channel
//!   types in [`agent_api`], so they can be developed and compiled independently.
//! * UI strings are localized inline with [`t!`] / [`tf!`] (Russian + English).

pub mod l10n;
pub mod paths;
pub mod secret;
pub mod types;
pub mod tool;
pub mod backend;
pub mod agent_api;
pub mod config;
pub mod sessions;
pub mod log;
pub mod util;
pub mod procs;
pub mod workflow;

pub use agent_api::*;
pub use backend::*;
pub use config::*;
pub use l10n::{lang, set_lang, Lang};
pub use secret::SecretString;
pub use sessions::{list_sessions, Conversation, SessionMeta};
pub use tool::*;
pub use types::*;

/// Product name shown everywhere.
pub const APP_NAME: &str = "Health";
/// Version of the whole workspace.
pub const APP_VERSION: &str = env!("CARGO_PKG_VERSION");
/// Maximum number of API keys the user can store.
pub const MAX_KEYS: usize = 20;
/// HTTP User-Agent used by every network request Health makes.
pub fn user_agent() -> String {
    format!("Health/{APP_VERSION} ({}; terminal AI assistant)", if cfg!(windows) { "Windows" } else { "Linux" })
}
