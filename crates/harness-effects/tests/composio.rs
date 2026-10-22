use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use harness_core::{
    BoxFuture, Capability, DocumentNode, Effect, HarnessError, Machine, RiskTier, State,
    WorkflowDocument,
};
use harness_effects::{Composio, ExecuteCall};
use serde_json::{Value, json};

const ACTION: &str = "GMAIL_SEND_EMAIL";
const CONNECTION: &str = "conn_123";

fn records() -> (
    Arc<Mutex<Vec<String>>>,
    impl Fn(ExecuteCall) -> BoxFuture<'static, Result<Value, HarnessError>> + Send + Sync + 'static,
) {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let record = Arc::clone(&seen);
    let transport = move |call: ExecuteCall| {
        let record = Arc::clone(&record);
        Box::pin(async move {
            record.lock().unwrap().push(format!(
                "https://backend.composio.dev/api/v3.1/tools/execute/{}",
                call.action
            ));
            let _ = call.key;
            Ok(json!({"ok": true}))
        }) as BoxFuture<'static, Result<Value, HarnessError>>
    };
    (seen, transport)
}

fn document() -> WorkflowDocument {
    WorkflowDocument::new(
        "fixture",
        RiskTier::Read,
        [ACTION],
        "send".into(),
        vec![DocumentNode::Effect {
            id: "send".into(),
            name: "composio".into(),
            reads: vec!["draft".into()],
            connection: Some(CONNECTION.into()),
            action: Some(ACTION.into()),
        }],
        vec![],
        Default::default(),
    )
    .unwrap()
}

struct Counting {
    calls: Arc<AtomicUsize>,
}

impl Effect for Counting {
    fn name(&self) -> &str {
        "file_read"
    }

    fn capability(&self) -> Capability {
        Capability::FileRead
    }

    fn run(&self, input: Value) -> BoxFuture<'_, Result<Value, HarnessError>> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Box::pin(async move { Ok(input) })
    }
}

#[tokio::test]
async fn an_action_outside_the_grant_makes_no_request() {
    let (seen, transport) = records();
    let effect = Composio::new(ACTION)
        .with_key(Some("test-key".into()))
        .with_transport(transport);
    let err = effect
        .run(json!({"action": "GMAIL_DELETE_EMAIL", "connection": CONNECTION}))
        .await
        .unwrap_err();
    assert_eq!(
        err,
        HarnessError::Workflow("effect `composio` action `GMAIL_DELETE_EMAIL` has no grant".into())
    );
    assert!(seen.lock().unwrap().is_empty());

    let mut doc = document();
    doc.grants.clear();
    let (seen, transport) = records();
    let calls = Arc::new(AtomicUsize::new(0));
    let err = Machine::builder()
        .effect(
            Composio::new(ACTION)
                .with_key(Some("test-key".into()))
                .with_transport(transport),
        )
        .effect(Counting {
            calls: Arc::clone(&calls),
        })
        .grant(Capability::Composio)
        .grant(Capability::FileRead)
        .build()
        .run_document(&doc, State::new())
        .await
        .unwrap_err();
    assert_eq!(
        err,
        HarnessError::Workflow(format!("effect `composio` action `{ACTION}` has no grant"))
    );
    assert!(seen.lock().unwrap().is_empty());
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn a_missing_key_fails_before_any_request() {
    let (seen, transport) = records();
    let calls = Arc::new(AtomicUsize::new(0));
    let err = Machine::builder()
        .effect(
            Composio::new(ACTION)
                .with_key(None)
                .with_transport(transport),
        )
        .effect(Counting {
            calls: Arc::clone(&calls),
        })
        .grant(Capability::Composio)
        .grant(Capability::FileRead)
        .build()
        .run_document(&document(), State::from([("draft".into(), json!("hello"))]))
        .await
        .unwrap_err();
    assert_eq!(
        err,
        HarnessError::Effect("composio: COMPOSIO_API_KEY is not set".into())
    );
    assert!(seen.lock().unwrap().is_empty());
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}
