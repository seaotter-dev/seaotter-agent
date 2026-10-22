//! The operations a consultant may use to build a workflow document.
//!
//! The effect names are `file_read` and `http_get`. Any other name is rejected
//! and leaves the catalog unchanged.

use std::collections::{BTreeMap, BTreeSet};

use crate::decision::Question;
use crate::document::{BranchEdge, BranchWhen, DocumentNode, RiskTier, WorkflowDocument};
use crate::error::HarnessError;
use crate::generate::OutputSpec;
use crate::node::{Gate, NodeId};
use crate::policy::{ConfidenceAction, ConfidencePolicy};

/// A workflow under construction. Grants are fixed when the catalog is opened.
#[derive(Debug, Clone, PartialEq)]
pub struct Catalog {
    owner: String,
    tier: RiskTier,
    grants: BTreeSet<String>,
    start: Option<NodeId>,
    nodes: Vec<DocumentNode>,
    edges: Vec<BranchEdge>,
    on_low: BTreeMap<ConfidenceAction, NodeId>,
}

impl Catalog {
    /// Rejects a blank owner. The grant list is not extended by later calls.
    pub fn new(
        owner: impl Into<String>,
        tier: RiskTier,
        grants: impl IntoIterator<Item = impl Into<String>>,
    ) -> Result<Self, HarnessError> {
        let owner = owner.into();
        if owner.trim().is_empty() {
            return Err(HarnessError::Workflow("document has no owner".into()));
        }
        Ok(Self {
            owner,
            tier,
            grants: grants.into_iter().map(Into::into).collect(),
            start: None,
            nodes: Vec::new(),
            edges: Vec::new(),
            on_low: BTreeMap::new(),
        })
    }

    /// The saved document. The first added node is `start`.
    pub fn document(&self) -> Result<WorkflowDocument, HarnessError> {
        let Some(start) = self.start.clone() else {
            return Err(HarnessError::Workflow("document has no start".into()));
        };
        WorkflowDocument::new(
            self.owner.clone(),
            self.tier,
            self.grants.iter().cloned(),
            start,
            self.nodes.clone(),
            self.edges.clone(),
            self.on_low.clone(),
        )
    }

    pub fn add_decide(
        &mut self,
        id: impl Into<NodeId>,
        reads: Vec<String>,
        questions: BTreeMap<String, Question>,
        policy: ConfidencePolicy,
    ) -> Result<(), HarnessError> {
        self.insert(DocumentNode::Decide {
            id: id.into(),
            reads,
            questions,
            policy,
        })
    }

    /// Stores the generate node with no gate.
    pub fn add_generate(
        &mut self,
        id: impl Into<NodeId>,
        task: impl Into<String>,
        reads: Vec<String>,
        output: OutputSpec,
        max_tokens: u32,
    ) -> Result<(), HarnessError> {
        self.insert(DocumentNode::Generate {
            id: id.into(),
            task: task.into(),
            reads,
            output,
            max_tokens,
            gate: None,
        })
    }

    /// Allows `file_read` and `http_get` when that name is already granted.
    pub fn add_effect(
        &mut self,
        id: impl Into<NodeId>,
        name: impl Into<String>,
        reads: Vec<String>,
    ) -> Result<(), HarnessError> {
        let name = name.into();
        if name != "file_read" && name != "http_get" {
            return Err(HarnessError::Workflow(format!("unknown effect `{name}`")));
        }
        if !self.grants.contains(&name) {
            return Err(HarnessError::Workflow(format!(
                "effect `{name}` has no grant"
            )));
        }
        self.insert(DocumentNode::Effect {
            id: id.into(),
            name,
            reads,
        })
    }

    /// `to` may name a sink that is not a node. `from` must be a node.
    pub fn connect(
        &mut self,
        from: impl Into<NodeId>,
        when: BranchWhen,
        to: impl Into<NodeId>,
    ) -> Result<(), HarnessError> {
        let from = from.into();
        if !self.nodes.iter().any(|node| node.id() == &from) {
            return Err(HarnessError::Workflow(format!(
                "edge from `{from}` is not a node"
            )));
        }
        self.edges.push(BranchEdge {
            from,
            when,
            to: to.into(),
        });
        Ok(())
    }

    /// Sets the gate on a generate node. `gate.decide` must be a decide node.
    pub fn set_gate(&mut self, id: impl Into<NodeId>, gate: Gate) -> Result<(), HarnessError> {
        let id = id.into();
        let Some(index) = self.nodes.iter().position(|node| node.id() == &id) else {
            return Err(HarnessError::Workflow(format!(
                "gate on `{id}` is not a generate node"
            )));
        };
        if !matches!(self.nodes[index], DocumentNode::Generate { .. }) {
            return Err(HarnessError::Workflow(format!(
                "gate on `{id}` is not a generate node"
            )));
        }
        let points_at_decide = self.nodes.iter().any(|other| {
            matches!(other, DocumentNode::Decide { id: decide, .. } if decide == &gate.decide)
        });
        if !points_at_decide {
            return Err(HarnessError::Workflow(format!(
                "gate on `{id}` must point at a Decide node, got `{}`",
                gate.decide
            )));
        }
        if let DocumentNode::Generate { gate: slot, .. } = &mut self.nodes[index] {
            *slot = Some(gate);
        }
        Ok(())
    }

    /// Sets one low-confidence edge. `Continue` and `RetryStronger` are rejected.
    pub fn set_low(
        &mut self,
        action: ConfidenceAction,
        to: impl Into<NodeId>,
    ) -> Result<(), HarnessError> {
        if matches!(
            action,
            ConfidenceAction::Continue | ConfidenceAction::RetryStronger
        ) {
            return Err(HarnessError::Workflow(format!(
                "{action:?} is not a low-confidence edge"
            )));
        }
        self.on_low.insert(action, to.into());
        Ok(())
    }

    fn insert(&mut self, node: DocumentNode) -> Result<(), HarnessError> {
        let id = node.id().clone();
        if self.nodes.iter().any(|other| other.id() == &id) {
            return Err(HarnessError::Workflow(format!("duplicate node id `{id}`")));
        }
        if self.start.is_none() {
            self.start = Some(id);
        }
        self.nodes.push(node);
        Ok(())
    }
}
