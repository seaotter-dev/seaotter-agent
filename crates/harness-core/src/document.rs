//! A workflow document a consultant can emit.
//!
//! Nodes are decide, generate, and effect. Branch edges are data. The
//! document has no [`Transition`](crate::workflow::Transition) and no
//! [`Step`](crate::node::Step). [`Machine::run_document`](crate::machine::Machine::run_document)
//! follows the edges.

use std::collections::{BTreeMap, BTreeSet};

use serde::de::Error as _;
use serde::{Deserialize, Deserializer, Serialize};

use crate::decision::Question;
use crate::error::HarnessError;
use crate::generate::OutputSpec;
use crate::node::{Gate, NodeId};
use crate::policy::{ConfidenceAction, ConfidencePolicy};

/// The risk tier stored on a document. There is no dangerous tier.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RiskTier {
    Read,
    Write,
}

/// A saved workflow. `to` on an edge may name a sink that is not a node.
///
/// Loading requires an owner, a [`RiskTier`], and a grant for every effect.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct WorkflowDocument {
    pub owner: String,
    pub tier: RiskTier,
    pub grants: BTreeSet<String>,
    pub start: NodeId,
    pub nodes: Vec<DocumentNode>,
    pub edges: Vec<BranchEdge>,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub on_low: BTreeMap<ConfidenceAction, NodeId>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct DocumentFile {
    owner: String,
    tier: RiskTier,
    #[serde(default)]
    grants: BTreeSet<String>,
    start: NodeId,
    nodes: Vec<DocumentNode>,
    edges: Vec<BranchEdge>,
    #[serde(default)]
    on_low: BTreeMap<ConfidenceAction, NodeId>,
}

/// A node in a [`WorkflowDocument`]. Model output never selects one.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum DocumentNode {
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
        #[serde(default, skip_serializing_if = "Option::is_none")]
        gate: Option<Gate>,
    },
    Effect {
        id: NodeId,
        name: String,
        reads: Vec<String>,
    },
}

/// One branch from a node to a node or a sink.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BranchEdge {
    pub from: NodeId,
    pub when: BranchWhen,
    pub to: NodeId,
}

/// The condition that selects a [`BranchEdge`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum BranchWhen {
    Always,
    /// The decide question's top label equals `equals`.
    Label {
        question: String,
        equals: String,
    },
    /// The decide question's top label matched none of the label edges.
    Other {
        question: String,
    },
    Generated,
    Skipped,
    /// A bool already stored in state, such as `approved`.
    State {
        key: String,
        equals: bool,
    },
}

impl DocumentNode {
    pub fn id(&self) -> &NodeId {
        match self {
            Self::Decide { id, .. } | Self::Generate { id, .. } | Self::Effect { id, .. } => id,
        }
    }
}

impl WorkflowDocument {
    /// Rejects a blank owner, an effect named `shell` or `dangerous`, an
    /// effect with no grant, a duplicate id, a missing `start`, a gate that
    /// does not point at a decide node, an edge whose `from` is not a node,
    /// and a low-confidence edge of `Continue` or `RetryStronger`.
    pub fn new(
        owner: impl Into<String>,
        tier: RiskTier,
        grants: impl IntoIterator<Item = impl Into<String>>,
        start: NodeId,
        nodes: Vec<DocumentNode>,
        edges: Vec<BranchEdge>,
        on_low: BTreeMap<ConfidenceAction, NodeId>,
    ) -> Result<Self, HarnessError> {
        let owner = owner.into();
        if owner.trim().is_empty() {
            return Err(HarnessError::Workflow("document has no owner".into()));
        }
        let grants: BTreeSet<String> = grants.into_iter().map(Into::into).collect();
        for node in &nodes {
            if let DocumentNode::Effect { name, .. } = node {
                if name == "shell" || name == "dangerous" {
                    return Err(HarnessError::Workflow(format!(
                        "effect `{name}` cannot be loaded"
                    )));
                }
                if !grants.contains(name) {
                    return Err(HarnessError::Workflow(format!(
                        "effect `{name}` has no grant"
                    )));
                }
            }
        }
        let mut ids = BTreeSet::new();
        for node in &nodes {
            let id = node.id().clone();
            if !ids.insert(id.clone()) {
                return Err(HarnessError::Workflow(format!("duplicate node id `{id}`")));
            }
        }
        if !ids.contains(&start) {
            return Err(HarnessError::Workflow(format!(
                "start node `{start}` is not defined"
            )));
        }
        for node in &nodes {
            if let DocumentNode::Generate {
                id,
                gate: Some(gate),
                ..
            } = node
            {
                let points_at_decide = nodes.iter().any(|other| {
                    matches!(other, DocumentNode::Decide { id: decide, .. } if decide == &gate.decide)
                });
                if !points_at_decide {
                    return Err(HarnessError::Workflow(format!(
                        "gate on `{id}` must point at a Decide node, got `{}`",
                        gate.decide
                    )));
                }
            }
        }
        for edge in &edges {
            if !ids.contains(&edge.from) {
                return Err(HarnessError::Workflow(format!(
                    "edge from `{}` is not a node",
                    edge.from
                )));
            }
        }
        for action in on_low.keys() {
            if matches!(
                action,
                ConfidenceAction::Continue | ConfidenceAction::RetryStronger
            ) {
                return Err(HarnessError::Workflow(format!(
                    "{action:?} is not a low-confidence edge"
                )));
            }
        }
        Ok(Self {
            owner,
            tier,
            grants,
            start,
            nodes,
            edges,
            on_low,
        })
    }
}

impl<'de> Deserialize<'de> for WorkflowDocument {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let file = DocumentFile::deserialize(deserializer)?;
        WorkflowDocument::new(
            file.owner,
            file.tier,
            file.grants,
            file.start,
            file.nodes,
            file.edges,
            file.on_low,
        )
        .map_err(D::Error::custom)
    }
}
