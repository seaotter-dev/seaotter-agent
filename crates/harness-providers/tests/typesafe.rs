use std::collections::BTreeMap;

use harness_core::{DecisionModel, DecisionRequest, HarnessError, NoulCriteria, Question, State};
use harness_providers::typesafe::{WireResponse, from_wire, to_wire};
use harness_providers::{TypeSafeConfig, TypeSafeDecision};
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::task::JoinHandle;

const FIXTURE: &str = include_str!("fixtures/systemone_response.json");

fn questions() -> BTreeMap<String, Question> {
    BTreeMap::from([
        (
            "route".into(),
            Question::Choice {
                instructions: "Which team should handle this ticket?".into(),
                criteria: BTreeMap::from([
                    ("billing".into(), "Charges and invoices".into()),
                    ("returns".into(), "Sending items back".into()),
                    ("shipping".into(), "Delivery problems".into()),
                ]),
            },
        ),
        (
            "bug_severity".into(),
            Question::Score {
                instructions: "How severe is the reported issue?".into(),
                criteria: vec![
                    "Cosmetic; no impact to functionality".into(),
                    "Broken or degraded feature, but workaround exists".into(),
                    "Blocking issue; no workaround exists".into(),
                ],
            },
        ),
        (
            "is_human_escalation".into(),
            Question::Noul {
                instructions: "Is the customer asking for a human agent?".into(),
                criteria: None,
            },
        ),
    ])
}

fn request() -> DecisionRequest {
    DecisionRequest {
        state: State::from([("ticket".into(), json!("I want to return my order."))]),
        questions: questions(),
    }
}

fn fixture() -> WireResponse {
    serde_json::from_str(FIXTURE).unwrap()
}

fn assert_close(actual: f64, expected: f64) {
    assert!((actual - expected).abs() < 1e-9, "{actual} != {expected}");
}

fn assert_provider<T: std::fmt::Debug>(result: Result<T, HarnessError>) {
    assert!(
        matches!(result, Err(HarnessError::Provider(_))),
        "{result:?}"
    );
}

#[test]
fn to_wire_matches_the_documented_body() {
    let mut req = request();
    req.questions.insert(
        "is_repeat_contact".into(),
        Question::Noul {
            instructions: "Has the customer contacted support before?".into(),
            criteria: Some(NoulCriteria {
                yes: "Mentions a prior attempt".into(),
                no: "No sign of previous contact".into(),
            }),
        },
    );
    let body = serde_json::to_value(to_wire(&req, "jev-latest")).unwrap();
    assert_eq!(
        body,
        json!({
            "model": "jev-latest",
            "state": {"ticket": "I want to return my order."},
            "questions": {
                "route": {
                    "type": "choice",
                    "instructions": "Which team should handle this ticket?",
                    "criteria": {
                        "billing": "Charges and invoices",
                        "returns": "Sending items back",
                        "shipping": "Delivery problems"
                    }
                },
                "bug_severity": {
                    "type": "score",
                    "instructions": "How severe is the reported issue?",
                    "criteria": [
                        "Cosmetic; no impact to functionality",
                        "Broken or degraded feature, but workaround exists",
                        "Blocking issue; no workaround exists"
                    ]
                },
                "is_human_escalation": {
                    "type": "noul",
                    "instructions": "Is the customer asking for a human agent?"
                },
                "is_repeat_contact": {
                    "type": "noul",
                    "instructions": "Has the customer contacted support before?",
                    "criteria": {
                        "true": "Mentions a prior attempt",
                        "false": "No sign of previous contact"
                    }
                }
            }
        })
    );
}

#[test]
fn fixture_maps_to_probabilities_and_confidence() {
    let resp = from_wire(fixture(), &questions(), 42).unwrap();

    let route = &resp.answers["route"];
    assert_eq!(route.top(), Some("returns"));
    assert_close(route.confidence, 1.0);
    assert_close(route.probabilities["billing"], 0.0);

    let severity = &resp.answers["bug_severity"];
    assert_eq!(severity.top(), Some("1"));
    assert_close(severity.confidence, 0.35);
    assert_close(severity.probabilities["2"], 0.43);

    let human = &resp.answers["is_human_escalation"];
    assert_eq!(human.top(), Some("true"));
    assert_close(human.probabilities["true"], 0.99);
    assert_close(human.probabilities["false"], 1.0 - 0.99);
    assert_close(human.confidence, 2.0 * (0.99 - 0.5));

    assert_eq!(resp.usage.latency_ms, 42);
    assert_eq!(resp.usage.input_tokens, Some(328));
    assert_eq!(resp.usage.output_tokens, Some(34));
    assert_eq!(resp.usage.cost_usd, None);
}

#[test]
fn undecided_noul_has_zero_confidence() {
    let mut wire = fixture();
    wire.answers.insert(
        "is_human_escalation".into(),
        serde_json::from_value(json!({"type": "noul", "noul": 0.5})).unwrap(),
    );
    let resp = from_wire(wire, &questions(), 0).unwrap();
    assert_close(resp.answers["is_human_escalation"].confidence, 0.0);
}

#[test]
fn missing_answer_is_a_provider_error() {
    let mut wire = fixture();
    wire.answers.remove("route");
    assert_provider(from_wire(wire, &questions(), 0));
}

#[test]
fn mismatched_answer_type_is_a_provider_error() {
    let mut wire = fixture();
    wire.answers.insert(
        "route".into(),
        serde_json::from_value(json!({"type": "noul", "noul": 0.9})).unwrap(),
    );
    assert_provider(from_wire(wire, &questions(), 0));
}

#[test]
fn unasked_answer_is_a_provider_error() {
    let mut qs = questions();
    qs.remove("bug_severity");
    assert_provider(from_wire(fixture(), &qs, 0));
}

#[test]
fn out_of_range_noul_is_a_provider_error() {
    let mut wire = fixture();
    wire.answers.insert(
        "is_human_escalation".into(),
        serde_json::from_value(json!({"type": "noul", "noul": 1.5})).unwrap(),
    );
    assert_provider(from_wire(wire, &questions(), 0));
}

struct Captured {
    head: String,
    body: Value,
}

/// Serves one HTTP response on a local port and returns what it received.
async fn serve_once(status: &'static str, body: &'static str) -> (String, JoinHandle<Captured>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base_url = format!("http://{}", listener.local_addr().unwrap());
    let handle = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut buf = Vec::new();
        let mut chunk = [0u8; 4096];
        let header_end = loop {
            let n = socket.read(&mut chunk).await.unwrap();
            assert!(n > 0, "connection closed before headers");
            buf.extend_from_slice(&chunk[..n]);
            if let Some(i) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                break i + 4;
            }
        };
        let head = String::from_utf8(buf[..header_end].to_vec()).unwrap();
        let len: usize = head
            .lines()
            .find_map(|l| {
                let (name, value) = l.split_once(':')?;
                name.eq_ignore_ascii_case("content-length")
                    .then(|| value.trim().parse().unwrap())
            })
            .unwrap_or(0);
        while buf.len() < header_end + len {
            let n = socket.read(&mut chunk).await.unwrap();
            assert!(n > 0, "connection closed before body");
            buf.extend_from_slice(&chunk[..n]);
        }
        let received = serde_json::from_slice(&buf[header_end..header_end + len]).unwrap();
        let response = format!(
            "HTTP/1.1 {status}\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
            body.len(),
        );
        socket.write_all(response.as_bytes()).await.unwrap();
        socket.shutdown().await.unwrap();
        Captured {
            head,
            body: received,
        }
    });
    (base_url, handle)
}

fn client(base_url: String) -> TypeSafeDecision {
    let mut config = TypeSafeConfig::new("test-key");
    config.base_url = base_url;
    TypeSafeDecision::new(config).unwrap()
}

#[tokio::test]
async fn evaluate_posts_only_state_and_questions() {
    let (base_url, server) = serve_once("200 OK", FIXTURE).await;
    let resp = client(base_url).evaluate(request()).await.unwrap();
    let captured = server.await.unwrap();

    let request_line = captured.head.lines().next().unwrap();
    assert_eq!(request_line, "POST /v1/systemone HTTP/1.1");
    assert!(
        captured
            .head
            .lines()
            .any(|l| l.eq_ignore_ascii_case("authorization: Bearer test-key")),
        "{}",
        captured.head
    );
    let keys: Vec<&str> = captured
        .body
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    assert_eq!(keys, ["model", "questions", "state"]);
    assert_eq!(
        captured.body,
        serde_json::to_value(to_wire(&request(), "jev-latest")).unwrap()
    );

    assert_eq!(resp.answers["route"].top(), Some("returns"));
    assert_eq!(resp.usage.input_tokens, Some(328));
}

#[tokio::test]
async fn non_success_status_is_a_provider_error() {
    let (base_url, server) =
        serve_once("401 Unauthorized", r#"{"error": "invalid api key"}"#).await;
    let err = client(base_url).evaluate(request()).await.unwrap_err();
    server.await.unwrap();
    match err {
        HarnessError::Provider(msg) => {
            assert!(msg.contains("401"), "{msg}");
            assert!(msg.contains("invalid api key"), "{msg}");
        }
        other => panic!("expected provider error, got {other:?}"),
    }
}

/// Calls the real API. Run with `TYPESAFE_API_KEY=... cargo test -p
/// harness-providers -- --ignored`.
#[tokio::test]
#[ignore = "live network call; needs TYPESAFE_API_KEY"]
async fn live_choice_call() {
    let Ok(config) = TypeSafeConfig::from_env() else {
        eprintln!("TYPESAFE_API_KEY not set; skipping");
        return;
    };
    let mut questions = questions();
    questions.retain(|k, _| k == "route");
    let req = DecisionRequest {
        state: request().state,
        questions,
    };
    let resp = TypeSafeDecision::new(config)
        .unwrap()
        .evaluate(req)
        .await
        .unwrap();
    let route = &resp.answers["route"];
    let total: f64 = route.probabilities.values().sum();
    assert!((total - 1.0).abs() < 1e-3, "probabilities sum to {total}");
    assert!((0.0..=1.0).contains(&route.confidence));
}
