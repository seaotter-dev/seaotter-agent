use std::collections::BTreeMap;

use harness_core::{
    BranchEdge, BranchWhen, ConfidenceAction, ConfidencePolicy, DocumentNode, Gate, HarnessError,
    JsonSchema, NodeId, OutputSpec, Question, WorkflowDocument,
};
use serde_json::json;

const TICKET_ROUTER: &str = include_str!("../fixtures/ticket-router.json");
const GATED_DRAFT: &str = include_str!("../fixtures/gated-draft.json");

fn load(json: &str) -> WorkflowDocument {
    let doc: WorkflowDocument = serde_json::from_str(json).unwrap();
    let rebuilt = WorkflowDocument::new(
        doc.start.clone(),
        doc.nodes.clone(),
        doc.edges.clone(),
        doc.on_low.clone(),
    )
    .unwrap();
    assert_eq!(doc, rebuilt);
    let encoded = serde_json::to_string(&doc).unwrap();
    let again: WorkflowDocument = serde_json::from_str(&encoded).unwrap();
    assert_eq!(doc, again);
    doc
}

fn decide(id: &str) -> DocumentNode {
    DocumentNode::Decide {
        id: id.into(),
        reads: vec!["ticket".into()],
        questions: BTreeMap::from([(
            "route".into(),
            Question::choice("Which team?", [("billing", "Payments")]).unwrap(),
        )]),
        policy: ConfidencePolicy {
            min_confidence: 0.7,
            on_low: ConfidenceAction::SafeDefault,
        },
    }
}

#[test]
fn ticket_router_fixture_round_trips() {
    assert!(!TICKET_ROUTER.contains("normalize"));
    assert!(!TICKET_ROUTER.contains("step"));
    let doc = load(TICKET_ROUTER);
    assert_eq!(doc.start, NodeId::from("classify"));
    assert_eq!(doc.nodes.len(), 1);
    let DocumentNode::Decide {
        reads,
        questions,
        policy,
        ..
    } = &doc.nodes[0]
    else {
        panic!("classify is a decide node");
    };
    assert_eq!(reads, &vec!["ticket".to_owned()]);
    let Question::Choice {
        instructions,
        criteria,
    } = questions.get("route").unwrap()
    else {
        panic!("route is a choice");
    };
    assert_eq!(
        instructions,
        "Which team should handle this support ticket?"
    );
    assert_eq!(
        criteria.get("billing").map(String::as_str),
        Some("Charges, refunds, invoices, and payment methods.")
    );
    assert_eq!(policy.min_confidence, 0.7);
    assert_eq!(policy.on_low, ConfidenceAction::SafeDefault);
    let targets: Vec<&str> = doc.edges.iter().map(|edge| edge.to.0.as_str()).collect();
    assert_eq!(targets, ["billing", "technical", "account", "queue"]);
    assert_eq!(
        doc.on_low.get(&ConfidenceAction::SafeDefault),
        Some(&NodeId::from("queue"))
    );
}

#[test]
fn gated_draft_fixture_round_trips() {
    assert!(!GATED_DRAFT.contains("\"approve\""));
    assert!(!GATED_DRAFT.contains("normalize"));
    assert!(!GATED_DRAFT.contains("step"));
    let doc = load(GATED_DRAFT);
    assert_eq!(doc.start, NodeId::from("gate"));
    assert_eq!(doc.nodes.len(), 2);
    let DocumentNode::Generate {
        task,
        max_tokens,
        gate,
        output,
        ..
    } = &doc.nodes[1]
    else {
        panic!("draft is a generate node");
    };
    assert_eq!(task, "Draft a short reply to this support request.");
    assert_eq!(*max_tokens, 128);
    assert_eq!(
        gate,
        &Some(Gate {
            decide: "gate".into(),
            question: "allow".into(),
            allow: "true".into(),
        })
    );
    assert!(matches!(output, OutputSpec::Schema(_)));
    assert_eq!(
        doc.edges[0].when,
        BranchWhen::State {
            key: "approved".into(),
            equals: true,
        }
    );
    assert_eq!(doc.edges[0].to, NodeId::from("draft"));
    assert_eq!(
        doc.edges[1].when,
        BranchWhen::State {
            key: "approved".into(),
            equals: false,
        }
    );
    assert_eq!(doc.edges[1].to, NodeId::from("safe"));
    assert_eq!(doc.edges[2].when, BranchWhen::Generated);
    assert_eq!(doc.edges[2].to, NodeId::from("accept"));
    assert_eq!(doc.edges[3].when, BranchWhen::Skipped);
    assert_eq!(doc.edges[3].to, NodeId::from("safe"));
    assert_eq!(
        doc.on_low.get(&ConfidenceAction::SafeDefault),
        Some(&NodeId::from("safe"))
    );
}

#[test]
fn effect_edge_round_trips() {
    let doc = WorkflowDocument::new(
        "read".into(),
        vec![DocumentNode::Effect {
            id: "read".into(),
            name: "file_read".into(),
            reads: vec!["path".into()],
        }],
        vec![BranchEdge {
            from: "read".into(),
            when: BranchWhen::Always,
            to: "done".into(),
        }],
        BTreeMap::new(),
    )
    .unwrap();
    let encoded = serde_json::to_string(&doc).unwrap();
    assert!(encoded.contains("file_read"));
    assert!(!encoded.contains("step"));
    let again: WorkflowDocument = serde_json::from_str(&encoded).unwrap();
    assert_eq!(doc, again);
    let DocumentNode::Effect { name, .. } = &again.nodes[0] else {
        panic!("read is an effect node");
    };
    assert_eq!(name, "file_read");
    assert_eq!(again.edges[0].when, BranchWhen::Always);
    assert_eq!(again.edges[0].to, NodeId::from("done"));
}

#[test]
fn a_step_field_fails_to_deserialize() {
    let json = r#"{
        "start": "classify",
        "nodes": [{
            "kind": "decide",
            "id": "classify",
            "reads": ["ticket"],
            "questions": {},
            "policy": { "min_confidence": 0.7, "on_low": "safe_default" },
            "step": "normalize"
        }],
        "edges": []
    }"#;
    let err = serde_json::from_str::<WorkflowDocument>(json).unwrap_err();
    assert!(err.to_string().contains("step"), "{err}");
}

#[test]
fn new_rejects_a_document_the_machine_cannot_load() {
    let duplicate = WorkflowDocument::new(
        "classify".into(),
        vec![decide("classify"), decide("classify")],
        vec![],
        BTreeMap::new(),
    );
    assert_eq!(
        duplicate,
        Err(HarnessError::Workflow(
            "duplicate node id `classify`".into()
        ))
    );

    let missing = WorkflowDocument::new(
        "missing".into(),
        vec![decide("classify")],
        vec![],
        BTreeMap::new(),
    );
    assert_eq!(
        missing,
        Err(HarnessError::Workflow(
            "start node `missing` is not defined".into()
        ))
    );

    let gate = WorkflowDocument::new(
        "draft".into(),
        vec![DocumentNode::Generate {
            id: "draft".into(),
            task: "Draft.".into(),
            reads: vec![],
            output: OutputSpec::Schema(JsonSchema(json!({"type": "object"}))),
            max_tokens: 32,
            gate: Some(Gate {
                decide: "missing".into(),
                question: "allow".into(),
                allow: "true".into(),
            }),
        }],
        vec![],
        BTreeMap::new(),
    );
    assert_eq!(
        gate,
        Err(HarnessError::Workflow(
            "gate on `draft` must point at a Decide node, got `missing`".into()
        ))
    );

    let edge = WorkflowDocument::new(
        "classify".into(),
        vec![decide("classify")],
        vec![BranchEdge {
            from: "billing".into(),
            when: BranchWhen::Always,
            to: "queue".into(),
        }],
        BTreeMap::new(),
    );
    assert_eq!(
        edge,
        Err(HarnessError::Workflow(
            "edge from `billing` is not a node".into()
        ))
    );

    let mut on_low = BTreeMap::new();
    on_low.insert(ConfidenceAction::Continue, "queue".into());
    let low = WorkflowDocument::new("classify".into(), vec![decide("classify")], vec![], on_low);
    assert_eq!(
        low,
        Err(HarnessError::Workflow(
            "Continue is not a low-confidence edge".into()
        ))
    );
}
