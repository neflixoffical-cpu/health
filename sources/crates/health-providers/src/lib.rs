//! health-providers — LLM providers for Health.
//!
//! * [`ProviderHub`] — model discovery (auto-parse / manual / hybrid), cache, key pools,
//!   backends for the agent ([`health_core::BackendResolver`]).
//! * Two wire protocols: Anthropic Messages ([`anthropic`]) and OpenAI Chat Completions
//!   ([`openai`]), both streamed over SSE.
//! * Key rotation and retries happen before any content is streamed; unsupported optional
//!   parameters are dropped automatically after a 400 ("self-healing").

pub mod anthropic;
pub mod caps;
pub mod errors;
pub mod hub;
pub mod openai;
pub mod sse;

pub use hub::{ProviderHub, RefreshReport};
