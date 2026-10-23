//! Core of the code-owned agent harness.
//!
//! Hosts the `Machine`, its explicit node graph, the `DecisionModel`,
//! `GenerativeModel`, and `Effect` traits, and the append-only event log.
//! Code owns control flow: the next step is always a Rust match or
//! state-machine transition, never model output. See `SPEC.md`.
//!
//! # What models cannot do
//!
//! - No request carries a tool list, and no response carries tool calls, a
//!   next node, or memory writes.
//! - [`Node`] values are built in Rust. [`Node::Effect`] names an effect in
//!   code, never from model output.
//! - [`State`] is changed only by deterministic steps and the machine. Model
//!   responses return data, not state edits.
//! - The [`Machine`] picks the next node only through the workflow's
//!   [`Transition`], a Rust function.
//! - Question or node skeletons generated at design time are written to files
//!   for review. The harness has no path to compile or run generated Rust.

mod anomaly;
mod catalog;
mod decision;
mod diff;
mod document;
mod drift;
mod effect;
mod error;
mod eval;
mod event;
mod future;
mod generate;
mod machine;
mod node;
mod policy;
mod state;
mod validate;
mod workflow;

pub use anomaly::anomaly;
pub use catalog::{Catalog, apply_reply};
pub use decision::{
    Answer, DecisionModel, DecisionRequest, DecisionRequestBuilder, DecisionResponse, NoulCriteria,
    Question,
};
pub use diff::{Divergence, diff_runs};
pub use document::{BranchEdge, BranchWhen, DocumentNode, RiskTier, WorkflowDocument};
pub use drift::{Baseline, DriftThresholds, PriorRun, load_baseline};
pub use effect::{Capability, Effect};
pub use error::HarnessError;
pub use eval::{FixtureError, check_answer_fixture, check_branch_fixture};
pub use event::{
    Event, EventKind, EventLog, GateOutcome, GrantDecision, Signal, Usage, append_run, input_hash,
    read_run,
};
pub use future::BoxFuture;
pub use generate::{GenOutput, GenRequest, GenResponse, GenerativeModel, JsonSchema, OutputSpec};
pub use machine::{Machine, MachineBuilder, Run, RunStatus};
pub use node::{Gate, Node, NodeId, Step};
pub use policy::{ConfidenceAction, ConfidencePolicy};
pub use state::State;
pub use validate::validate_output;
pub use workflow::{Next, Outcome, Transition, Workflow};
