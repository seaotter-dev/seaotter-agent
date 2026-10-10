use gated_generate::{load_fixture, run};
use harness_core::{EventKind, HarnessError, RunStatus};
use serde_json::json;

#[tokio::test]
async fn allow_fixture_drafts_a_reply() {
    let fixture = load_fixture(include_str!("../fixtures/allow.json")).unwrap();
    let finished = run(&fixture, None).await.unwrap();
    assert_eq!(finished.status, RunStatus::Completed);
    assert_eq!(finished.state.get("destination"), Some(&json!("drafted")));
    assert_eq!(
        finished.state.get("draft"),
        Some(&json!({
            "reply": "I refunded the duplicate charge on invoice 1042."
        }))
    );
}

#[tokio::test]
async fn deny_fixture_skips_generation() {
    let fixture = load_fixture(include_str!("../fixtures/deny.json")).unwrap();
    let finished = run(&fixture, None).await.unwrap();
    assert_eq!(finished.status, RunStatus::Completed);
    assert_eq!(finished.state.get("destination"), Some(&json!("safe")));
    assert!(finished.path.iter().any(|id| id.0 == "draft"));
    assert!(
        finished
            .events
            .iter()
            .all(|event| event.kind != EventKind::Generate)
    );
}

#[tokio::test]
async fn low_confidence_does_not_visit_draft() {
    let fixture = load_fixture(include_str!("../fixtures/low_confidence.json")).unwrap();
    let finished = run(&fixture, None).await.unwrap();
    assert_eq!(finished.status, RunStatus::Completed);
    assert_eq!(finished.state.get("destination"), Some(&json!("safe")));
    assert!(finished.path.iter().all(|id| id.0 != "draft"));
}

#[tokio::test]
async fn invalid_body_is_schema_invalid() {
    let fixture = load_fixture(include_str!("../fixtures/invalid.json")).unwrap();
    let err = run(&fixture, None).await.unwrap_err();
    assert!(matches!(err, HarnessError::SchemaInvalid(_)), "{err}");
}
