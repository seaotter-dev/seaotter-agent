use std::fmt;

use crate::effect::Capability;

/// Errors surfaced by models, effects, and validation.
#[derive(Debug, Clone, PartialEq)]
pub enum HarnessError {
    /// A decision or generative provider failed.
    Provider(String),
    /// An effect failed while running.
    Effect(String),
    /// A generative response did not match its schema.
    SchemaInvalid(String),
    /// A node needs a capability the machine was not granted.
    CapabilityDenied(Capability),
}

impl fmt::Display for HarnessError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Provider(msg) => write!(f, "provider error: {msg}"),
            Self::Effect(msg) => write!(f, "effect error: {msg}"),
            Self::SchemaInvalid(msg) => write!(f, "schema-invalid output: {msg}"),
            Self::CapabilityDenied(cap) => write!(f, "capability denied: {cap:?}"),
        }
    }
}

impl std::error::Error for HarnessError {}
