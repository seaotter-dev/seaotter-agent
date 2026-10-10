use std::collections::BTreeMap;
use std::fmt;

use serde::{Deserialize, Serialize};

use crate::decision::Question;
use crate::error::HarnessError;
use crate::generate::OutputSpec;
use crate::policy::ConfidencePolicy;
use crate::state::State;

#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct NodeId(pub String);

impl From<&str> for NodeId {
    fn from(id: &str) -> Self {
        Self(id.to_owned())
    }
}

impl fmt::Display for NodeId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// A pure Rust step over workflow state.
pub type Step = fn(&mut State) -> Result<(), HarnessError>;

/// Allows a `Generate` node to run only if the `decide` node answered
/// `question` with `allow` and its confidence policy continued.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Gate {
    pub decide: NodeId,
    pub question: String,
    pub allow: String,
}

/// A workflow node. Built in Rust; model output never constructs or selects one.
///
/// `reads` lists the state keys the node may see.
#[derive(Debug, Clone)]
pub enum Node {
    Deterministic {
        id: NodeId,
        step: Step,
    },
    Decide {
        id: NodeId,
        reads: Vec<String>,
        questions: BTreeMap<String, Question>,
        policy: ConfidencePolicy,
    },
    Generate {
        id: NodeId,
        task: String,
        reads: Vec<String>,
        output: OutputSpec,
        max_tokens: u32,
        gate: Option<Gate>,
    },
    Effect {
        id: NodeId,
        name: String,
        reads: Vec<String>,
    },
}

impl Node {
    pub fn id(&self) -> &NodeId {
        match self {
            Self::Deterministic { id, .. }
            | Self::Decide { id, .. }
            | Self::Generate { id, .. }
            | Self::Effect { id, .. } => id,
        }
    }
}
