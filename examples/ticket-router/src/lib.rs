//! Support-ticket router.
//!
//! A deterministic step normalizes the ticket. A System One `Choice` picks
//! `billing`, `technical`, or `account`. Rust matches that label. Below the
//! confidence minimum, the machine takes the safe-default edge to `queue`.

use std::collections::BTreeMap;

use harness_core::{
    Answer, BoxFuture, ConfidenceAction, ConfidencePolicy, DecisionModel, DecisionRequest,
    DecisionResponse, EventKind, HarnessError, Machine, Next, Node, NodeId, Outcome, Question, Run,
    State, Usage, Workflow,
};
use harness_providers::{TypeSafeConfig, TypeSafeDecision};
use serde::Deserialize;
use serde_json::{Value, json};

/// Checked-in ticket, recorded answer, and the usage of that sample.
#[derive(Debug, Clone, Deserialize)]
pub struct Fixture {
    pub ticket: Ticket,
    pub answer: Answer,
    pub usage: Usage,
}

/// Subject and body of a support ticket, before normalization.
#[derive(Debug, Clone, Deserialize)]
pub struct Ticket {
    pub subject: String,
    pub body: String,
}

/// Parses a ticket fixture. A bad document is a workflow error.
pub fn load_fixture(json: &str) -> Result<Fixture, HarnessError> {
    serde_json::from_str(json)
        .map_err(|err| HarnessError::Workflow(format!("ticket fixture: {err}")))
}

/// The router graph. `classify` reads only `ticket`.
pub fn workflow() -> Result<Workflow, HarnessError> {
    let route = Question::choice(
        "Which team should handle this support ticket?",
        [
            (
                "billing",
                "Charges, refunds, invoices, and payment methods.",
            ),
            ("technical", "Product bugs, outages, and how-to questions."),
            ("account", "Login, profile, and account access."),
        ],
    )?;
    Workflow::new(
        "normalize".into(),
        vec![
            Node::Deterministic {
                id: "normalize".into(),
                step: normalize,
            },
            Node::Decide {
                id: "classify".into(),
                reads: vec!["ticket".into()],
                questions: BTreeMap::from([("route".into(), route)]),
                policy: ConfidencePolicy {
                    min_confidence: 0.7,
                    on_low: ConfidenceAction::SafeDefault,
                },
            },
            Node::Deterministic {
                id: "billing".into(),
                step: to_billing,
            },
            Node::Deterministic {
                id: "technical".into(),
                step: to_technical,
            },
            Node::Deterministic {
                id: "account".into(),
                step: to_account,
            },
            Node::Deterministic {
                id: "queue".into(),
                step: to_queue,
            },
        ],
        transition,
    )?
    .on_low(ConfidenceAction::SafeDefault, "queue".into())
}

/// Runs `fixture`. `live` calls TypeSafe and ignores the recorded answer.
pub async fn run(fixture: &Fixture, live: bool) -> Result<Run, HarnessError> {
    let wf = workflow()?;
    let mut state = State::new();
    state.insert("subject".into(), json!(fixture.ticket.subject));
    state.insert("body".into(), json!(fixture.ticket.body));
    let machine = if live {
        let model = TypeSafeDecision::new(TypeSafeConfig::from_env()?)?;
        Machine::builder().decision(model).build()
    } else {
        Machine::builder()
            .decision(FixtureModel {
                answer: fixture.answer.clone(),
                usage: fixture.usage.clone(),
            })
            .build()
    };
    machine.run(&wf, state).await
}

/// Destination, chosen label, confidence, latency, and cost from a finished run.
pub fn report(run: &Run) -> String {
    let destination = run
        .state
        .get("destination")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    let Some(event) = run
        .events
        .iter()
        .find(|event| event.kind == EventKind::Decide)
    else {
        return format!("destination: {destination}\nroute: unknown");
    };
    let label = serde_json::from_value::<BTreeMap<String, Answer>>(event.output.clone())
        .ok()
        .and_then(|answers| {
            answers
                .get("route")
                .and_then(Answer::top)
                .map(str::to_owned)
        })
        .unwrap_or_else(|| "unknown".to_owned());
    let confidence = event.confidence.unwrap_or(0.0);
    let cost = event
        .usage
        .cost_usd
        .map(|cost| cost.to_string())
        .unwrap_or_else(|| "none".to_owned());
    format!(
        "destination: {destination}\nroute: {label}\nconfidence: {confidence}\nlatency_ms: {}\ncost_usd: {cost}",
        event.usage.latency_ms
    )
}

fn normalize(state: &mut State) -> Result<(), HarnessError> {
    let subject = text(state, "subject");
    let body = text(state, "body");
    let ticket_text = format!("{subject}\n{body}");
    state.insert(
        "ticket".into(),
        json!({
            "subject": subject,
            "body": body,
            "text": ticket_text,
        }),
    );
    Ok(())
}

fn text(state: &State, key: &str) -> String {
    state
        .get(key)
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim()
        .to_owned()
}

fn to_billing(state: &mut State) -> Result<(), HarnessError> {
    write_destination(state, "billing")
}

fn to_technical(state: &mut State) -> Result<(), HarnessError> {
    write_destination(state, "technical")
}

fn to_account(state: &mut State) -> Result<(), HarnessError> {
    write_destination(state, "account")
}

fn to_queue(state: &mut State) -> Result<(), HarnessError> {
    write_destination(state, "queue")
}

fn write_destination(state: &mut State, destination: &str) -> Result<(), HarnessError> {
    state.insert("destination".into(), json!(destination));
    Ok(())
}

/// Continues from `normalize` into `classify`, then matches the top label.
/// A label outside the route set goes to `queue`. Low confidence never reaches
/// this match: the machine sends it to the safe-default handler.
fn transition(node: &NodeId, outcome: &Outcome, _: &State) -> Next {
    match (node.0.as_str(), outcome) {
        ("normalize", _) => Next::Goto("classify".into()),
        ("classify", Outcome::Decided(answers)) => {
            let label = answers.get("route").and_then(Answer::top).unwrap_or("");
            let next = match label {
                "billing" | "technical" | "account" => label,
                _ => "queue",
            };
            Next::Goto(next.into())
        }
        _ => Next::End,
    }
}

struct FixtureModel {
    answer: Answer,
    usage: Usage,
}

impl DecisionModel for FixtureModel {
    fn evaluate(
        &self,
        req: DecisionRequest,
    ) -> BoxFuture<'_, Result<DecisionResponse, HarnessError>> {
        let answer = self.answer.clone();
        let usage = self.usage.clone();
        Box::pin(async move {
            let answers = req
                .questions
                .into_keys()
                .map(|key| (key, answer.clone()))
                .collect();
            Ok(DecisionResponse { answers, usage })
        })
    }
}
