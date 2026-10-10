use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use harness_core::{
    Answer, BoxFuture, ConfidenceAction, ConfidencePolicy, DecisionModel, DecisionRequest,
    DecisionResponse, HarnessError, Machine, Next, Node, NodeId, Outcome, Question, RunStatus,
    State, Usage, Workflow,
};
use serde_json::json;

struct FixedDecision {
    confidence: f64,
    calls: Arc<AtomicUsize>,
}

impl DecisionModel for FixedDecision {
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
                    let answer = Answer {
                        probabilities: BTreeMap::from([("yes".to_owned(), self.confidence)]),
                        confidence: self.confidence,
                    };
                    (key, answer)
                })
                .collect();
            Ok(DecisionResponse {
                answers,
                usage: Usage::default(),
            })
        })
    }
}

fn decision(confidence: f64) -> (FixedDecision, Arc<AtomicUsize>) {
    let calls = Arc::new(AtomicUsize::new(0));
    let model = FixedDecision {
        confidence,
        calls: calls.clone(),
    };
    (model, calls)
}

fn mark(state: &mut State) -> Result<(), HarnessError> {
    state.insert("marked".into(), json!(true));
    Ok(())
}

/// Always sends `route` to `next`, so any other path was taken by the Machine.
fn always_next(node: &NodeId, _: &Outcome, _: &State) -> Next {
    match node.0.as_str() {
        "route" => Next::Goto("next".into()),
        _ => Next::End,
    }
}

fn workflow(on_low: ConfidenceAction) -> Workflow {
    let step = |id: &str| Node::Deterministic {
        id: id.into(),
        step: mark,
    };
    let nodes = vec![
        Node::Decide {
            id: "route".into(),
            reads: vec![],
            questions: BTreeMap::from([(
                "q".into(),
                Question::Noul {
                    instructions: "Is this safe to answer?".into(),
                },
            )]),
            policy: ConfidencePolicy {
                min_confidence: 0.7,
                on_low,
            },
        },
        step("next"),
        step("fallback"),
        step("escalate"),
        step("human"),
    ];
    Workflow::new("route".into(), nodes, always_next).unwrap()
}

fn ids(path: &[NodeId]) -> Vec<&str> {
    path.iter().map(|id| id.0.as_str()).collect()
}

#[tokio::test]
async fn confident_decision_continues_to_transition() {
    let (dm, _) = decision(0.9);
    let machine = Machine::builder().decision(dm).build();

    let run = machine
        .run(&workflow(ConfidenceAction::Escalate), State::new())
        .await
        .unwrap();

    assert_eq!(ids(&run.path), ["route", "next"]);
    assert_eq!(run.status, RunStatus::Completed);
}

#[tokio::test]
async fn explicit_continue_accepts_low_confidence() {
    let (dm, _) = decision(0.3);
    let machine = Machine::builder().decision(dm).build();

    let run = machine
        .run(&workflow(ConfidenceAction::Continue), State::new())
        .await
        .unwrap();

    assert_eq!(ids(&run.path), ["route", "next"]);
    assert_eq!(run.status, RunStatus::Completed);
}

#[tokio::test]
async fn safe_default_goes_to_its_handler() {
    let (dm, _) = decision(0.3);
    let machine = Machine::builder().decision(dm).build();
    let wf = workflow(ConfidenceAction::SafeDefault)
        .on_low(ConfidenceAction::SafeDefault, "fallback".into())
        .unwrap();

    let run = machine.run(&wf, State::new()).await.unwrap();

    assert_eq!(ids(&run.path), ["route", "fallback"]);
    assert_eq!(run.status, RunStatus::Completed);
}

#[tokio::test]
async fn escalate_without_handler_halts() {
    let (dm, _) = decision(0.3);
    let machine = Machine::builder().decision(dm).build();

    let run = machine
        .run(&workflow(ConfidenceAction::Escalate), State::new())
        .await
        .unwrap();

    assert_eq!(ids(&run.path), ["route"]);
    assert_eq!(run.status, RunStatus::Halted(ConfidenceAction::Escalate));
    assert!(!run.state.contains_key("marked"));
}

#[tokio::test]
async fn human_goes_to_its_handler() {
    let (dm, _) = decision(0.3);
    let machine = Machine::builder().decision(dm).build();
    let wf = workflow(ConfidenceAction::Human)
        .on_low(ConfidenceAction::Human, "human".into())
        .unwrap();

    let run = machine.run(&wf, State::new()).await.unwrap();

    assert_eq!(ids(&run.path), ["route", "human"]);
}

#[tokio::test]
async fn retry_stronger_uses_the_confident_second_answer() {
    let (weak, weak_calls) = decision(0.4);
    let (strong, strong_calls) = decision(0.95);
    let machine = Machine::builder()
        .decision(weak)
        .stronger_decision(strong)
        .build();

    let run = machine
        .run(&workflow(ConfidenceAction::RetryStronger), State::new())
        .await
        .unwrap();

    assert_eq!(ids(&run.path), ["route", "next"]);
    assert_eq!(weak_calls.load(Ordering::SeqCst), 1);
    assert_eq!(strong_calls.load(Ordering::SeqCst), 1);
    assert_eq!(run.state["route"]["q"]["confidence"], json!(0.95));
    assert_eq!(run.state["route"]["q"]["probabilities"]["yes"], json!(0.95));
}

#[tokio::test]
async fn retry_stronger_still_low_escalates() {
    let (weak, _) = decision(0.4);
    let (strong, strong_calls) = decision(0.5);
    let machine = Machine::builder()
        .decision(weak)
        .stronger_decision(strong)
        .build();
    let wf = workflow(ConfidenceAction::RetryStronger)
        .on_low(ConfidenceAction::Escalate, "escalate".into())
        .unwrap();

    let run = machine.run(&wf, State::new()).await.unwrap();

    assert_eq!(ids(&run.path), ["route", "escalate"]);
    assert_eq!(strong_calls.load(Ordering::SeqCst), 1);
    assert_eq!(run.state["route"]["q"]["confidence"], json!(0.5));
}

#[tokio::test]
async fn retry_stronger_needs_a_stronger_model() {
    let (weak, _) = decision(0.4);
    let machine = Machine::builder().decision(weak).build();

    let err = machine
        .run(&workflow(ConfidenceAction::RetryStronger), State::new())
        .await
        .unwrap_err();

    assert!(matches!(err, HarnessError::Workflow(_)), "{err}");
}

#[test]
fn on_low_rejects_non_edges_and_unknown_handlers() {
    for action in [ConfidenceAction::Continue, ConfidenceAction::RetryStronger] {
        let result = workflow(ConfidenceAction::Escalate).on_low(action, "fallback".into());
        assert!(matches!(result, Err(HarnessError::Workflow(_))));
    }
    let unknown =
        workflow(ConfidenceAction::Escalate).on_low(ConfidenceAction::Escalate, "missing".into());
    assert!(matches!(unknown, Err(HarnessError::Workflow(_))));
}
