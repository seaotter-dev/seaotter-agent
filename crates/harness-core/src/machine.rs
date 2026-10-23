use std::collections::{BTreeMap, BTreeSet};
use std::time::Instant;

use serde_json::Value;

use crate::decision::{Answer, DecisionModel, DecisionRequest, DecisionResponse};
use crate::document::{BranchWhen, DocumentNode, RiskTier, WorkflowDocument};
use crate::effect::{Capability, Effect};
use crate::error::HarnessError;
use crate::event::{
    Event, EventKind, EventLog, GateOutcome, GrantDecision, Signal, Usage, input_hash,
};
use crate::generate::{GenOutput, GenRequest, GenResponse, GenerativeModel, OutputSpec};
use crate::node::{Gate, Node, NodeId};
use crate::policy::{ConfidenceAction, ConfidencePolicy};
use crate::state::State;
use crate::validate::validate_output;
use crate::workflow::{Next, Outcome, Workflow};

const DEFAULT_MAX_STEPS: usize = 1000;

type Answers = BTreeMap<String, Answer>;
type Decisions = BTreeMap<NodeId, Answers>;

/// How a run ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunStatus {
    /// The transition returned `Next::End`.
    Completed,
    /// A low-confidence decision took an edge with no registered handler.
    Halted(ConfidenceAction),
}

/// The result of a finished run.
#[derive(Debug, Clone, PartialEq)]
pub struct Run {
    pub state: State,
    /// Every node visited, in order.
    pub path: Vec<NodeId>,
    /// Decide, generate, and effect steps that succeeded. Deterministic nodes
    /// and skipped generates are omitted. A failed run returns `Err` and does
    /// not carry this log.
    pub events: EventLog,
    pub status: RunStatus,
}

/// Configures a [`Machine`]. No capability is granted unless `grant` is called.
pub struct MachineBuilder {
    decision: Option<Box<dyn DecisionModel>>,
    stronger_decision: Option<Box<dyn DecisionModel>>,
    generative: Option<Box<dyn GenerativeModel>>,
    effects: BTreeMap<String, Box<dyn Effect>>,
    granted: BTreeSet<Capability>,
    max_steps: usize,
}

impl MachineBuilder {
    pub fn decision(mut self, model: impl DecisionModel + 'static) -> Self {
        self.decision = Some(Box::new(model));
        self
    }

    /// The model used for the second call of a `RetryStronger` policy.
    pub fn stronger_decision(mut self, model: impl DecisionModel + 'static) -> Self {
        self.stronger_decision = Some(Box::new(model));
        self
    }

    pub fn generative(mut self, model: impl GenerativeModel + 'static) -> Self {
        self.generative = Some(Box::new(model));
        self
    }

    /// Registers an effect under its `name()`, replacing any with the same name.
    pub fn effect(mut self, effect: impl Effect + 'static) -> Self {
        self.effects
            .insert(effect.name().to_owned(), Box::new(effect));
        self
    }

    pub fn grant(mut self, capability: Capability) -> Self {
        self.granted.insert(capability);
        self
    }

    pub fn max_steps(mut self, max_steps: usize) -> Self {
        self.max_steps = max_steps;
        self
    }

    pub fn build(self) -> Machine {
        Machine {
            decision: self.decision,
            stronger_decision: self.stronger_decision,
            generative: self.generative,
            effects: self.effects,
            granted: self.granted,
            max_steps: self.max_steps,
        }
    }
}

/// Runs a [`Workflow`] or a [`WorkflowDocument`]. The next node comes from
/// the workflow's transition function or the document's edges, or from a
/// low-confidence handler. Model output is stored as data, never followed.
pub struct Machine {
    decision: Option<Box<dyn DecisionModel>>,
    stronger_decision: Option<Box<dyn DecisionModel>>,
    generative: Option<Box<dyn GenerativeModel>>,
    effects: BTreeMap<String, Box<dyn Effect>>,
    granted: BTreeSet<Capability>,
    max_steps: usize,
}

impl Machine {
    pub fn builder() -> MachineBuilder {
        MachineBuilder {
            decision: None,
            stronger_decision: None,
            generative: None,
            effects: BTreeMap::new(),
            granted: BTreeSet::new(),
            max_steps: DEFAULT_MAX_STEPS,
        }
    }

    /// Runs `wf` from its start node until the transition returns `Next::End`
    /// or a low-confidence edge has no handler.
    ///
    /// Decide, Generate, and Effect outputs are written to `state[node id]`.
    pub async fn run(&self, wf: &Workflow, state: State) -> Result<Run, HarnessError> {
        self.replay(wf, state, &[]).await
    }

    /// Re-runs `wf` from `state`, serving a step from `trace` when the next
    /// event has the same node, kind, and input hash. Anything the trace does
    /// not cover runs live: a decide or generate calls the configured model,
    /// and an effect calls `run`.
    ///
    /// Deterministic steps always re-run. A covered effect still has to be
    /// registered and granted; its `run` is not called.
    pub async fn replay(
        &self,
        wf: &Workflow,
        mut state: State,
        trace: &[Event],
    ) -> Result<Run, HarnessError> {
        let mut trace = Trace {
            events: trace,
            next: 0,
        };
        let mut decisions = Decisions::new();
        let mut path = Vec::new();
        let mut events = EventLog::default();
        let mut current = wf.start().clone();
        loop {
            if path.len() >= self.max_steps {
                return Err(HarnessError::StepLimit(self.max_steps));
            }
            let node = wf
                .node(&current)
                .ok_or_else(|| HarnessError::Workflow(format!("unknown node `{current}`")))?;
            path.push(current.clone());

            let (outcome, low_edge) = self
                .step(
                    &|id| decide_policy(wf, id),
                    node,
                    &mut state,
                    &decisions,
                    &mut events,
                    &mut trace,
                )
                .await?;
            match &outcome {
                Outcome::Decided(answers) => {
                    let value = serde_json::to_value(answers)
                        .map_err(|e| HarnessError::Provider(e.to_string()))?;
                    state.insert(current.0.clone(), value);
                    decisions.insert(current.clone(), answers.clone());
                }
                Outcome::Generated(output) => {
                    state.insert(current.0.clone(), stored_gen(output));
                }
                Outcome::Effected(value) => {
                    state.insert(current.0.clone(), value.clone());
                }
                Outcome::Done | Outcome::Skipped => {}
            }

            if let Some(action) = low_edge {
                match wf.handler(action) {
                    Some(handler) => {
                        current = handler.clone();
                        continue;
                    }
                    None => {
                        return Ok(Run {
                            state,
                            path,
                            events,
                            status: RunStatus::Halted(action),
                        });
                    }
                }
            }

            match (wf.transition())(&current, &outcome, &state) {
                Next::Goto(next) => current = next,
                Next::End => {
                    return Ok(Run {
                        state,
                        path,
                        events,
                        status: RunStatus::Completed,
                    });
                }
            }
        }
    }

    /// Runs `doc` from its start node. The document's edges are the transition.
    ///
    /// An id that is not a node is a sink: the run pushes it on the path and
    /// completes. No matching edge also completes the run, so a missing
    /// `approved` value does not visit a later generate node. A write-tier
    /// effect is skipped until `human_gate` passes. An effect named `shell`
    /// or `dangerous`, or one whose capability is `DangerousShell`, does not run.
    pub async fn run_document(
        &self,
        doc: &WorkflowDocument,
        mut state: State,
    ) -> Result<Run, HarnessError> {
        let empty = &[][..];
        let mut trace = Trace {
            events: empty,
            next: 0,
        };
        let mut decisions = Decisions::new();
        let mut path = Vec::new();
        let mut events = EventLog::default();
        let mut current = doc.start.clone();
        loop {
            if path.len() >= self.max_steps {
                return Err(HarnessError::StepLimit(self.max_steps));
            }
            let Some(doc_node) = doc.nodes.iter().find(|node| node.id() == &current) else {
                path.push(current);
                return Ok(Run {
                    state,
                    path,
                    events,
                    status: RunStatus::Completed,
                });
            };
            path.push(current.clone());
            let node = node_from_document(doc_node);
            let effect_name = match doc_node {
                DocumentNode::Effect { name, .. } => Some(name.clone()),
                _ => None,
            };
            let (outcome, low_edge) = if let Some(name) = effect_name {
                self.document_effect(
                    doc,
                    &name,
                    &node,
                    &mut state,
                    &decisions,
                    &mut events,
                    &mut trace,
                )
                .await?
            } else {
                self.step(
                    &|id| document_policy(doc, id),
                    &node,
                    &mut state,
                    &decisions,
                    &mut events,
                    &mut trace,
                )
                .await?
            };
            record_node_output(&current, &outcome, &mut state, &mut decisions)?;

            if let Some(action) = low_edge {
                match doc.on_low.get(&action) {
                    Some(handler) => {
                        current = handler.clone();
                        continue;
                    }
                    None => {
                        return Ok(Run {
                            state,
                            path,
                            events,
                            status: RunStatus::Halted(action),
                        });
                    }
                }
            }

            match next_edge(doc, &current, &outcome, &state) {
                Some(next) => current = next,
                None => {
                    return Ok(Run {
                        state,
                        path,
                        events,
                        status: RunStatus::Completed,
                    });
                }
            }
        }
    }

    /// Runs one document effect, or skips a write effect whose human gate is closed.
    ///
    /// `shell`, `dangerous`, `platform`, and `DangerousShell` return before
    /// `Effect::run`. A composio call reads its key only after the owner, the
    /// grant, and the human gate.
    async fn document_effect(
        &self,
        doc: &WorkflowDocument,
        name: &str,
        node: &Node,
        state: &mut State,
        decisions: &Decisions,
        events: &mut EventLog,
        trace: &mut Trace<'_>,
    ) -> Result<(Outcome, Option<ConfidenceAction>), HarnessError> {
        if name == "shell"
            || name == "dangerous"
            || name == "platform"
            || self.effects.get(name).is_some_and(|effect| {
                effect.capability() == Capability::DangerousShell || effect.provider() == "platform"
            })
        {
            return Err(HarnessError::Workflow(format!(
                "effect `{name}` cannot run"
            )));
        }
        if doc.owner.trim().is_empty() {
            return Err(HarnessError::Workflow("document has no owner".into()));
        }
        let action_granted = match node {
            Node::Effect {
                name: effect_name,
                action,
                ..
            } if effect_name == "composio" => {
                let Some(action) = action
                    .as_deref()
                    .map(str::trim)
                    .filter(|text| !text.is_empty())
                else {
                    return Err(HarnessError::Workflow(
                        "effect `composio` has no action".into(),
                    ));
                };
                if !doc.grants.contains(action) {
                    return Err(HarnessError::Workflow(format!(
                        "effect `composio` action `{action}` has no grant"
                    )));
                }
                true
            }
            _ => false,
        };
        if !action_granted && !doc.grants.contains(name) {
            return Err(HarnessError::Workflow(format!(
                "effect `{name}` has no grant"
            )));
        }
        let gate_passed = doc
            .human_gate
            .as_ref()
            .is_some_and(|gate| gate_open(gate, document_policy(doc, &gate.decide), decisions));
        let gate = if doc.tier != RiskTier::Write {
            GateOutcome::NotRequired
        } else if gate_passed {
            GateOutcome::Passed
        } else {
            GateOutcome::Skipped
        };
        events.push_grant(GrantDecision {
            owner: doc.owner.clone(),
            tier: doc.tier,
            tool: name.to_owned(),
            gate,
        });
        if doc.tier == RiskTier::Write && !gate_passed {
            events.push_signal(Signal {
                check: "write_skipped".into(),
                node: node.id().clone(),
                finding: name.to_owned(),
            });
            return Ok((Outcome::Skipped, None));
        }
        if name == "composio"
            && self
                .effects
                .get(name)
                .is_some_and(|effect| effect.provider() != "composio")
        {
            return Err(HarnessError::Workflow(format!(
                "effect `{name}` cannot run"
            )));
        }
        self.step(
            &|id| document_policy(doc, id),
            node,
            state,
            decisions,
            events,
            trace,
        )
        .await
    }

    /// Runs one node. The second value is the low-confidence edge a `Decide`
    /// node must take instead of the transition, if any.
    async fn step(
        &self,
        gate_policy: &impl Fn(&NodeId) -> Option<ConfidencePolicy>,
        node: &Node,
        state: &mut State,
        decisions: &Decisions,
        events: &mut EventLog,
        trace: &mut Trace<'_>,
    ) -> Result<(Outcome, Option<ConfidenceAction>), HarnessError> {
        let outcome = match node {
            Node::Deterministic { step, .. } => {
                step(state)?;
                Outcome::Done
            }
            Node::Decide {
                id,
                reads,
                questions,
                policy,
            } => {
                let req = DecisionRequest {
                    state: slice(state, reads),
                    questions: questions.clone(),
                };
                let (answers, low_edge) = self.decide(id, req, policy, events, trace).await?;
                return Ok((Outcome::Decided(answers), low_edge));
            }
            Node::Generate {
                id,
                task,
                reads,
                output,
                max_tokens,
                gate,
            } => {
                if let Some(gate) = gate
                    && !gate_open(gate, gate_policy(&gate.decide), decisions)
                {
                    return Ok((Outcome::Skipped, None));
                }
                let req = GenRequest {
                    task: task.clone(),
                    context: Value::Object(slice(state, reads).into_iter().collect()),
                    output: output.clone(),
                    max_tokens: *max_tokens,
                };
                let hash = input_hash(&req)?;
                let resp = if let Some(event) = trace.take(id, EventKind::Generate, &hash) {
                    GenResponse {
                        output: fixture_gen(output, &event.output)?,
                        usage: event.usage.clone(),
                    }
                } else {
                    let model = self.generative.as_deref().ok_or_else(|| {
                        HarnessError::Workflow("no generative model configured".into())
                    })?;
                    model.complete(req.clone()).await?
                };
                if let Err(err) = validate_output(output, &resp.output) {
                    if matches!(err, HarnessError::SchemaInvalid(_)) {
                        events.push_signal(Signal {
                            check: "schema_invalid".into(),
                            node: id.clone(),
                            finding: err.to_string(),
                        });
                    }
                    return Err(err);
                }
                events.push(Event {
                    node: id.clone(),
                    name: id.0.clone(),
                    kind: EventKind::Generate,
                    input_hash: hash,
                    output: stored_gen(&resp.output),
                    confidence: None,
                    usage: resp.usage.clone(),
                });
                Outcome::Generated(resp.output)
            }
            Node::Effect {
                id,
                name,
                reads,
                connection,
                action,
            } => {
                let effect = self
                    .effects
                    .get(name)
                    .ok_or_else(|| HarnessError::Workflow(format!("unknown effect `{name}`")))?;
                let capability = effect.capability();
                if !self.granted.contains(&capability) {
                    return Err(HarnessError::CapabilityDenied(capability));
                }
                let mut input_map = slice(state, reads);
                if let Some(connection) = connection {
                    input_map.insert("connection".to_owned(), Value::String(connection.clone()));
                }
                if let Some(action) = action {
                    input_map.insert("action".to_owned(), Value::String(action.clone()));
                }
                let input = Value::Object(input_map.into_iter().collect());
                let hashed = input_hash(&input)?;
                let (output, usage) =
                    if let Some(event) = trace.take(id, EventKind::Effect, &hashed) {
                        (event.output.clone(), event.usage.clone())
                    } else {
                        let started = Instant::now();
                        let output = effect.run(input).await?;
                        (
                            output,
                            Usage {
                                latency_ms: elapsed_ms(started),
                                input_tokens: None,
                                output_tokens: None,
                                cost_usd: None,
                            },
                        )
                    };
                events.push(Event {
                    node: id.clone(),
                    name: name.clone(),
                    kind: EventKind::Effect,
                    input_hash: hashed,
                    output: output.clone(),
                    confidence: None,
                    usage,
                });
                Outcome::Effected(output)
            }
        };
        Ok((outcome, None))
    }

    /// Calls the decision model and applies `policy`. Returns the final
    /// answers and the low-confidence edge to take, if any.
    ///
    /// `RetryStronger` makes one more call to the stronger model with the same
    /// request; if that is still low confidence, the edge is `Escalate`.
    async fn decide(
        &self,
        node: &NodeId,
        req: DecisionRequest,
        policy: &ConfidencePolicy,
        events: &mut EventLog,
        trace: &mut Trace<'_>,
    ) -> Result<(Answers, Option<ConfidenceAction>), HarnessError> {
        let retry = policy.on_low == ConfidenceAction::RetryStronger;
        let resp = resolve_decide(
            self.decision.as_deref(),
            node,
            &req,
            trace,
            "no decision model configured",
        )
        .await?;
        record_decide(node, &req, &resp, events)?;
        let answers = resp.answers;
        if confidence(&answers) >= policy.min_confidence {
            return Ok((answers, None));
        }
        match (policy.on_low, retry) {
            (ConfidenceAction::Continue, _) => Ok((answers, None)),
            (ConfidenceAction::RetryStronger, true) => {
                let resp = resolve_decide(
                    self.stronger_decision.as_deref(),
                    node,
                    &req,
                    trace,
                    "no stronger decision model configured",
                )
                .await?;
                record_decide(node, &req, &resp, events)?;
                let answers = resp.answers;
                if confidence(&answers) >= policy.min_confidence {
                    Ok((answers, None))
                } else {
                    record_low_confidence(events, node, ConfidenceAction::Escalate);
                    Ok((answers, Some(ConfidenceAction::Escalate)))
                }
            }
            (action, _) => {
                record_low_confidence(events, node, action);
                Ok((answers, Some(action)))
            }
        }
    }
}

fn record_low_confidence(events: &mut EventLog, node: &NodeId, action: ConfidenceAction) {
    events.push_signal(Signal {
        check: "confidence_low".into(),
        node: node.clone(),
        finding: match action {
            ConfidenceAction::Escalate => "escalate",
            ConfidenceAction::RetryStronger => "retry_stronger",
            ConfidenceAction::SafeDefault => "safe_default",
            ConfidenceAction::Human => "human",
            ConfidenceAction::Continue => "continue",
        }
        .to_owned(),
    });
}

struct Trace<'a> {
    events: &'a [Event],
    next: usize,
}

impl Trace<'_> {
    /// Consumes the next event when it is this step. A mismatch leaves the
    /// cursor where it is, so a later step can still match that event.
    fn take(&mut self, node: &NodeId, kind: EventKind, hash: &str) -> Option<&Event> {
        let event = self.events.get(self.next)?;
        if &event.node == node && event.kind == kind && event.input_hash == hash {
            self.next += 1;
            Some(event)
        } else {
            None
        }
    }
}

async fn resolve_decide(
    model: Option<&dyn DecisionModel>,
    node: &NodeId,
    req: &DecisionRequest,
    trace: &mut Trace<'_>,
    missing: &str,
) -> Result<DecisionResponse, HarnessError> {
    let hash = input_hash(req)?;
    if let Some(event) = trace.take(node, EventKind::Decide, &hash) {
        return fixture_decide(event);
    }
    let model = model.ok_or_else(|| HarnessError::Workflow(missing.to_owned()))?;
    model.evaluate(req.clone()).await
}

fn fixture_decide(event: &Event) -> Result<DecisionResponse, HarnessError> {
    let answers = serde_json::from_value(event.output.clone()).map_err(|e| {
        HarnessError::Workflow(format!(
            "replay fixture for `{}` is not a decision: {e}",
            event.node
        ))
    })?;
    Ok(DecisionResponse {
        answers,
        usage: event.usage.clone(),
    })
}

fn fixture_gen(spec: &OutputSpec, value: &Value) -> Result<GenOutput, HarnessError> {
    match spec {
        OutputSpec::FreeText => match value.as_str() {
            Some(text) => Ok(GenOutput::Text(text.to_owned())),
            None => Err(HarnessError::Workflow(
                "replay fixture for a free-text generate is not a string".into(),
            )),
        },
        OutputSpec::Schema(_) => Ok(GenOutput::Json(value.clone())),
    }
}

fn record_decide(
    node: &NodeId,
    req: &DecisionRequest,
    resp: &DecisionResponse,
    events: &mut EventLog,
) -> Result<(), HarnessError> {
    let output =
        serde_json::to_value(&resp.answers).map_err(|e| HarnessError::Provider(e.to_string()))?;
    events.push(Event {
        node: node.clone(),
        name: node.0.clone(),
        kind: EventKind::Decide,
        input_hash: input_hash(req)?,
        output,
        confidence: Some(confidence(&resp.answers)),
        usage: resp.usage.clone(),
    });
    Ok(())
}

fn stored_gen(output: &GenOutput) -> Value {
    match output {
        GenOutput::Json(v) => v.clone(),
        GenOutput::Text(s) => Value::String(s.clone()),
    }
}

fn elapsed_ms(started: Instant) -> u64 {
    u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX)
}

/// The lowest confidence across a decision's answers. No answers, or a NaN
/// confidence, counts as 0.0.
fn confidence(answers: &Answers) -> f64 {
    answers
        .values()
        .map(|a| {
            if a.confidence.is_nan() {
                0.0
            } else {
                a.confidence
            }
        })
        .reduce(f64::min)
        .unwrap_or(0.0)
}

/// The subset of `state` named by `reads`. Missing keys are left out.
fn slice(state: &State, reads: &[String]) -> State {
    reads
        .iter()
        .filter_map(|key| state.get(key).map(|v| (key.clone(), v.clone())))
        .collect()
}

/// Open only if the gate's Decide node has run, its top answer to `question`
/// is `allow`, and that answer meets the node's `min_confidence`.
fn gate_open(gate: &Gate, policy: Option<ConfidencePolicy>, decisions: &Decisions) -> bool {
    let Some(policy) = policy else {
        return false;
    };
    decisions
        .get(&gate.decide)
        .and_then(|answers| answers.get(&gate.question))
        .is_some_and(|answer| {
            answer.top() == Some(gate.allow.as_str()) && answer.confidence >= policy.min_confidence
        })
}

fn decide_policy(wf: &Workflow, id: &NodeId) -> Option<ConfidencePolicy> {
    match wf.node(id) {
        Some(Node::Decide { policy, .. }) => Some(*policy),
        _ => None,
    }
}

fn document_policy(doc: &WorkflowDocument, id: &NodeId) -> Option<ConfidencePolicy> {
    doc.nodes.iter().find_map(|node| match node {
        DocumentNode::Decide {
            id: decide, policy, ..
        } if decide == id => Some(*policy),
        _ => None,
    })
}

fn node_from_document(node: &DocumentNode) -> Node {
    match node {
        DocumentNode::Decide {
            id,
            reads,
            questions,
            policy,
        } => Node::Decide {
            id: id.clone(),
            reads: reads.clone(),
            questions: questions.clone(),
            policy: *policy,
        },
        DocumentNode::Generate {
            id,
            task,
            reads,
            output,
            max_tokens,
            gate,
        } => Node::Generate {
            id: id.clone(),
            task: task.clone(),
            reads: reads.clone(),
            output: output.clone(),
            max_tokens: *max_tokens,
            gate: gate.clone(),
        },
        DocumentNode::Effect {
            id,
            name,
            reads,
            connection,
            action,
        } => Node::Effect {
            id: id.clone(),
            name: name.clone(),
            reads: reads.clone(),
            connection: connection.clone(),
            action: action.clone(),
        },
    }
}

fn record_node_output(
    current: &NodeId,
    outcome: &Outcome,
    state: &mut State,
    decisions: &mut Decisions,
) -> Result<(), HarnessError> {
    match outcome {
        Outcome::Decided(answers) => {
            let value = serde_json::to_value(answers)
                .map_err(|err| HarnessError::Provider(err.to_string()))?;
            state.insert(current.0.clone(), value);
            decisions.insert(current.clone(), answers.clone());
        }
        Outcome::Generated(output) => {
            state.insert(current.0.clone(), stored_gen(output));
        }
        Outcome::Effected(value) => {
            state.insert(current.0.clone(), value.clone());
        }
        Outcome::Done | Outcome::Skipped => {}
    }
    Ok(())
}

fn next_edge(
    doc: &WorkflowDocument,
    from: &NodeId,
    outcome: &Outcome,
    state: &State,
) -> Option<NodeId> {
    doc.edges
        .iter()
        .find(|edge| &edge.from == from && when_matches(&edge.when, from, doc, outcome, state))
        .map(|edge| edge.to.clone())
}

fn when_matches(
    when: &BranchWhen,
    from: &NodeId,
    doc: &WorkflowDocument,
    outcome: &Outcome,
    state: &State,
) -> bool {
    match when {
        BranchWhen::Always => true,
        BranchWhen::Label { question, equals } => answer_top(outcome, question) == Some(equals),
        BranchWhen::Other { question } => {
            let Some(top) = answer_top(outcome, question) else {
                return false;
            };
            !doc.edges.iter().any(|edge| {
                &edge.from == from
                    && matches!(
                        &edge.when,
                        BranchWhen::Label { question: q, equals } if q == question && equals == top
                    )
            })
        }
        BranchWhen::Generated => matches!(outcome, Outcome::Generated(_)),
        BranchWhen::Skipped => matches!(outcome, Outcome::Skipped),
        BranchWhen::State { key, equals } => {
            state.get(key).and_then(Value::as_bool) == Some(*equals)
        }
    }
}

fn answer_top<'a>(outcome: &'a Outcome, question: &str) -> Option<&'a str> {
    match outcome {
        Outcome::Decided(answers) => answers.get(question).and_then(Answer::top),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::future::BoxFuture;
    use crate::generate::{GenOutput, GenRequest, GenResponse, GenerativeModel, JsonSchema};
    use crate::workflow::{Next, Workflow};
    use serde_json::json;

    struct TextGen;

    impl GenerativeModel for TextGen {
        fn complete(&self, _req: GenRequest) -> BoxFuture<'_, Result<GenResponse, HarnessError>> {
            Box::pin(async {
                Ok(GenResponse {
                    output: GenOutput::Text("Refund issued.".into()),
                    usage: Usage::default(),
                })
            })
        }
    }

    fn draft() -> Node {
        Node::Generate {
            id: "draft".into(),
            task: "Draft a billing reply".into(),
            reads: vec![],
            output: OutputSpec::Schema(JsonSchema(json!({
                "type": "object",
                "required": ["reply"],
                "properties": { "reply": { "type": "string" } }
            }))),
            max_tokens: 32,
            gate: None,
        }
    }

    #[tokio::test]
    async fn schema_invalid_records_a_signal_before_the_error() {
        let machine = Machine::builder().generative(TextGen).build();
        let node = draft();
        let mut state = State::new();
        let decisions = Decisions::new();
        let mut events = EventLog::default();
        let mut trace = Trace {
            events: &[],
            next: 0,
        };
        let err = machine
            .step(
                &|_| None,
                &node,
                &mut state,
                &decisions,
                &mut events,
                &mut trace,
            )
            .await
            .unwrap_err();
        assert!(matches!(err, HarnessError::SchemaInvalid(_)));
        assert!(events.iter().all(|event| event.kind != EventKind::Generate));
        assert_eq!(events.signals().len(), 1);
        assert_eq!(events.signals()[0].check, "schema_invalid");
        assert_eq!(events.signals()[0].node, "draft".into());
        assert_eq!(events.signals()[0].finding, err.to_string());
    }

    #[tokio::test]
    async fn schema_invalid_run_returns_only_the_error() {
        let machine = Machine::builder().generative(TextGen).build();
        let workflow = Workflow::new("draft".into(), vec![draft()], |_, _, _| Next::End).unwrap();
        let err = machine.run(&workflow, State::new()).await.unwrap_err();
        assert!(matches!(err, HarnessError::SchemaInvalid(_)));
    }
}
