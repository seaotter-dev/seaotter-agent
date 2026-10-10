use std::collections::BTreeMap;

use harness_core::{DecisionRequest, HarnessError, NoulCriteria, Question};
use serde_json::json;

fn choice() -> Question {
    Question::choice(
        "Which team owns this ticket?",
        [
            ("billing", "Payments and invoices"),
            ("technical", "Bugs and outages"),
        ],
    )
    .unwrap()
}

#[test]
fn constructors_match_hand_built_questions() {
    assert_eq!(
        choice(),
        Question::Choice {
            instructions: "Which team owns this ticket?".into(),
            criteria: BTreeMap::from([
                ("billing".into(), "Payments and invoices".into()),
                ("technical".into(), "Bugs and outages".into()),
            ]),
        }
    );
    assert_eq!(
        Question::score(
            "How severe is the issue?",
            ["Cosmetic", "Degraded", "Blocking"]
        ),
        Question::Score {
            instructions: "How severe is the issue?".into(),
            criteria: vec!["Cosmetic".into(), "Degraded".into(), "Blocking".into()],
        }
    );
    assert_eq!(
        Question::noul("Is this safe to answer?"),
        Question::Noul {
            instructions: "Is this safe to answer?".into(),
            criteria: None,
        }
    );
    assert_eq!(
        Question::noul_meanings(
            "Is this a refund?",
            "The customer wants money back",
            "It is not"
        ),
        Question::Noul {
            instructions: "Is this a refund?".into(),
            criteria: Some(NoulCriteria {
                yes: "The customer wants money back".into(),
                no: "It is not".into(),
            }),
        }
    );
}

#[test]
fn repeated_choice_label_is_rejected() {
    let err = Question::choice("Pick one", [("billing", "a"), ("billing", "b")]).unwrap_err();
    assert!(
        matches!(err, HarnessError::Workflow(ref msg) if msg.contains("billing")),
        "{err:?}"
    );
}

#[test]
fn builder_keeps_reads_and_rejects_the_rest() {
    let req = DecisionRequest::builder(["subject"])
        .set("subject", "refund")
        .unwrap()
        .ask("route", choice())
        .unwrap()
        .build();
    assert_eq!(req.state.get("subject"), Some(&json!("refund")));
    assert!(!req.state.contains_key("secret"));
    assert_eq!(req.questions.get("route"), Some(&choice()));

    let err = DecisionRequest::builder(["subject"])
        .set("secret", "do-not-send")
        .unwrap_err();
    assert!(
        matches!(err, HarnessError::Workflow(ref msg) if msg.contains("secret")),
        "{err:?}"
    );

    let unread = DecisionRequest::builder(["subject"]).build();
    assert!(!unread.state.contains_key("subject"));

    let err = DecisionRequest::builder(["subject"])
        .ask("route", choice())
        .unwrap()
        .ask("route", Question::noul("again"))
        .unwrap_err();
    assert!(
        matches!(err, HarnessError::Workflow(ref msg) if msg.contains("route")),
        "{err:?}"
    );
}
