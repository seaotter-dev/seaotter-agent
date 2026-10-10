//! Model providers for the code-owned agent harness.
//!
//! System One decision clients (TypeSafe Jev and compatible `/v1/systemone`
//! endpoints) implement `DecisionModel`. OpenAI-compatible and Anthropic
//! clients implement `GenerativeModel` as single-shot, schema-bounded calls
//! that never choose tools or the next step. See `SPEC.md`.
//!
//! - [`TypeSafeDecision`]: System One client. Configure it with
//!   [`TypeSafeConfig::new`] or [`TypeSafeConfig::from_env`].

pub mod typesafe;

pub use typesafe::{TypeSafeConfig, TypeSafeDecision};
