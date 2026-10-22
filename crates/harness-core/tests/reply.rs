use std::collections::BTreeMap;

use harness_core::{
    Answer, BoxFuture, BranchWhen, ConfidenceAction, DecisionModel, DecisionRequest,
    DecisionResponse, HarnessError, Machine, NodeId, RiskTier, RunStatus, State, Usage,
    apply_reply,
};

const REPLY: &str = include_str!("../fixtures/consultant-reply.json");

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

fn ends_at(path: &[NodeId]) -> Option<&str> {
    path.last().map(|id| id.0.as_str())
}

#[tokio::test]
async fn a_recorded_reply_becomes_a_document_and_the_run_does_not_build() {
    let doc = apply_reply("fixture", RiskTier::Read, Vec::<String>::new(), REPLY).unwrap();
    assert_eq!(doc.start.0, "classify");
    assert!(matches!(
        &doc.edges[0].when,
        BranchWhen::Label { question, equals }
            if question == "route" && equals == "billing" && doc.edges[0].to.0 == "billing"
    ));
    assert_eq!(
        doc.on_low
            .get(&ConfidenceAction::SafeDefault)
            .map(|id| id.0.as_str()),
        Some("queue")
    );

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

#[test]
fn a_reply_outside_the_catalog_is_rejected() {
    let err = apply_reply(
        "fixture",
        RiskTier::Read,
        Vec::<String>::new(),
        r#"[{"op":"compile"}]"#,
    )
    .unwrap_err();
    assert_eq!(
        err,
        HarnessError::Workflow("unknown catalog call `compile`".into())
    );
}

#[test]
fn a_reply_that_adds_a_grant_is_rejected() {
    for op in ["add_grant", "grant_add"] {
        let err = apply_reply(
            "fixture",
            RiskTier::Read,
            Vec::<String>::new(),
            &format!(r#"[{{"op":"{op}","name":"shell"}}]"#),
        )
        .unwrap_err();
        assert_eq!(
            err,
            HarnessError::Workflow("grant add is not a catalog call".into())
        );
    }
}
