use std::collections::BTreeMap;

use harness_core::{
    BranchWhen, Catalog, ConfidenceAction, ConfidencePolicy, DocumentNode, Gate, HarnessError,
    JsonSchema, OutputSpec, Question, RiskTier,
};
use serde_json::json;

fn policy() -> ConfidencePolicy {
    ConfidencePolicy {
        min_confidence: 0.7,
        on_low: ConfidenceAction::SafeDefault,
    }
}

fn decide_questions() -> BTreeMap<String, Question> {
    BTreeMap::from([(
        "allow".into(),
        Question::choice("May this draft run?", [("true", "Yes"), ("false", "No")]).unwrap(),
    )])
}

#[test]
fn a_catalog_builds_a_document_from_the_allowed_operations() {
    let mut catalog = Catalog::new("fixture", RiskTier::Read, ["file_read", "http_get"]).unwrap();
    catalog
        .add_decide(
            "classify",
            vec!["ticket".into()],
            decide_questions(),
            policy(),
        )
        .unwrap();
    catalog
        .add_generate(
            "draft",
            "Draft a reply.",
            vec!["ticket".into()],
            OutputSpec::Schema(JsonSchema(json!({"type": "object"}))),
            128,
        )
        .unwrap();
    catalog
        .set_gate(
            "draft",
            Gate {
                decide: "classify".into(),
                question: "allow".into(),
                allow: "true".into(),
            },
        )
        .unwrap();
    catalog
        .connect("classify", BranchWhen::Always, "draft")
        .unwrap();
    catalog
        .set_low(ConfidenceAction::SafeDefault, "queue")
        .unwrap();
    catalog.add_effect("read", "file_read", vec![]).unwrap();
    catalog.add_effect("fetch", "http_get", vec![]).unwrap();

    let doc = catalog.document().unwrap();
    let again = serde_json::from_str(&serde_json::to_string(&doc).unwrap()).unwrap();
    assert_eq!(doc, again);
    assert_eq!(doc.owner, "fixture");
    assert_eq!(doc.tier, RiskTier::Read);
    assert!(doc.grants.contains("file_read"));
    assert!(doc.grants.contains("http_get"));
    assert_eq!(doc.start.0, "classify");
    assert!(matches!(doc.nodes[0], DocumentNode::Decide { .. }));
    assert!(matches!(
        &doc.nodes[1],
        DocumentNode::Generate { gate: Some(gate), .. } if gate.decide.0 == "classify"
    ));
    assert!(matches!(
        &doc.nodes[2],
        DocumentNode::Effect { name, .. } if name == "file_read"
    ));
    assert!(matches!(
        &doc.nodes[3],
        DocumentNode::Effect { name, .. } if name == "http_get"
    ));
    assert_eq!(doc.edges[0].from.0, "classify");
    assert_eq!(doc.edges[0].to.0, "draft");
    assert_eq!(
        doc.on_low
            .get(&ConfidenceAction::SafeDefault)
            .map(|id| id.0.as_str()),
        Some("queue")
    );
}

#[test]
fn an_unknown_effect_leaves_the_catalog_unchanged() {
    let mut catalog = Catalog::new("fixture", RiskTier::Read, ["shell", "file_read"]).unwrap();
    catalog
        .add_decide("classify", vec![], decide_questions(), policy())
        .unwrap();
    for name in ["shell", "dangerous", "composio"] {
        let before = catalog.clone();
        let err = catalog.add_effect("run", name, vec![]).unwrap_err();
        assert_eq!(
            err,
            HarnessError::Workflow(format!("unknown effect `{name}`"))
        );
        assert_eq!(catalog, before);
    }
}

#[test]
fn a_known_effect_without_a_grant_leaves_the_catalog_unchanged() {
    let mut catalog = Catalog::new("fixture", RiskTier::Read, Vec::<String>::new()).unwrap();
    let before = catalog.clone();
    let err = catalog.add_effect("read", "file_read", vec![]).unwrap_err();
    assert_eq!(
        err,
        HarnessError::Workflow("effect `file_read` has no grant".into())
    );
    assert_eq!(catalog, before);
}

#[test]
fn a_bad_gate_or_low_edge_leaves_the_catalog_unchanged() {
    let mut catalog = Catalog::new("fixture", RiskTier::Read, Vec::<String>::new()).unwrap();
    catalog
        .add_generate(
            "draft",
            "Draft.",
            vec![],
            OutputSpec::Schema(JsonSchema(json!({"type": "object"}))),
            32,
        )
        .unwrap();
    let before = catalog.clone();
    let gate = catalog
        .set_gate(
            "draft",
            Gate {
                decide: "missing".into(),
                question: "allow".into(),
                allow: "true".into(),
            },
        )
        .unwrap_err();
    assert_eq!(
        gate,
        HarnessError::Workflow("gate on `draft` must point at a Decide node, got `missing`".into())
    );
    assert_eq!(catalog, before);

    let low = catalog
        .set_low(ConfidenceAction::Continue, "queue")
        .unwrap_err();
    assert_eq!(
        low,
        HarnessError::Workflow("Continue is not a low-confidence edge".into())
    );
    assert_eq!(catalog, before);
}
