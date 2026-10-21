use std::collections::BTreeMap;

use harness_core::{
    Answer, BoxFuture, Capability, ConfidenceAction, ConfidencePolicy, DecisionModel,
    DecisionRequest, DecisionResponse, Effect, Event, EventKind, Gate, GenOutput, GenRequest,
    GenResponse, GenerativeModel, HarnessError, JsonSchema, Node, NoulCriteria, OutputSpec,
    Question, State, Usage,
};
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};

fn roundtrip<T: Serialize + DeserializeOwned + PartialEq + std::fmt::Debug>(value: &T) {
    let encoded = serde_json::to_string(value).unwrap();
    let decoded: T = serde_json::from_str(&encoded).unwrap();
    assert_eq!(&decoded, value);
}

fn route_question() -> Question {
    Question::Choice {
        instructions: "Which team should handle this ticket?".into(),
        criteria: BTreeMap::from([
            ("billing".into(), "Payments and invoices".into()),
            ("technical".into(), "Bugs and outages".into()),
        ]),
    }
}

fn answer(probabilities: &[(&str, f64)], confidence: f64) -> Answer {
    Answer {
        probabilities: probabilities
            .iter()
            .map(|(label, p)| ((*label).to_owned(), *p))
            .collect(),
        confidence,
    }
}

struct FixedDecision;

impl DecisionModel for FixedDecision {
    fn evaluate(
        &self,
        req: DecisionRequest,
    ) -> BoxFuture<'_, Result<DecisionResponse, HarnessError>> {
        Box::pin(async move {
            let answers = req
                .questions
                .into_keys()
                .map(|key| (key, answer(&[("billing", 0.8), ("technical", 0.2)], 0.9)))
                .collect();
            Ok(DecisionResponse {
                answers,
                usage: Usage::default(),
            })
        })
    }
}

struct EchoGenerator;

impl GenerativeModel for EchoGenerator {
    fn complete(&self, req: GenRequest) -> BoxFuture<'_, Result<GenResponse, HarnessError>> {
        Box::pin(async move {
            let output = match req.output {
                OutputSpec::Schema(_) => GenOutput::Json(req.context),
                OutputSpec::FreeText => GenOutput::Text(req.task),
            };
            Ok(GenResponse {
                output,
                usage: Usage::default(),
            })
        })
    }
}

struct UpperEffect;

impl Effect for UpperEffect {
    fn name(&self) -> &str {
        "upper"
    }

    fn capability(&self) -> Capability {
        Capability::FileRead
    }

    fn run(&self, input: Value) -> BoxFuture<'_, Result<Value, HarnessError>> {
        Box::pin(async move {
            input
                .as_str()
                .map(|s| Value::String(s.to_uppercase()))
                .ok_or_else(|| HarnessError::Effect("expected a string".into()))
        })
    }
}

fn assert_send<T: Send>(_: &T) {}

#[tokio::test]
async fn decision_model_is_dyn_safe() {
    let model: Box<dyn DecisionModel> = Box::new(FixedDecision);
    let req = DecisionRequest {
        state: State::from([("subject".into(), json!("refund"))]),
        questions: BTreeMap::from([("route".into(), route_question())]),
    };
    let fut = model.evaluate(req);
    assert_send(&fut);
    let resp = fut.await.unwrap();
    assert_eq!(resp.answers["route"].top(), Some("billing"));
}

#[tokio::test]
async fn generative_model_is_dyn_safe() {
    let model: Box<dyn GenerativeModel> = Box::new(EchoGenerator);
    let req = GenRequest {
        task: "summarize".into(),
        context: json!({"summary": "ok"}),
        output: OutputSpec::Schema(JsonSchema(json!({"type": "object"}))),
        max_tokens: 64,
    };
    let resp = model.complete(req).await.unwrap();
    assert_eq!(resp.output, GenOutput::Json(json!({"summary": "ok"})));
}

#[tokio::test]
async fn effect_is_dyn_safe() {
    let effects: Vec<Box<dyn Effect>> = vec![Box::new(UpperEffect)];
    let effect = effects.iter().find(|e| e.name() == "upper").unwrap();
    assert_eq!(effect.capability(), Capability::FileRead);
    assert_eq!(effect.run(json!("hi")).await.unwrap(), json!("HI"));
    assert!(matches!(
        effect.run(json!(1)).await,
        Err(HarnessError::Effect(_))
    ));
}

#[test]
fn answer_top_picks_highest_probability() {
    let a = answer(&[("a", 0.1), ("b", 0.7), ("c", 0.2)], 0.7);
    assert_eq!(a.top(), Some("b"));
    assert_eq!(answer(&[], 0.0).top(), None);
}

#[test]
fn types_roundtrip_through_json() {
    roundtrip(&route_question());
    roundtrip(&Question::Noul {
        instructions: "Is this spam?".into(),
        criteria: None,
    });
    roundtrip(&Question::Noul {
        instructions: "Has the customer contacted support before?".into(),
        criteria: Some(NoulCriteria {
            yes: "Mentions a prior attempt".into(),
            no: "No sign of previous contact".into(),
        }),
    });
    roundtrip(&Question::Score {
        instructions: "How severe is the issue?".into(),
        criteria: vec!["Cosmetic".into(), "Degraded".into(), "Blocking".into()],
    });
    roundtrip(&DecisionResponse {
        answers: BTreeMap::from([("route".into(), answer(&[("billing", 1.0)], 0.95))]),
        usage: Usage {
            latency_ms: 12,
            input_tokens: Some(40),
            output_tokens: None,
            cost_usd: Some(0.0001),
        },
    });
    for output in [
        OutputSpec::Schema(JsonSchema(json!({"type": "object"}))),
        OutputSpec::FreeText,
    ] {
        roundtrip(&GenRequest {
            task: "reply".into(),
            context: json!({"ticket": 1}),
            output,
            max_tokens: 256,
        });
    }
    roundtrip(&Event {
        node: "route".into(),
        name: "route".into(),
        kind: EventKind::Decide,
        input_hash: "abc123".into(),
        output: json!({"route": "billing"}),
        confidence: Some(0.9),
        usage: Usage::default(),
    });
}

fn normalize(state: &mut State) -> Result<(), HarnessError> {
    state.insert("normalized".into(), json!(true));
    Ok(())
}

#[test]
fn nodes_cover_every_variant() {
    let policy = ConfidencePolicy {
        min_confidence: 0.7,
        on_low: ConfidenceAction::Escalate,
    };
    let nodes = [
        Node::Deterministic {
            id: "normalize".into(),
            step: normalize,
        },
        Node::Decide {
            id: "route".into(),
            reads: vec!["subject".into()],
            questions: BTreeMap::from([("route".into(), route_question())]),
            policy,
        },
        Node::Generate {
            id: "draft".into(),
            task: "Draft a reply".into(),
            reads: vec!["subject".into()],
            output: OutputSpec::Schema(JsonSchema(json!({"type": "object"}))),
            max_tokens: 256,
            gate: Some(Gate {
                decide: "route".into(),
                question: "route".into(),
                allow: "billing".into(),
            }),
        },
        Node::Effect {
            id: "fetch".into(),
            name: "upper".into(),
            reads: vec!["subject".into()],
        },
    ];
    assert_eq!(nodes.len(), 4);

    if let Node::Deterministic { step, .. } = &nodes[0] {
        let mut state = State::new();
        step(&mut state).unwrap();
        assert_eq!(state["normalized"], json!(true));
    }
}
