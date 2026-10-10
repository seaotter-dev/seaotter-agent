use harness_core::{check_answer_fixture, check_branch_fixture};

const CHOICE_IN_RANGE: &str = include_str!("fixtures/choice_in_range.json");
const BRANCH_ESCALATE: &str = include_str!("fixtures/branch_escalate.json");

#[test]
fn choice_fixture_accepts_the_top_label_inside_the_range() {
    check_answer_fixture(CHOICE_IN_RANGE).unwrap();
}

#[test]
fn branch_fixture_takes_escalate() {
    check_branch_fixture(BRANCH_ESCALATE).unwrap();
}

#[test]
fn wrong_label_and_out_of_range_confidence_fail() {
    let wrong = r#"{
        "question": "route",
        "probabilities": { "billing": 0.82, "technical": 0.18 },
        "confidence": 0.9,
        "expect": "technical",
        "min_confidence": 0.8,
        "max_confidence": 1.0
    }"#;
    let err = check_answer_fixture(wrong).unwrap_err();
    assert!(err.message.contains("technical"), "{err}");

    let low = r#"{
        "question": "route",
        "probabilities": { "billing": 1.0 },
        "confidence": 0.5,
        "expect": "billing",
        "min_confidence": 0.8,
        "max_confidence": 1.0
    }"#;
    let err = check_answer_fixture(low).unwrap_err();
    assert!(err.message.contains("outside"), "{err}");
    assert!(check_answer_fixture("not json").is_err());
}

#[test]
fn inclusive_endpoints_pass() {
    for confidence in ["0.8", "1.0"] {
        let json = format!(
            r#"{{
                "question": "route",
                "probabilities": {{ "billing": 1.0 }},
                "confidence": {confidence},
                "expect": "billing",
                "min_confidence": 0.8,
                "max_confidence": 1.0
            }}"#
        );
        check_answer_fixture(&json).unwrap();
    }
}

#[test]
fn high_score_continues_and_low_score_takes_safe_default() {
    let continues = r#"{
        "question": "route",
        "probabilities": { "billing": 0.9 },
        "confidence": 0.95,
        "policy": { "min_confidence": 0.7, "on_low": "escalate" },
        "expect": "continue"
    }"#;
    check_branch_fixture(continues).unwrap();

    let safe = r#"{
        "question": "route",
        "probabilities": { "billing": 0.9 },
        "confidence": 0.4,
        "policy": { "min_confidence": 0.7, "on_low": "safe_default" },
        "expect": "safe_default"
    }"#;
    check_branch_fixture(safe).unwrap();
}
