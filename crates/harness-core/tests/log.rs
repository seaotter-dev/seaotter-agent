use harness_core::{Event, EventKind, Usage, append_run, read_run};
use serde_json::json;

fn event(name: &str, kind: EventKind, hash: &str) -> Event {
    Event {
        node: name.into(),
        name: name.into(),
        kind,
        input_hash: hash.into(),
        output: json!({"route": "billing"}),
        confidence: Some(0.88),
        usage: Usage::default(),
    }
}

#[test]
fn append_run_round_trips_one_run_and_omits_secrets() {
    let path = std::env::temp_dir().join(format!("seaotter-log-{}", std::process::id()));
    let _ = std::fs::remove_file(&path);
    let first = event("classify", EventKind::Decide, "abc");
    let second = Event {
        node: "read".into(),
        name: "file_read".into(),
        kind: EventKind::Effect,
        input_hash: "def".into(),
        output: json!({"contents": "workspace note"}),
        confidence: None,
        usage: Usage::default(),
    };
    let id1 = append_run(&path, "examples/ticket-router", "user-7", &[first.clone()]).unwrap();
    let id2 = append_run(&path, "examples/effects", "other", &[second]).unwrap();
    assert_ne!(id1, id2);

    let read = read_run(&path, &id1).unwrap();
    assert_eq!(read, vec![first]);
    assert!(read_run(&path, &id2).unwrap()[0].name == "file_read");

    let text = std::fs::read_to_string(&path).unwrap();
    assert!(text.contains("user-7"), "{text}");
    assert!(text.contains("\"input_hash\":\"abc\""), "{text}");
    assert!(text.contains("\"outcome\":"), "{text}");
    assert!(!text.contains("sk-test"), "{text}");
    std::fs::remove_file(&path).unwrap();
}
