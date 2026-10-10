//! Model providers for the code-owned agent harness.
//!
//! System One decision clients (TypeSafe Jev and compatible `/v1/systemone`
//! endpoints) implement `DecisionModel`. OpenAI-compatible and Anthropic
//! clients implement `GenerativeModel` as single-shot, schema-bounded calls
//! that never choose tools or the next step. See `SPEC.md`.
//!
//! - [`TypeSafeDecision`]: System One client. Configure it with
//!   [`TypeSafeConfig::new`] or [`TypeSafeConfig::from_env`].
//! - [`OpenAiCompatible`]: `/chat/completions` client for Grok, Astra, and
//!   other OpenAI-compatible endpoints. Configure it with
//!   [`OpenAiCompatibleConfig::new`] or [`OpenAiCompatibleConfig::from_env`].
//! - [`Anthropic`]: Messages API client for Claude. Configure it with
//!   [`AnthropicConfig::new`] or [`AnthropicConfig::from_env`].
//!
//! The generative clients are interchangeable behind `GenerativeModel`:
//! swapping one for another does not change `Machine` control flow.

pub mod anthropic;
mod http;
pub mod openai;
pub mod typesafe;

pub use anthropic::{Anthropic, AnthropicConfig, AnthropicOutputMode};
pub use openai::{MaxTokensField, OpenAiCompatible, OpenAiCompatibleConfig, StructuredMode};
pub use typesafe::{TypeSafeConfig, TypeSafeDecision};
