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
/// A write effect runs only after `human_gate` has passed.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct WorkflowDocument {
    pub owner: String,
    pub tier: RiskTier,
    pub grants: BTreeSet<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub human_gate: Option<Gate>,
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
    #[serde(default)]
    human_gate: Option<Gate>,
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
        #[serde(default, skip_serializing_if = "Option::is_none")]
        connection: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        action: Option<String>,
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
            if let DocumentNode::Effect {
                name,
                connection,
                action,
                ..
            } = node
            {
                if name == "shell" || name == "dangerous" || name == "platform" {
                    return Err(HarnessError::Workflow(format!(
                        "effect `{name}` cannot be loaded"
                    )));
                }
                if name == "composio" {
                    let action = required_text(action, "effect `composio` has no action")?;
                    required_text(connection, "effect `composio` has no connection")?;
                    if !grants.contains(action) {
                        return Err(HarnessError::Workflow(format!(
                            "effect `composio` action `{action}` has no grant"
                        )));
                    }
                } else {
                    if connection.is_some() || action.is_some() {
                        return Err(HarnessError::Workflow(format!(
                            "effect `{name}` does not take a connection"
                        )));
                    }
                    if !grants.contains(name) {
                        return Err(HarnessError::Workflow(format!(
                            "effect `{name}` has no grant"
                        )));
                    }
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
            human_gate: None,
            start,
            nodes,
            edges,
            on_low,
        })
    }

    /// Points the document's human gate at a Decide node.
    ///
    /// A write-tier effect stays skipped until this gate passes.
    pub fn with_human_gate(mut self, gate: Gate) -> Result<Self, HarnessError> {
        let points_at_decide = self
            .nodes
            .iter()
            .any(|other| matches!(other, DocumentNode::Decide { id, .. } if id == &gate.decide));
        if !points_at_decide {
            return Err(HarnessError::Workflow(format!(
                "human gate must point at a Decide node, got `{}`",
                gate.decide
            )));
        }
        self.human_gate = Some(gate);
        Ok(self)
    }

    /// Replaces one question on a decide node.
    pub fn set_question(
        &mut self,
        id: impl Into<NodeId>,
        key: &str,
        question: Question,
    ) -> Result<(), HarnessError> {
        let id = id.into();
        let Some(node) = self.nodes.iter_mut().find(|node| node.id() == &id) else {
            return Err(HarnessError::Workflow(format!(
                "node `{id}` is not defined"
            )));
        };
        let DocumentNode::Decide { questions, .. } = node else {
            return Err(HarnessError::Workflow(format!(
                "question on `{id}` is not a decide node"
            )));
        };
        let Some(slot) = questions.get_mut(key) else {
            return Err(HarnessError::Workflow(format!(
                "question `{key}` is not on `{id}`"
            )));
        };
        *slot = question;
        Ok(())
    }

    /// Removes a node, the edges that touch it, and a gate that pointed at it.
    ///
    /// The first remaining node becomes `start` when the deleted node was the
    /// start. The last node cannot be deleted.
    pub fn delete_node(&mut self, id: impl Into<NodeId>) -> Result<(), HarnessError> {
        let id = id.into();
        if !self.nodes.iter().any(|node| node.id() == &id) {
            return Err(HarnessError::Workflow(format!(
                "node `{id}` is not defined"
            )));
        }
        if self.nodes.len() == 1 {
            return Err(HarnessError::Workflow(format!(
                "cannot delete the last node `{id}`"
            )));
        }
        self.nodes.retain(|node| node.id() != &id);
        self.edges.retain(|edge| edge.from != id && edge.to != id);
        self.on_low.retain(|_, to| to != &id);
        for node in &mut self.nodes {
            if let DocumentNode::Generate { gate, .. } = node
                && gate.as_ref().is_some_and(|gate| gate.decide == id)
            {
                *gate = None;
            }
        }
        if self
            .human_gate
            .as_ref()
            .is_some_and(|gate| gate.decide == id)
        {
            self.human_gate = None;
        }
        if self.start == id {
            self.start = self.nodes[0].id().clone();
        }
        Ok(())
    }

    /// Points the edge that leaves `from` under `when` at `to`.
    pub fn reconnect(
        &mut self,
        from: impl Into<NodeId>,
        when: &BranchWhen,
        to: impl Into<NodeId>,
    ) -> Result<(), HarnessError> {
        let from = from.into();
        let Some(edge) = self
            .edges
            .iter_mut()
            .find(|edge| edge.from == from && &edge.when == when)
        else {
            return Err(HarnessError::Workflow(format!("no edge from `{from}`")));
        };
        edge.to = to.into();
        Ok(())
    }
}

fn required_text<'a>(value: &'a Option<String>, missing: &str) -> Result<&'a str, HarnessError> {
    match value
        .as_deref()
        .map(str::trim)
        .filter(|text| !text.is_empty())
    {
        Some(text) => Ok(text),
        None => Err(HarnessError::Workflow(missing.into())),
    }
}

impl<'de> Deserialize<'de> for WorkflowDocument {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let file = DocumentFile::deserialize(deserializer)?;
        let gate = file.human_gate;
        let doc = WorkflowDocument::new(
            file.owner,
            file.tier,
            file.grants,
            file.start,
            file.nodes,
            file.edges,
            file.on_low,
        )
        .map_err(D::Error::custom)?;
        match gate {
            Some(gate) => doc.with_human_gate(gate).map_err(D::Error::custom),
            None => Ok(doc),
        }
    }
}
