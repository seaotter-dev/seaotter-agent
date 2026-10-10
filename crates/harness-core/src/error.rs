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
    /// The workflow or machine configuration is invalid.
    Workflow(String),
    /// A run exceeded the machine's step limit.
    StepLimit(usize),
}

impl fmt::Display for HarnessError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Provider(msg) => write!(f, "provider error: {msg}"),
            Self::Effect(msg) => write!(f, "effect error: {msg}"),
            Self::SchemaInvalid(msg) => write!(f, "schema-invalid output: {msg}"),
            Self::CapabilityDenied(cap) => write!(f, "capability denied: {cap:?}"),
            Self::Workflow(msg) => write!(f, "workflow error: {msg}"),
            Self::StepLimit(n) => write!(f, "step limit of {n} exceeded"),
        }
    }
}

impl std::error::Error for HarnessError {}
