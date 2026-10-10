use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use harness_core::{
    Answer, BoxFuture, Capability, ConfidenceAction, ConfidencePolicy, DecisionModel,
    DecisionRequest, DecisionResponse, Divergence, Effect, GenOutput, GenRequest, GenResponse,
    GenerativeModel, HarnessError, JsonSchema, Machine, Next, Node, NodeId, Outcome, OutputSpec,
    Question, Run, State, Usage, Workflow, diff_runs,
};
use serde_json::{Value, json};

struct CountingDecision {
    top: &'static str,
    confidence: f64,
    calls: Arc<AtomicUsize>,
}

impl DecisionModel for CountingDecision {
    fn evaluate(
        &self,
        req: DecisionRequest,
    ) -> BoxFuture<'_, Result<DecisionResponse, HarnessError>> {
        Box::pin(async move {
            self.calls.fetch_add(1, Ordering::SeqCst);
            let answers = req
                .questions
                .into_keys()
                .map(|key| {
                    (
                        key,
                        Answer {
                            probabilities: BTreeMap::from([(self.top.to_owned(), self.confidence)]),
                            confidence: self.confidence,
                        },
                    )
                })
                .collect();
            Ok(DecisionResponse {
                answers,
                usage: Usage {
                    latency_ms: 12,
                    input_tokens: Some(40),
                    output_tokens: Some(8),
                    cost_usd: Some(0.0001),
                },
            })
        })
    }
}

struct CountingGen {
    reply: &'static str,
    calls: Arc<AtomicUsize>,
}

impl GenerativeModel for CountingGen {
    fn complete(&self, _req: GenRequest) -> BoxFuture<'_, Result<GenResponse, HarnessError>> {
        Box::pin(async move {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Ok(GenResponse {
                output: GenOutput::Json(json!({"reply": self.reply})),
                usage: Usage {
                    latency_ms: 20,
                    input_tokens: Some(10),
                    output_tokens: Some(4),
                    cost_usd: Some(0.0002),
                },
            })
        })
    }
}

struct CountingEffect {
    calls: Arc<AtomicUsize>,
}

impl Effect for CountingEffect {
    fn name(&self) -> &str {
        "echo"
    }

    fn capability(&self) -> Capability {
        Capability::HttpGet
    }

    fn run(&self, input: Value) -> BoxFuture<'_, Result<Value, HarnessError>> {
        Box::pin(async move {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Ok(input)
        })
    }
}

fn calls() -> Arc<AtomicUsize> {
    Arc::new(AtomicUsize::new(0))
}

fn decision(top: &'static str, confidence: f64, calls: Arc<AtomicUsize>) -> CountingDecision {
    CountingDecision {
        top,
        confidence,
        calls,
    }
}

fn workflow(on_low: ConfidenceAction) -> Workflow {
    let nodes = vec![
        Node::Deterministic {
            id: "normalize".into(),
            step: normalize,
        },
        Node::Decide {
            id: "route".into(),
            reads: vec!["subject".into()],
            questions: BTreeMap::from([(
                "route".into(),
                Question::Choice {
                    instructions: "Which team owns this ticket?".into(),
                    criteria: BTreeMap::from([
                        ("billing".into(), "Payments and invoices".into()),
                        ("technical".into(), "Bugs and outages".into()),
                    ]),
                },
            )]),
            policy: ConfidencePolicy {
                min_confidence: 0.7,
                on_low,
            },
        },
        Node::Generate {
            id: "draft".into(),
            task: "Draft a billing reply".into(),
            reads: vec!["subject".into()],
            output: OutputSpec::Schema(JsonSchema(json!({
                "type": "object",
                "required": ["reply"],
                "properties": { "reply": { "type": "string" } },
            }))),
            max_tokens: 128,
            gate: None,
        },
        Node::Effect {
            id: "fetch".into(),
            name: "echo".into(),
            reads: vec!["subject".into()],
        },
        Node::Deterministic {
            id: "escalate".into(),
            step: |state: &mut State| {
                state.insert("escalated".into(), json!(true));
                Ok(())
            },
        },
    ];
    Workflow::new("normalize".into(), nodes, transition)
        .unwrap()
        .on_low(ConfidenceAction::Escalate, "escalate".into())
        .unwrap()
}

fn normalize(state: &mut State) -> Result<(), HarnessError> {
    let subject = state
        .get("subject")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .trim()
        .to_lowercase();
    state.insert("subject".into(), json!(subject));
    Ok(())
}

fn transition(node: &NodeId, outcome: &Outcome, _: &State) -> Next {
    match (node.0.as_str(), outcome) {
        ("normalize", _) => Next::Goto("route".into()),
        ("route", Outcome::Decided(answers)) if answers["route"].top() == Some("billing") => {
            Next::Goto("draft".into())
        }
        ("route", _) => Next::Goto("escalate".into()),
        ("draft", _) => Next::Goto("fetch".into()),
        _ => Next::End,
    }
}

fn ticket() -> State {
    State::from([("subject".into(), json!("  Refund PLEASE "))])
}

fn machine(
    top: &'static str,
    confidence: f64,
    reply: &'static str,
    granted: bool,
) -> (
    Machine,
    Arc<AtomicUsize>,
    Arc<AtomicUsize>,
    Arc<AtomicUsize>,
) {
    let decisions = calls();
    let generates = calls();
    let effects = calls();
    let mut builder = Machine::builder()
        .decision(decision(top, confidence, decisions.clone()))
        .generative(CountingGen {
            reply,
            calls: generates.clone(),
        })
        .effect(CountingEffect {
            calls: effects.clone(),
        });
    if granted {
        builder = builder.grant(Capability::HttpGet);
    }
    (builder.build(), decisions, generates, effects)
}

fn outputs(run: &Run) -> Vec<Value> {
    run.events
        .as_slice()
        .iter()
        .map(|event| event.output.clone())
        .collect()
}

fn ids(run: &Run) -> Vec<&str> {
    run.path.iter().map(|id| id.0.as_str()).collect()
}

#[tokio::test]
async fn replay_reproduces_the_branch_without_live_calls() {
    let workflow = workflow(ConfidenceAction::Escalate);
    let (original_machine, _, _, _) = machine("billing", 0.9, "Refund issued.", true);
    let original = original_machine.run(&workflow, ticket()).await.unwrap();
    assert_eq!(ids(&original), ["normalize", "route", "draft", "fetch"]);

    let (replay_machine, decisions, generates, effects) =
        machine("technical", 0.99, "different reply", true);
    let replayed = replay_machine
        .replay(&workflow, ticket(), original.events.as_slice())
        .await
        .unwrap();

    assert_eq!(replayed.path, original.path);
    assert_eq!(replayed.state, original.state);
    assert_eq!(outputs(&replayed), outputs(&original));
    assert_eq!(decisions.load(Ordering::SeqCst), 0);
    assert_eq!(generates.load(Ordering::SeqCst), 0);
    assert_eq!(effects.load(Ordering::SeqCst), 0);
    assert!(diff_runs(&original, &replayed).is_empty());
}

#[tokio::test]
async fn replay_reproduces_the_low_confidence_branch() {
    let workflow = workflow(ConfidenceAction::Escalate);
    let (original_machine, _, _, _) = machine("billing", 0.4, "unused", true);
    let original = original_machine.run(&workflow, ticket()).await.unwrap();
    assert_eq!(ids(&original), ["normalize", "route", "escalate"]);
    assert_eq!(original.state["escalated"], json!(true));

    let (replay_machine, decisions, _, _) = machine("billing", 0.95, "live reply", true);
    let replayed = replay_machine
        .replay(&workflow, ticket(), original.events.as_slice())
        .await
        .unwrap();

    assert_eq!(replayed.path, original.path);
    assert_eq!(replayed.state["escalated"], json!(true));
    assert!(!replayed.state.contains_key("draft"));
    assert_eq!(decisions.load(Ordering::SeqCst), 0);
    assert!(diff_runs(&original, &replayed).is_empty());
}

#[tokio::test]
async fn uncovered_steps_fall_through_to_live_calls() {
    let workflow = workflow(ConfidenceAction::Escalate);
    let (original_machine, _, _, _) = machine("billing", 0.9, "Refund issued.", true);
    let original = original_machine.run(&workflow, ticket()).await.unwrap();
    let decide_only = &original.events.as_slice()[..1];

    let (replay_machine, decisions, generates, effects) =
        machine("technical", 0.2, "live reply", true);
    let replayed = replay_machine
        .replay(&workflow, ticket(), decide_only)
        .await
        .unwrap();

    assert_eq!(decisions.load(Ordering::SeqCst), 0);
    assert_eq!(generates.load(Ordering::SeqCst), 1);
    assert_eq!(effects.load(Ordering::SeqCst), 1);
    assert_eq!(replayed.state["draft"], json!({"reply": "live reply"}));
    assert_eq!(replayed.state["fetch"], json!({"subject": "refund please"}));
}

#[tokio::test]
async fn covered_effect_still_requires_the_capability() {
    let workflow = workflow(ConfidenceAction::Escalate);
    let (original_machine, _, _, _) = machine("billing", 0.9, "Refund issued.", true);
    let original = original_machine.run(&workflow, ticket()).await.unwrap();

    let (replay_machine, _, _, effects) = machine("billing", 0.9, "Refund issued.", false);
    let err = replay_machine
        .replay(&workflow, ticket(), original.events.as_slice())
        .await
        .unwrap_err();
    assert_eq!(err, HarnessError::CapabilityDenied(Capability::HttpGet));
    assert_eq!(effects.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn diff_reports_branches_confidences_and_outputs() {
    let workflow = workflow(ConfidenceAction::Escalate);
    let (high_machine, _, _, _) = machine("billing", 0.9, "Refund issued.", true);
    let high = high_machine.run(&workflow, ticket()).await.unwrap();
    let (low_machine, _, _, _) = machine("billing", 0.4, "unused", true);
    let low = low_machine.run(&workflow, ticket()).await.unwrap();

    let diverged = diff_runs(&high, &low);
    assert!(
        diverged
            .iter()
            .any(|item| matches!(item, Divergence::Branch { .. }))
    );
    assert!(
        diverged
            .iter()
            .any(|item| matches!(item, Divergence::Confidence { .. }))
    );

    let (other_machine, _, _, _) = machine("billing", 0.9, "A different reply.", true);
    let other = other_machine.run(&workflow, ticket()).await.unwrap();
    let outputs = diff_runs(&high, &other);
    assert!(
        outputs
            .iter()
            .any(|item| matches!(item, Divergence::Output { .. }))
    );
    assert!(
        outputs
            .iter()
            .all(|item| !matches!(item, Divergence::Branch { .. }))
    );
    assert_eq!(high.path, other.path);
}
