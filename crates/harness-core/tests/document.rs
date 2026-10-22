use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use harness_core::{
    Answer, BoxFuture, BranchEdge, BranchWhen, Capability, ConfidenceAction, ConfidencePolicy,
    DecisionModel, DecisionRequest, DecisionResponse, DocumentNode, Effect, EventKind, Gate,
    GateOutcome, GenOutput, GenRequest, GenResponse, GenerativeModel, GrantDecision, HarnessError,
    JsonSchema, Machine, NodeId, OutputSpec, Question, RiskTier, RunStatus, State, Usage,
    WorkflowDocument, read_run,
};
use serde_json::{Value, json};

const TICKET_ROUTER: &str = include_str!("../fixtures/ticket-router.json");
const GATED_DRAFT: &str = include_str!("../fixtures/gated-draft.json");

fn load(json: &str) -> WorkflowDocument {
    let doc: WorkflowDocument = serde_json::from_str(json).unwrap();
    let rebuilt = WorkflowDocument::new(
        doc.owner.clone(),
        doc.tier,
        doc.grants.clone(),
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

fn opened(
    start: impl Into<NodeId>,
    nodes: Vec<DocumentNode>,
    edges: Vec<BranchEdge>,
    on_low: BTreeMap<ConfidenceAction, NodeId>,
) -> Result<WorkflowDocument, HarnessError> {
    WorkflowDocument::new(
        "fixture",
        RiskTier::Read,
        BTreeSet::<String>::new(),
        start.into(),
        nodes,
        edges,
        on_low,
    )
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
        "fixture",
        RiskTier::Read,
        ["file_read"],
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
        "owner": "fixture",
        "tier": "read",
        "grants": [],
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
    let duplicate = opened(
        "classify",
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

    let missing = opened("missing", vec![decide("classify")], vec![], BTreeMap::new());
    assert_eq!(
        missing,
        Err(HarnessError::Workflow(
            "start node `missing` is not defined".into()
        ))
    );

    let gate = opened(
        "draft",
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

    let edge = opened(
        "classify",
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
    let low = opened("classify", vec![decide("classify")], vec![], on_low);
    assert_eq!(
        low,
        Err(HarnessError::Workflow(
            "Continue is not a low-confidence edge".into()
        ))
    );
}

#[test]
fn a_document_without_an_owner_or_a_known_tier_fails_to_load() {
    let missing = r#"{
        "tier": "read",
        "grants": [],
        "start": "classify",
        "nodes": [{
            "kind": "decide",
            "id": "classify",
            "reads": [],
            "questions": {},
            "policy": { "min_confidence": 0.7, "on_low": "safe_default" }
        }],
        "edges": []
    }"#;
    assert!(serde_json::from_str::<WorkflowDocument>(missing).is_err());
    assert_eq!(
        opened(
            "classify",
            vec![decide("classify")],
            vec![],
            BTreeMap::new()
        )
        .map(|doc| doc.owner),
        Ok("fixture".to_owned())
    );
    assert_eq!(
        WorkflowDocument::new(
            "   ",
            RiskTier::Read,
            BTreeSet::<String>::new(),
            "classify".into(),
            vec![decide("classify")],
            vec![],
            BTreeMap::new(),
        ),
        Err(HarnessError::Workflow("document has no owner".into()))
    );
    for tier in ["dangerous", "shell"] {
        let json = format!(
            r#"{{"owner":"fixture","tier":{tier:?},"grants":[],"start":"classify","nodes":[{{"kind":"decide","id":"classify","reads":[],"questions":{{}},"policy":{{"min_confidence":0.7,"on_low":"safe_default"}}}}],"edges":[]}}"#
        );
        let err = serde_json::from_str::<WorkflowDocument>(&json).unwrap_err();
        assert!(err.to_string().contains(tier), "{err}");
    }
}

#[test]
fn shell_and_dangerous_effects_fail_even_with_a_grant() {
    for name in ["shell", "dangerous"] {
        let loaded = WorkflowDocument::new(
            "fixture",
            RiskTier::Write,
            [name],
            "run".into(),
            vec![DocumentNode::Effect {
                id: "run".into(),
                name: name.into(),
                reads: vec![],
            }],
            vec![],
            BTreeMap::new(),
        );
        assert_eq!(
            loaded,
            Err(HarnessError::Workflow(format!(
                "effect `{name}` cannot be loaded"
            )))
        );
        let json = format!(
            r#"{{"owner":"fixture","tier":"write","grants":[{name:?}],"start":"run","nodes":[{{"kind":"effect","id":"run","name":{name:?},"reads":[]}}],"edges":[]}}"#
        );
        let err = serde_json::from_str::<WorkflowDocument>(&json).unwrap_err();
        assert!(err.to_string().contains("cannot be loaded"), "{err}");
    }
}

#[test]
fn an_effect_requires_its_grant() {
    let node = DocumentNode::Effect {
        id: "read".into(),
        name: "file_read".into(),
        reads: vec![],
    };
    let missing = WorkflowDocument::new(
        "fixture",
        RiskTier::Read,
        BTreeSet::<String>::new(),
        "read".into(),
        vec![node.clone()],
        vec![],
        BTreeMap::new(),
    );
    assert_eq!(
        missing,
        Err(HarnessError::Workflow(
            "effect `file_read` has no grant".into()
        ))
    );
    let granted = WorkflowDocument::new(
        "fixture",
        RiskTier::Read,
        ["file_read"],
        "read".into(),
        vec![node],
        vec![],
        BTreeMap::new(),
    )
    .unwrap();
    let again: WorkflowDocument =
        serde_json::from_str(&serde_json::to_string(&granted).unwrap()).unwrap();
    assert_eq!(granted, again);
}

#[test]
fn a_document_stores_no_api_key() {
    let encoded = serde_json::to_string(&load(TICKET_ROUTER)).unwrap();
    for key in [
        "OPENAI_API_KEY",
        "ANTHROPIC_API_KEY",
        "TYPESAFE_API_KEY",
        "COMPOSIO_API_KEY",
    ] {
        assert!(!TICKET_ROUTER.contains(key), "{key}");
        assert!(!GATED_DRAFT.contains(key), "{key}");
        assert!(!encoded.contains(key), "{key}");
        let with_key =
            TICKET_ROUTER.replacen("\"start\"", &format!("\"{key}\": \"secret\", \"start\""), 1);
        assert!(
            serde_json::from_str::<WorkflowDocument>(&with_key).is_err(),
            "{key}"
        );
    }
}

struct LabelDecision {
    top: &'static str,
    confidence: f64,
}

impl DecisionModel for LabelDecision {
    fn evaluate(
        &self,
        req: DecisionRequest,
    ) -> BoxFuture<'_, Result<DecisionResponse, HarnessError>> {
        let top = self.top.to_owned();
        let confidence = self.confidence;
        Box::pin(async move {
            let answers = req
                .questions
                .into_keys()
                .map(|key| {
                    (
                        key,
                        Answer {
                            probabilities: BTreeMap::from([(top.clone(), confidence)]),
                            confidence,
                        },
                    )
                })
                .collect();
            Ok(DecisionResponse {
                answers,
                usage: Usage::default(),
            })
        })
    }
}

struct CountingGenerate {
    calls: Arc<AtomicUsize>,
}

impl GenerativeModel for CountingGenerate {
    fn complete(&self, _req: GenRequest) -> BoxFuture<'_, Result<GenResponse, HarnessError>> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Box::pin(async {
            Ok(GenResponse {
                output: GenOutput::Json(json!({ "reply": "ok" })),
                usage: Usage::default(),
            })
        })
    }
}

fn ends_at(path: &[NodeId]) -> Option<&str> {
    path.last().map(|id| id.0.as_str())
}

#[tokio::test]
async fn billing_fixture_reaches_billing() {
    let doc = load(TICKET_ROUTER);
    let run = Machine::builder()
        .decision(LabelDecision {
            top: "billing",
            confidence: 0.88,
        })
        .build()
        .run_document(&doc, State::new())
        .await
        .unwrap();
    assert_eq!(run.status, RunStatus::Completed);
    assert_eq!(ends_at(&run.path), Some("billing"));
}

#[tokio::test]
async fn low_confidence_takes_the_document_queue_edge() {
    let doc = load(TICKET_ROUTER);
    let run = Machine::builder()
        .decision(LabelDecision {
            top: "billing",
            confidence: 0.2,
        })
        .build()
        .run_document(&doc, State::new())
        .await
        .unwrap();
    assert_eq!(run.status, RunStatus::Completed);
    assert_eq!(ends_at(&run.path), Some("queue"));
    assert!(!run.path.iter().any(|id| id.0 == "billing"));
}

#[tokio::test]
async fn gated_draft_runs_the_model_only_when_the_allow_label_matches() {
    let doc = load(GATED_DRAFT);
    let calls = Arc::new(AtomicUsize::new(0));
    let mut state = State::new();
    state.insert("approved".into(), json!(true));
    state.insert("request".into(), json!("Please refund invoice 1042."));

    let allowed = Machine::builder()
        .decision(LabelDecision {
            top: "true",
            confidence: 0.86,
        })
        .generative(CountingGenerate {
            calls: Arc::clone(&calls),
        })
        .build()
        .run_document(&doc, state.clone())
        .await
        .unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert!(
        allowed
            .events
            .iter()
            .any(|event| event.kind == EventKind::Generate)
    );
    assert_eq!(ends_at(&allowed.path), Some("accept"));

    calls.store(0, Ordering::SeqCst);
    let denied = Machine::builder()
        .decision(LabelDecision {
            top: "false",
            confidence: 0.86,
        })
        .generative(CountingGenerate {
            calls: Arc::clone(&calls),
        })
        .build()
        .run_document(&doc, state)
        .await
        .unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    assert!(
        !denied
            .events
            .iter()
            .any(|event| event.kind == EventKind::Generate)
    );
    assert_eq!(ends_at(&denied.path), Some("safe"));
}

#[tokio::test]
async fn missing_approval_does_not_visit_draft() {
    let doc = load(GATED_DRAFT);
    let calls = Arc::new(AtomicUsize::new(0));
    let mut with_request = State::new();
    with_request.insert("request".into(), json!("Please refund invoice 1042."));

    let missing = Machine::builder()
        .decision(LabelDecision {
            top: "true",
            confidence: 0.86,
        })
        .generative(CountingGenerate {
            calls: Arc::clone(&calls),
        })
        .build()
        .run_document(&doc, with_request.clone())
        .await
        .unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    assert!(!missing.path.iter().any(|id| id.0 == "draft"));

    calls.store(0, Ordering::SeqCst);
    with_request.insert("approved".into(), json!(false));
    let denied = Machine::builder()
        .decision(LabelDecision {
            top: "true",
            confidence: 0.86,
        })
        .generative(CountingGenerate { calls })
        .build()
        .run_document(&doc, with_request)
        .await
        .unwrap();
    assert_eq!(denied.status, RunStatus::Completed);
    assert_eq!(ends_at(&denied.path), Some("safe"));
    assert!(!denied.path.iter().any(|id| id.0 == "draft"));
    assert!(
        !denied
            .events
            .iter()
            .any(|event| event.kind == EventKind::Generate)
    );
}

fn file_read_node() -> DocumentNode {
    DocumentNode::Effect {
        id: "read".into(),
        name: "file_read".into(),
        reads: vec![],
    }
}

fn allow_decide() -> DocumentNode {
    DocumentNode::Decide {
        id: "allow".into(),
        reads: vec![],
        questions: BTreeMap::from([(
            "allow".into(),
            Question::choice("May this write run?", [("true", "Yes"), ("false", "No")]).unwrap(),
        )]),
        policy: ConfidencePolicy {
            min_confidence: 0.7,
            on_low: ConfidenceAction::SafeDefault,
        },
    }
}

fn granted_read(
    tier: RiskTier,
    nodes: Vec<DocumentNode>,
    edges: Vec<BranchEdge>,
) -> WorkflowDocument {
    WorkflowDocument::new(
        "fixture",
        tier,
        ["file_read"],
        nodes[0].id().clone(),
        nodes,
        edges,
        BTreeMap::new(),
    )
    .unwrap()
}

struct CountingEffect {
    name: &'static str,
    capability: Capability,
    calls: Arc<AtomicUsize>,
}

impl Effect for CountingEffect {
    fn name(&self) -> &str {
        self.name
    }

    fn capability(&self) -> Capability {
        self.capability
    }

    fn run(&self, input: Value) -> BoxFuture<'_, Result<Value, HarnessError>> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Box::pin(async move { Ok(input) })
    }
}

fn counting(name: &'static str, capability: Capability) -> (Arc<AtomicUsize>, CountingEffect) {
    let calls = Arc::new(AtomicUsize::new(0));
    (
        Arc::clone(&calls),
        CountingEffect {
            name,
            capability,
            calls,
        },
    )
}

#[tokio::test]
async fn a_read_effect_runs_when_the_grant_is_present() {
    let doc = granted_read(RiskTier::Read, vec![file_read_node()], vec![]);
    let (calls, effect) = counting("file_read", Capability::FileRead);
    let run = Machine::builder()
        .effect(effect)
        .grant(Capability::FileRead)
        .build()
        .run_document(&doc, State::new())
        .await
        .unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert!(
        run.events
            .iter()
            .any(|event| event.kind == EventKind::Effect)
    );
    assert_eq!(
        run.events.grants(),
        &[GrantDecision {
            owner: "fixture".into(),
            tier: RiskTier::Read,
            tool: "file_read".into(),
            gate: GateOutcome::NotRequired,
        }]
    );
    assert_eq!(ends_at(&run.path), Some("read"));
}

#[tokio::test]
async fn a_write_effect_waits_for_the_human_gate() {
    let skipped = BranchEdge {
        from: "read".into(),
        when: BranchWhen::Skipped,
        to: "held".into(),
    };
    let closed = granted_read(
        RiskTier::Write,
        vec![file_read_node()],
        vec![skipped.clone()],
    );
    let (calls, effect) = counting("file_read", Capability::FileRead);
    let held = Machine::builder()
        .effect(effect)
        .grant(Capability::FileRead)
        .build()
        .run_document(&closed, State::new())
        .await
        .unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    assert!(
        held.events
            .iter()
            .all(|event| event.kind != EventKind::Effect)
    );
    assert_eq!(held.events.grants(), &[skipped_grant()]);
    assert_eq!(ends_at(&held.path), Some("held"));

    let mut waiting = granted_read(
        RiskTier::Write,
        vec![allow_decide(), file_read_node()],
        vec![
            BranchEdge {
                from: "allow".into(),
                when: BranchWhen::Always,
                to: "read".into(),
            },
            skipped,
        ],
    );
    waiting = waiting
        .with_human_gate(Gate {
            decide: "allow".into(),
            question: "allow".into(),
            allow: "true".into(),
        })
        .unwrap();
    let (calls, effect) = counting("file_read", Capability::FileRead);
    let denied = Machine::builder()
        .decision(LabelDecision {
            top: "false",
            confidence: 0.9,
        })
        .effect(effect)
        .grant(Capability::FileRead)
        .build()
        .run_document(&waiting, State::new())
        .await
        .unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    assert!(
        denied
            .events
            .iter()
            .all(|event| event.kind != EventKind::Effect)
    );
    assert_eq!(denied.events.grants(), &[skipped_grant()]);
    assert_eq!(ends_at(&denied.path), Some("held"));

    let (calls, effect) = counting("file_read", Capability::FileRead);
    let allowed = Machine::builder()
        .decision(LabelDecision {
            top: "true",
            confidence: 0.9,
        })
        .effect(effect)
        .grant(Capability::FileRead)
        .build()
        .run_document(&waiting, State::new())
        .await
        .unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert!(
        allowed
            .events
            .iter()
            .any(|event| event.kind == EventKind::Effect)
    );
    assert_eq!(
        allowed.events.grants(),
        &[GrantDecision {
            owner: "fixture".into(),
            tier: RiskTier::Write,
            tool: "file_read".into(),
            gate: GateOutcome::Passed,
        }]
    );
    assert_eq!(ends_at(&allowed.path), Some("read"));
}

fn skipped_grant() -> GrantDecision {
    GrantDecision {
        owner: "fixture".into(),
        tier: RiskTier::Write,
        tool: "file_read".into(),
        gate: GateOutcome::Skipped,
    }
}

#[tokio::test]
async fn a_grant_decision_appends_without_a_credential() {
    let doc = granted_read(RiskTier::Read, vec![file_read_node()], vec![]);
    let (_calls, effect) = counting("file_read", Capability::FileRead);
    let run = Machine::builder()
        .effect(effect)
        .grant(Capability::FileRead)
        .build()
        .run_document(&doc, State::new())
        .await
        .unwrap();
    let path = std::env::temp_dir().join(format!("seaotter-grant-{}", std::process::id()));
    let _ = std::fs::remove_file(&path);
    let id = run.events.append_to(&path, "document", "user-7").unwrap();
    let text = std::fs::read_to_string(&path).unwrap();
    assert!(text.contains("\"owner\":\"fixture\""), "{text}");
    assert!(text.contains("\"tier\":\"read\""), "{text}");
    assert!(text.contains("\"tool\":\"file_read\""), "{text}");
    assert!(text.contains("\"gate\":\"not_required\""), "{text}");
    for key in [
        "OPENAI_API_KEY",
        "ANTHROPIC_API_KEY",
        "TYPESAFE_API_KEY",
        "COMPOSIO_API_KEY",
        "host_permission",
    ] {
        assert!(!text.contains(key), "{key} in {text}");
    }
    let read = read_run(&path, &id).unwrap();
    assert_eq!(read.len(), 1);
    assert_eq!(read[0].kind, EventKind::Effect);
    std::fs::remove_file(&path).unwrap();
}

#[tokio::test]
async fn a_dangerous_effect_cannot_run() {
    for name in ["shell", "dangerous"] {
        let doc = WorkflowDocument {
            owner: "fixture".into(),
            tier: RiskTier::Write,
            grants: BTreeSet::from([name.to_owned()]),
            human_gate: None,
            start: "run".into(),
            nodes: vec![DocumentNode::Effect {
                id: "run".into(),
                name: name.into(),
                reads: vec![],
            }],
            edges: vec![],
            on_low: BTreeMap::new(),
        };
        let (calls, effect) = counting(name, Capability::FileRead);
        let err = Machine::builder()
            .effect(effect)
            .grant(Capability::FileRead)
            .build()
            .run_document(&doc, State::new())
            .await
            .unwrap_err();
        assert_eq!(
            err,
            HarnessError::Workflow(format!("effect `{name}` cannot run"))
        );
        assert_eq!(calls.load(Ordering::SeqCst), 0);
    }

    let doc = granted_read(RiskTier::Read, vec![file_read_node()], vec![]);
    let (calls, effect) = counting("file_read", Capability::DangerousShell);
    let err = Machine::builder()
        .effect(effect)
        .grant(Capability::DangerousShell)
        .build()
        .run_document(&doc, State::new())
        .await
        .unwrap_err();
    assert_eq!(
        err,
        HarnessError::Workflow("effect `file_read` cannot run".into())
    );
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}

#[test]
fn a_human_gate_must_point_at_a_decide_node() {
    let doc = granted_read(RiskTier::Write, vec![file_read_node()], vec![]);
    let err = doc.with_human_gate(Gate {
        decide: "missing".into(),
        question: "allow".into(),
        allow: "true".into(),
    });
    assert_eq!(
        err,
        Err(HarnessError::Workflow(
            "human gate must point at a Decide node, got `missing`".into()
        ))
    );
    let json = r#"{
        "owner": "fixture",
        "tier": "write",
        "grants": ["file_read"],
        "human_gate": { "decide": "missing", "question": "allow", "allow": "true" },
        "start": "read",
        "nodes": [{ "kind": "effect", "id": "read", "name": "file_read", "reads": [] }],
        "edges": []
    }"#;
    let loaded = serde_json::from_str::<WorkflowDocument>(json).unwrap_err();
    assert!(
        loaded
            .to_string()
            .contains("human gate must point at a Decide node"),
        "{loaded}"
    );
}
