use std::collections::BTreeMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use harness_core::{
    Answer, BoxFuture, Capability, ConfidenceAction, ConfidencePolicy, DecisionModel,
    DecisionRequest, DecisionResponse, Effect, Gate, GenOutput, GenRequest, GenResponse,
    GenerativeModel, HarnessError, JsonSchema, Machine, Next, Node, NodeId, Outcome, OutputSpec,
    Question, RunStatus, State, Usage, Workflow,
};
use serde_json::{Value, json};

struct ScriptedDecision {
    top: &'static str,
    confidence: f64,
    seen: Arc<Mutex<Vec<DecisionRequest>>>,
}

impl DecisionModel for ScriptedDecision {
    fn evaluate(
        &self,
        req: DecisionRequest,
    ) -> BoxFuture<'_, Result<DecisionResponse, HarnessError>> {
        Box::pin(async move {
            let answers = req
                .questions
                .keys()
                .map(|key| {
                    let answer = Answer {
                        probabilities: BTreeMap::from([(self.top.to_owned(), self.confidence)]),
                        confidence: self.confidence,
                    };
                    (key.clone(), answer)
                })
                .collect();
            self.seen.lock().unwrap().push(req);
            Ok(DecisionResponse {
                answers,
                usage: Usage::default(),
            })
        })
    }
}

struct ScriptedGen {
    output: GenOutput,
    calls: Arc<AtomicUsize>,
}

impl GenerativeModel for ScriptedGen {
    fn complete(&self, _req: GenRequest) -> BoxFuture<'_, Result<GenResponse, HarnessError>> {
        Box::pin(async move {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Ok(GenResponse {
                output: self.output.clone(),
                usage: Usage::default(),
            })
        })
    }
}

struct EchoEffect {
    calls: Arc<AtomicUsize>,
}

impl Effect for EchoEffect {
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

fn decision(
    top: &'static str,
    confidence: f64,
) -> (ScriptedDecision, Arc<Mutex<Vec<DecisionRequest>>>) {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let model = ScriptedDecision {
        top,
        confidence,
        seen: seen.clone(),
    };
    (model, seen)
}

fn generator(output: GenOutput) -> (ScriptedGen, Arc<AtomicUsize>) {
    let calls = Arc::new(AtomicUsize::new(0));
    let model = ScriptedGen {
        output,
        calls: calls.clone(),
    };
    (model, calls)
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

fn escalate(state: &mut State) -> Result<(), HarnessError> {
    state.insert("escalated".into(), json!(true));
    Ok(())
}

fn router(node: &NodeId, outcome: &Outcome, _state: &State) -> Next {
    match (node.0.as_str(), outcome) {
        ("normalize", _) => Next::Goto("route".into()),
        ("route", Outcome::Decided(answers)) if answers["route"].top() == Some("billing") => {
            Next::Goto("draft".into())
        }
        ("route", _) => Next::Goto("escalate".into()),
        ("draft", Outcome::Skipped) => Next::Goto("escalate".into()),
        _ => Next::End,
    }
}

fn reply_schema() -> OutputSpec {
    OutputSpec::Schema(JsonSchema(json!({
        "type": "object",
        "required": ["reply"],
        "properties": { "reply": { "type": "string" } },
    })))
}

fn ticket_workflow(on_low: ConfidenceAction) -> Workflow {
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
            output: reply_schema(),
            max_tokens: 128,
            gate: Some(Gate {
                decide: "route".into(),
                question: "route".into(),
                allow: "billing".into(),
            }),
        },
        Node::Deterministic {
            id: "escalate".into(),
            step: escalate,
        },
    ];
    Workflow::new("normalize".into(), nodes, router).unwrap()
}

fn ticket_state() -> State {
    State::from([
        ("subject".into(), json!("  Refund PLEASE ")),
        ("customer_secret".into(), json!("do-not-send")),
    ])
}

fn ids(path: &[NodeId]) -> Vec<&str> {
    path.iter().map(|id| id.0.as_str()).collect()
}

#[tokio::test]
async fn mixed_workflow_runs_deterministic_decide_and_generate() {
    let (dm, _) = decision("billing", 0.9);
    let (gm, calls) = generator(GenOutput::Json(json!({"reply": "Refund issued."})));
    let machine = Machine::builder().decision(dm).generative(gm).build();

    let run = machine
        .run(&ticket_workflow(ConfidenceAction::Escalate), ticket_state())
        .await
        .unwrap();

    assert_eq!(ids(&run.path), ["normalize", "route", "draft"]);
    assert_eq!(run.state["subject"], json!("refund please"));
    assert_eq!(run.state["route"]["route"]["confidence"], json!(0.9));
    assert_eq!(run.state["draft"], json!({"reply": "Refund issued."}));
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert_eq!(run.status, RunStatus::Completed);
}

#[tokio::test]
async fn decide_receives_only_its_state_slice() {
    let (dm, seen) = decision("billing", 0.9);
    let (gm, _) = generator(GenOutput::Json(json!({"reply": "ok"})));
    let machine = Machine::builder().decision(dm).generative(gm).build();

    machine
        .run(&ticket_workflow(ConfidenceAction::Escalate), ticket_state())
        .await
        .unwrap();

    let seen = seen.lock().unwrap();
    let keys: Vec<_> = seen[0].state.keys().map(String::as_str).collect();
    assert_eq!(keys, ["subject"]);
}

#[tokio::test]
async fn closed_gate_skips_generation() {
    let (dm, _) = decision("billing", 0.5);
    let (gm, calls) = generator(GenOutput::Json(json!({"reply": "unused"})));
    let machine = Machine::builder().decision(dm).generative(gm).build();

    // `Continue` accepts the low-confidence answer, so the run reaches `draft`
    // and the gate closes on confidence.
    let run = machine
        .run(&ticket_workflow(ConfidenceAction::Continue), ticket_state())
        .await
        .unwrap();

    assert_eq!(ids(&run.path), ["normalize", "route", "draft", "escalate"]);
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    assert!(!run.state.contains_key("draft"));
    assert_eq!(run.state["escalated"], json!(true));
    assert_eq!(run.status, RunStatus::Completed);
}

#[tokio::test]
async fn schema_invalid_output_stops_the_run() {
    for output in [
        GenOutput::Json(json!({"reply": 5})),
        GenOutput::Text("Refund issued.".into()),
    ] {
        let (dm, _) = decision("billing", 0.9);
        let (gm, _) = generator(output);
        let machine = Machine::builder().decision(dm).generative(gm).build();

        let err = machine
            .run(&ticket_workflow(ConfidenceAction::Escalate), ticket_state())
            .await
            .unwrap_err();
        assert!(matches!(err, HarnessError::SchemaInvalid(_)), "{err}");
    }
}

fn free_text_workflow() -> Workflow {
    let nodes = vec![Node::Generate {
        id: "note".into(),
        task: "Write a note".into(),
        reads: vec![],
        output: OutputSpec::FreeText,
        max_tokens: 32,
        gate: None,
    }];
    Workflow::new("note".into(), nodes, |_, _, _| Next::End).unwrap()
}

#[tokio::test]
async fn explicit_free_text_accepts_text() {
    let (gm, _) = generator(GenOutput::Text("hello".into()));
    let machine = Machine::builder().generative(gm).build();

    let run = machine
        .run(&free_text_workflow(), State::new())
        .await
        .unwrap();
    assert_eq!(run.state["note"], json!("hello"));
    assert_eq!(run.status, RunStatus::Completed);
}

fn effect_workflow() -> Workflow {
    let nodes = vec![Node::Effect {
        id: "fetch".into(),
        name: "echo".into(),
        reads: vec!["subject".into()],
    }];
    Workflow::new("fetch".into(), nodes, |_, _, _| Next::End).unwrap()
}

#[tokio::test]
async fn effect_requires_granted_capability() {
    let calls = Arc::new(AtomicUsize::new(0));
    let denied = Machine::builder()
        .effect(EchoEffect {
            calls: calls.clone(),
        })
        .build();
    let err = denied
        .run(&effect_workflow(), ticket_state())
        .await
        .unwrap_err();
    assert_eq!(err, HarnessError::CapabilityDenied(Capability::HttpGet));
    assert_eq!(calls.load(Ordering::SeqCst), 0);

    let granted = Machine::builder()
        .effect(EchoEffect {
            calls: calls.clone(),
        })
        .grant(Capability::HttpGet)
        .build();
    let run = granted
        .run(&effect_workflow(), ticket_state())
        .await
        .unwrap();
    assert_eq!(run.state["fetch"], json!({"subject": "  Refund PLEASE "}));
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert_eq!(run.status, RunStatus::Completed);
}

#[tokio::test]
async fn misconfiguration_is_a_workflow_error() {
    let err = Machine::builder()
        .build()
        .run(&effect_workflow(), State::new())
        .await
        .unwrap_err();
    assert!(matches!(err, HarnessError::Workflow(_)), "{err}");

    let err = Machine::builder()
        .build()
        .run(&ticket_workflow(ConfidenceAction::Escalate), ticket_state())
        .await
        .unwrap_err();
    assert!(matches!(err, HarnessError::Workflow(_)), "{err}");
}

#[test]
fn workflow_new_rejects_bad_graphs() {
    let step = |id: &str| Node::Deterministic {
        id: id.into(),
        step: escalate,
    };
    let end = |_: &NodeId, _: &Outcome, _: &State| Next::End;

    let duplicate = Workflow::new("a".into(), vec![step("a"), step("a")], end);
    assert!(matches!(duplicate, Err(HarnessError::Workflow(_))));

    let missing_start = Workflow::new("b".into(), vec![step("a")], end);
    assert!(matches!(missing_start, Err(HarnessError::Workflow(_))));

    let bad_gate = Workflow::new(
        "a".into(),
        vec![
            step("a"),
            Node::Generate {
                id: "g".into(),
                task: "t".into(),
                reads: vec![],
                output: OutputSpec::FreeText,
                max_tokens: 8,
                gate: Some(Gate {
                    decide: "a".into(),
                    question: "q".into(),
                    allow: "yes".into(),
                }),
            },
        ],
        end,
    );
    assert!(matches!(bad_gate, Err(HarnessError::Workflow(_))));
}

#[tokio::test]
async fn cycles_are_bounded_by_max_steps() {
    let nodes = vec![Node::Deterministic {
        id: "spin".into(),
        step: escalate,
    }];
    let wf = Workflow::new("spin".into(), nodes, |_, _, _| Next::Goto("spin".into())).unwrap();
    let machine = Machine::builder().max_steps(5).build();

    let fut = machine.run(&wf, State::new());
    assert_send(&fut);
    assert_eq!(fut.await.unwrap_err(), HarnessError::StepLimit(5));
}

fn assert_send<T: Send>(_: &T) {}
