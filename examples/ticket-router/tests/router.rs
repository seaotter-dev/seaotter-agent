use harness_core::RunStatus;
use serde_json::json;
use ticket_router::{load_fixture, run};

#[tokio::test]
async fn billing_fixture_routes_to_billing() {
    let fixture = load_fixture(include_str!("../fixtures/billing.json")).unwrap();
    let finished = run(&fixture, false).await.unwrap();
    assert_eq!(finished.status, RunStatus::Completed);
    assert_eq!(finished.state.get("destination"), Some(&json!("billing")));
}

#[tokio::test]
async fn low_confidence_goes_to_the_queue() {
    let fixture = load_fixture(include_str!("../fixtures/low_confidence.json")).unwrap();
    let finished = run(&fixture, false).await.unwrap();
    assert_eq!(finished.status, RunStatus::Completed);
    assert_eq!(finished.state.get("destination"), Some(&json!("queue")));
    assert!(finished.path.iter().any(|id| id.0 == "queue"));
}
