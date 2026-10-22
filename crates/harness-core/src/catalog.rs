//! The operations a consultant may use to build a workflow document.
//!
//! The effect names are `file_read`, `http_get`, and `composio`. Any other name
//! is rejected and leaves the catalog unchanged. The `composio` grant is one
//! action name.

use std::collections::{BTreeMap, BTreeSet};

use serde::Deserialize;
use serde_json::Value;

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
    /// `composio` is granted by one action name, and stores that action with
    /// its connection id.
    pub fn add_effect(
        &mut self,
        id: impl Into<NodeId>,
        name: impl Into<String>,
        reads: Vec<String>,
        connection: Option<String>,
        action: Option<String>,
    ) -> Result<(), HarnessError> {
        let name = name.into();
        if name == "composio" {
            let Some(action) = action
                .as_deref()
                .map(str::trim)
                .filter(|text| !text.is_empty())
            else {
                return Err(HarnessError::Workflow(
                    "effect `composio` has no action".into(),
                ));
            };
            let Some(connection) = connection
                .as_deref()
                .map(str::trim)
                .filter(|text| !text.is_empty())
            else {
                return Err(HarnessError::Workflow(
                    "effect `composio` has no connection".into(),
                ));
            };
            if !self.grants.contains(action) {
                return Err(HarnessError::Workflow(format!(
                    "effect `composio` action `{action}` has no grant"
                )));
            }
            return self.insert(DocumentNode::Effect {
                id: id.into(),
                name,
                reads,
                connection: Some(connection.to_owned()),
                action: Some(action.to_owned()),
            });
        }
        if name != "file_read" && name != "http_get" {
            return Err(HarnessError::Workflow(format!("unknown effect `{name}`")));
        }
        if connection.is_some() || action.is_some() {
            return Err(HarnessError::Workflow(format!(
                "effect `{name}` does not take a connection"
            )));
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
            connection: None,
            action: None,
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

/// Applies a recorded consultant reply. The owner, tier, and grants come from
/// the arguments. A failed call returns that error and no document.
pub fn apply_reply(
    owner: impl Into<String>,
    tier: RiskTier,
    grants: impl IntoIterator<Item = impl Into<String>>,
    reply: &str,
) -> Result<WorkflowDocument, HarnessError> {
    let calls: Vec<Value> = serde_json::from_str(reply)
        .map_err(|err| HarnessError::Workflow(format!("consultant reply: {err}")))?;
    let mut catalog = Catalog::new(owner, tier, grants)?;
    for call in calls {
        apply_call(&mut catalog, &call)?;
    }
    catalog.document()
}

fn apply_call(catalog: &mut Catalog, call: &Value) -> Result<(), HarnessError> {
    let op = call.get("op").and_then(Value::as_str).unwrap_or("");
    if op == "add_grant" || op == "grant_add" {
        return Err(HarnessError::Workflow(
            "grant add is not a catalog call".into(),
        ));
    }
    if !matches!(
        op,
        "add_decide" | "add_generate" | "add_effect" | "connect" | "set_gate" | "set_low"
    ) {
        return Err(HarnessError::Workflow(format!(
            "unknown catalog call `{op}`"
        )));
    }
    let parsed: CatalogCall = serde_json::from_value(call.clone())
        .map_err(|err| HarnessError::Workflow(format!("consultant reply: {err}")))?;
    match parsed {
        CatalogCall::AddDecide {
            id,
            reads,
            questions,
            policy,
        } => catalog.add_decide(id, reads, questions, policy),
        CatalogCall::AddGenerate {
            id,
            task,
            reads,
            output,
            max_tokens,
        } => catalog.add_generate(id, task, reads, output, max_tokens),
        CatalogCall::AddEffect {
            id,
            name,
            reads,
            connection,
            action,
        } => catalog.add_effect(id, name, reads, connection, action),
        CatalogCall::Connect { from, when, to } => catalog.connect(from, when, to),
        CatalogCall::SetGate { id, gate } => catalog.set_gate(id, gate),
        CatalogCall::SetLow { action, to } => catalog.set_low(action, to),
    }
}

#[derive(Deserialize)]
#[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
enum CatalogCall {
    AddDecide {
        id: NodeId,
        reads: Vec<String>,
        questions: BTreeMap<String, Question>,
        policy: ConfidencePolicy,
    },
    AddGenerate {
        id: NodeId,
        task: String,
        reads: Vec<String>,
        output: OutputSpec,
        max_tokens: u32,
    },
    AddEffect {
        id: NodeId,
        name: String,
        reads: Vec<String>,
        #[serde(default)]
        connection: Option<String>,
        #[serde(default)]
        action: Option<String>,
    },
    Connect {
        from: NodeId,
        when: BranchWhen,
        to: NodeId,
    },
    SetGate {
        id: NodeId,
        gate: Gate,
    },
    SetLow {
        action: ConfidenceAction,
        to: NodeId,
    },
}
