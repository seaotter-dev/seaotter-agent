use std::path::PathBuf;

use gated_generate::{Approval, load_fixture, run, token_is_present};
use harness_core::{EventKind, HarnessError, RunStatus};
use serde_json::json;

fn approval(name: &str, create: bool, token_present: bool) -> (PathBuf, Approval) {
    let path =
        std::env::temp_dir().join(format!("seaotter-approval-{name}-{}", std::process::id()));
    if create {
        std::fs::write(&path, b"").unwrap();
    } else {
        let _ = std::fs::remove_file(&path);
    }
    let approval = Approval {
        path: path.clone(),
        token_present,
    };
    (path, approval)
}

#[test]
fn blank_token_is_not_approval() {
    assert!(!token_is_present(None));
    assert!(!token_is_present(Some("")));
    assert!(!token_is_present(Some("  ")));
    assert!(token_is_present(Some("ok")));
}

#[tokio::test]
async fn allow_fixture_with_a_file_drafts_a_reply() {
    let (path, approval) = approval("file", true, false);
    let fixture = load_fixture(include_str!("../fixtures/allow.json")).unwrap();
    let finished = run(&fixture, None, &approval).await.unwrap();
    let _ = std::fs::remove_file(&path);
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
async fn allow_fixture_with_only_the_token_drafts_a_reply() {
    let (_path, approval) = approval("token", false, true);
    let fixture = load_fixture(include_str!("../fixtures/allow.json")).unwrap();
    let finished = run(&fixture, None, &approval).await.unwrap();
    assert_eq!(finished.status, RunStatus::Completed);
    assert_eq!(finished.state.get("destination"), Some(&json!("drafted")));
    assert!(
        finished
            .events
            .iter()
            .any(|event| event.kind == EventKind::Generate)
    );
}

#[tokio::test]
async fn missing_approval_does_not_call_the_generative_model() {
    let (_path, approval) = approval("missing", false, false);
    let fixture = load_fixture(include_str!("../fixtures/allow.json")).unwrap();
    let finished = run(&fixture, None, &approval).await.unwrap();
    assert_eq!(finished.status, RunStatus::Completed);
    assert_eq!(finished.state.get("destination"), Some(&json!("safe")));
    assert!(finished.path.iter().all(|id| id.0 != "draft"));
    assert!(
        finished
            .events
            .iter()
            .all(|event| event.kind != EventKind::Generate)
    );
}

#[tokio::test]
async fn deny_fixture_skips_generation() {
    let (path, approval) = approval("deny", true, false);
    let fixture = load_fixture(include_str!("../fixtures/deny.json")).unwrap();
    let finished = run(&fixture, None, &approval).await.unwrap();
    let _ = std::fs::remove_file(&path);
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
    let (_path, approval) = approval("low", false, false);
    let fixture = load_fixture(include_str!("../fixtures/low_confidence.json")).unwrap();
    let finished = run(&fixture, None, &approval).await.unwrap();
    assert_eq!(finished.status, RunStatus::Completed);
    assert_eq!(finished.state.get("destination"), Some(&json!("safe")));
    assert!(finished.path.iter().all(|id| id.0 != "draft"));
}

#[tokio::test]
async fn invalid_body_is_schema_invalid() {
    let (path, approval) = approval("invalid", true, false);
    let fixture = load_fixture(include_str!("../fixtures/invalid.json")).unwrap();
    let err = run(&fixture, None, &approval).await.unwrap_err();
    let _ = std::fs::remove_file(&path);
    assert!(matches!(err, HarnessError::SchemaInvalid(_)), "{err}");
}
