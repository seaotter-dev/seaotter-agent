use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use harness_core::{
    Answer, BoxFuture, Capability, ConfidenceAction, ConfidencePolicy, DecisionModel,
    DecisionRequest, DecisionResponse, Effect, EventKind, EventLog, Gate, GenOutput, GenRequest,
    GenResponse, GenerativeModel, HarnessError, JsonSchema, Machine, Next, Node, NodeId, Outcome,
    OutputSpec, Question, RunStatus, State, Usage, Workflow,
};
use serde_json::{Value, json};

fn billed() -> Usage {
    Usage {
        latency_ms: 12,
        input_tokens: Some(40),
        output_tokens: Some(8),
        cost_usd: Some(0.0001),
    }
}

struct ScriptedDecision {
    top: &'static str,
    confidence: f64,
    usage: Usage,
}

impl DecisionModel for ScriptedDecision {
    fn evaluate(
        &self,
        req: DecisionRequest,
    ) -> BoxFuture<'_, Result<DecisionResponse, HarnessError>> {
        Box::pin(async move {
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
                usage: self.usage.clone(),
            })
        })
    }
}

struct ScriptedGen {
    output: GenOutput,
    usage: Usage,
    calls: Arc<AtomicUsize>,
}

impl GenerativeModel for ScriptedGen {
    fn complete(&self, _req: GenRequest) -> BoxFuture<'_, Result<GenResponse, HarnessError>> {
        Box::pin(async move {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Ok(GenResponse {
                output: self.output.clone(),
                usage: self.usage.clone(),
            })
        })
    }
}

struct EchoEffect;

impl Effect for EchoEffect {
    fn name(&self) -> &str {
        "echo"
    }

    fn capability(&self) -> Capability {
        Capability::HttpGet
    }

    fn run(&self, input: Value) -> BoxFuture<'_, Result<Value, HarnessError>> {
        Box::pin(async move { Ok(input) })
    }
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

fn route_question() -> Question {
    Question::Choice {
        instructions: "Which team owns this ticket?".into(),
        criteria: BTreeMap::from([
            ("billing".into(), "Payments and invoices".into()),
            ("technical".into(), "Bugs and outages".into()),
        ]),
    }
}

fn reply_schema() -> OutputSpec {
    OutputSpec::Schema(JsonSchema(json!({
        "type": "object",
        "required": ["reply"],
        "properties": { "reply": { "type": "string" } },
    })))
}

fn mixed(on_low: ConfidenceAction, gate: bool) -> Workflow {
    let nodes = vec![
        Node::Deterministic {
            id: "normalize".into(),
            step: normalize,
        },
        Node::Decide {
            id: "route".into(),
            reads: vec!["subject".into()],
            questions: BTreeMap::from([("route".into(), route_question())]),
            policy: ConfidencePolicy {
                min_confidence: 0.7,
                on_low,
            },
        },
        Node::Generate {
            id: "draft".into(),
            task: "Draft a billing reply".into(),
            reads: vec!["subject".into()],
            output: reply_schema(),
            max_tokens: 128,
            gate: gate.then(|| Gate {
                decide: "route".into(),
                question: "route".into(),
                allow: "billing".into(),
            }),
        },
        Node::Effect {
            id: "fetch".into(),
            name: "echo".into(),
            reads: vec!["subject".into()],
        },
    ];
    Workflow::new(
        "normalize".into(),
        nodes,
        |node: &NodeId, outcome: &Outcome, _: &State| match (node.0.as_str(), outcome) {
            ("normalize", _) => Next::Goto("route".into()),
            ("route", _) => Next::Goto("draft".into()),
            ("draft", _) => Next::Goto("fetch".into()),
            _ => Next::End,
        },
    )
    .unwrap()
}

fn ticket() -> State {
    State::from([("subject".into(), json!("  Refund PLEASE "))])
}

fn machine(confidence: f64) -> (Machine, Arc<AtomicUsize>) {
    let calls = Arc::new(AtomicUsize::new(0));
    let machine = Machine::builder()
        .decision(ScriptedDecision {
            top: "billing",
            confidence,
            usage: billed(),
        })
        .generative(ScriptedGen {
            output: GenOutput::Json(json!({"reply": "Refund issued."})),
            usage: billed(),
            calls: calls.clone(),
        })
        .effect(EchoEffect)
        .grant(Capability::HttpGet)
        .build();
    (machine, calls)
}

#[test]
fn log_starts_empty_and_is_only_read() {
    let log = EventLog::default();
    assert!(log.is_empty());
    assert_eq!(log.len(), 0);
    assert!(log.as_slice().is_empty());
    assert_eq!(log.iter().count(), 0);
}

#[tokio::test]
async fn mixed_workflow_records_one_event_per_ai_call_and_effect() {
    let (machine, calls) = machine(0.9);
    let run = machine
        .run(&mixed(ConfidenceAction::Continue, false), ticket())
        .await
        .unwrap();

    let events = run.events.as_slice();
    assert_eq!(events.len(), 3);
    assert_eq!(
        events
            .iter()
            .map(|event| (event.node.0.as_str(), event.kind))
            .collect::<Vec<_>>(),
        vec![
            ("route", EventKind::Decide),
            ("draft", EventKind::Generate),
            ("fetch", EventKind::Effect),
        ]
    );
    assert!(
        !events.iter().any(|event| event.node.0 == "normalize"),
        "deterministic nodes are omitted"
    );

    assert_eq!(events[0].output, run.state["route"]);
    assert_eq!(events[0].confidence, Some(0.9));
    assert_eq!(events[0].usage, billed());

    assert_eq!(events[1].output, run.state["draft"]);
    assert_eq!(events[1].output, json!({"reply": "Refund issued."}));
    assert_eq!(events[1].confidence, None);
    assert_eq!(events[1].usage, billed());
    assert_eq!(calls.load(Ordering::SeqCst), 1);

    assert_eq!(events[2].output, run.state["fetch"]);
    assert_eq!(events[2].output, json!({"subject": "refund please"}));
    assert_eq!(events[2].confidence, None);
    assert_eq!(events[2].usage.input_tokens, None);
    assert_eq!(events[2].usage.output_tokens, None);
    assert_eq!(events[2].usage.cost_usd, None);
}

#[tokio::test]
async fn input_hash_is_stable_and_tracks_the_read_state() {
    let (first, _) = machine(0.9);
    let (second, _) = machine(0.9);
    let workflow = mixed(ConfidenceAction::Continue, false);
    let one = first.run(&workflow, ticket()).await.unwrap();
    let two = second.run(&workflow, ticket()).await.unwrap();
    assert_eq!(
        one.events.as_slice()[0].input_hash,
        two.events.as_slice()[0].input_hash
    );

    let mut changed = ticket();
    changed.insert("subject".into(), json!("something else"));
    let (third, _) = machine(0.9);
    let three = third.run(&workflow, changed).await.unwrap();
    assert_ne!(
        one.events.as_slice()[0].input_hash,
        three.events.as_slice()[0].input_hash
    );
}

#[tokio::test]
async fn closed_gate_writes_no_generate_event() {
    let (machine, calls) = machine(0.4);
    let run = machine
        .run(&mixed(ConfidenceAction::Continue, true), ticket())
        .await
        .unwrap();

    assert!(run.path.iter().any(|id| id.0 == "draft"));
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    let events = run.events.as_slice();
    assert!(events.iter().all(|event| event.kind != EventKind::Generate));
    assert_eq!(events.len(), 2);
    assert_eq!(events[0].kind, EventKind::Decide);
    assert_eq!(events[1].kind, EventKind::Effect);
}

#[tokio::test]
async fn retry_stronger_records_both_calls() {
    let weak = ScriptedDecision {
        top: "billing",
        confidence: 0.4,
        usage: billed(),
    };
    let mut strong_usage = billed();
    strong_usage.latency_ms = 30;
    let strong = ScriptedDecision {
        top: "billing",
        confidence: 0.95,
        usage: strong_usage.clone(),
    };
    let machine = Machine::builder()
        .decision(weak)
        .stronger_decision(strong)
        .build();
    let nodes = vec![
        Node::Decide {
            id: "route".into(),
            reads: vec!["subject".into()],
            questions: BTreeMap::from([("route".into(), route_question())]),
            policy: ConfidencePolicy {
                min_confidence: 0.7,
                on_low: ConfidenceAction::RetryStronger,
            },
        },
        Node::Deterministic {
            id: "next".into(),
            step: |_state| Ok(()),
        },
    ];
    let workflow = Workflow::new("route".into(), nodes, |node: &NodeId, _, _| {
        if node.0 == "route" {
            Next::Goto("next".into())
        } else {
            Next::End
        }
    })
    .unwrap();

    let run = machine.run(&workflow, ticket()).await.unwrap();
    let events = run.events.as_slice();
    assert_eq!(events.len(), 2);
    assert!(events.iter().all(|event| event.kind == EventKind::Decide));
    assert!(events.iter().all(|event| event.node.0 == "route"));
    assert_eq!(events[0].input_hash, events[1].input_hash);
    assert_eq!(events[0].confidence, Some(0.4));
    assert_eq!(events[0].usage, billed());
    assert_eq!(events[1].confidence, Some(0.95));
    assert_eq!(events[1].usage, strong_usage);
    assert_eq!(run.state["route"]["route"]["confidence"], json!(0.95));
}

#[tokio::test]
async fn halted_run_keeps_its_decide_event() {
    let (machine, _) = machine(0.3);
    let nodes = vec![Node::Decide {
        id: "route".into(),
        reads: vec![],
        questions: BTreeMap::from([("route".into(), route_question())]),
        policy: ConfidencePolicy {
            min_confidence: 0.7,
            on_low: ConfidenceAction::Escalate,
        },
    }];
    let workflow = Workflow::new("route".into(), nodes, |_, _, _| Next::End).unwrap();
    let run = machine.run(&workflow, State::new()).await.unwrap();
    assert_eq!(run.status, RunStatus::Halted(ConfidenceAction::Escalate));
    assert_eq!(run.events.len(), 1);
    assert_eq!(run.events.as_slice()[0].kind, EventKind::Decide);
    assert_eq!(run.events.as_slice()[0].confidence, Some(0.3));
}

#[tokio::test]
async fn denied_effect_returns_the_error_without_a_run() {
    let machine = Machine::builder().effect(EchoEffect).build();
    let nodes = vec![Node::Effect {
        id: "fetch".into(),
        name: "echo".into(),
        reads: vec!["subject".into()],
    }];
    let workflow = Workflow::new("fetch".into(), nodes, |_, _, _| Next::End).unwrap();
    let err = machine.run(&workflow, ticket()).await.unwrap_err();
    assert_eq!(err, HarnessError::CapabilityDenied(Capability::HttpGet));
}
