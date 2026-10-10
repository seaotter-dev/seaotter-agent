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

mod http;
pub mod openai;
pub mod typesafe;

pub use openai::{MaxTokensField, OpenAiCompatible, OpenAiCompatibleConfig, StructuredMode};
pub use typesafe::{TypeSafeConfig, TypeSafeDecision};
