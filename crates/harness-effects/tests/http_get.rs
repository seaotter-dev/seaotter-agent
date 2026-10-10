use std::net::SocketAddr;
use std::time::Duration;

use harness_core::{
    Capability, Effect, HarnessError, Machine, Next, Node, NodeId, Outcome, State, Workflow,
};
use harness_effects::HttpGet;
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::task::JoinHandle;

fn response(status: &str, headers: &[(&str, &str)], body: &[u8]) -> Vec<u8> {
    let mut out = format!("HTTP/1.1 {status}\r\nconnection: close\r\n");
    for (name, value) in headers {
        out.push_str(&format!("{name}: {value}\r\n"));
    }
    out.push_str("\r\n");
    let mut bytes = out.into_bytes();
    bytes.extend_from_slice(body);
    bytes
}

fn ok(content_type: &str, body: &str) -> Vec<u8> {
    let len = body.len().to_string();
    response(
        "200 OK",
        &[("content-type", content_type), ("content-length", &len)],
        body.as_bytes(),
    )
}

/// Accepts one connection, returns its request head, and replies with `reply`.
fn serve_once(listener: TcpListener, reply: Vec<u8>) -> JoinHandle<String> {
    tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut buf = Vec::new();
        let mut chunk = [0u8; 4096];
        while !buf.windows(4).any(|w| w == b"\r\n\r\n") {
            let n = socket.read(&mut chunk).await.unwrap();
            assert!(n > 0, "connection closed before headers");
            buf.extend_from_slice(&chunk[..n]);
        }
        // The client may hang up early (size limits), so write errors are fine.
        let _ = socket.write_all(&reply).await;
        let _ = socket.shutdown().await;
        String::from_utf8(buf).unwrap()
    })
}

async fn listener() -> (TcpListener, SocketAddr) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    (listener, addr)
}

async fn assert_no_connection(listener: &TcpListener) {
    let accepted = tokio::time::timeout(Duration::from_millis(200), listener.accept()).await;
    assert!(accepted.is_err(), "a request reached the server");
}

fn local() -> HttpGet {
    HttpGet::new(["127.0.0.1"])
        .unwrap()
        .allow_http(true)
        .unwrap()
}

fn effect_error(result: Result<Value, HarnessError>) -> String {
    match result {
        Err(HarnessError::Effect(msg)) => msg,
        other => panic!("expected an effect error, got {other:?}"),
    }
}

#[tokio::test]
async fn allowlisted_get_returns_the_body() {
    let (l, addr) = listener().await;
    let server = serve_once(l, ok("text/plain", "hello"));
    let out = local()
        .run(json!({"url": format!("http://{addr}/greeting?x=1")}))
        .await
        .unwrap();
    let head = server.await.unwrap();
    assert!(head.starts_with("GET /greeting?x=1 HTTP/1.1"), "{head}");
    assert_eq!(
        out,
        json!({
            "url": format!("http://{addr}/greeting?x=1"),
            "status": 200,
            "content_type": "text/plain",
            "body": "hello"
        })
    );
}

#[tokio::test]
async fn explicit_get_method_is_accepted() {
    let (l, addr) = listener().await;
    let server = serve_once(l, ok("text/plain", "ok"));
    let out = local()
        .run(json!({"url": format!("http://{addr}/"), "method": "get"}))
        .await
        .unwrap();
    server.await.unwrap();
    assert_eq!(out["body"], "ok");
}

#[tokio::test]
async fn non_2xx_status_is_data_not_an_error() {
    let (l, addr) = listener().await;
    let server = serve_once(
        l,
        response("404 Not Found", &[("content-length", "7")], b"missing"),
    );
    let out = local()
        .run(json!({"url": format!("http://{addr}/nope")}))
        .await
        .unwrap();
    server.await.unwrap();
    assert_eq!(out["status"], 404);
    assert_eq!(out["body"], "missing");
    assert_eq!(out["content_type"], Value::Null);
}

#[tokio::test]
async fn off_list_host_fails_before_sending() {
    let (l, addr) = listener().await;
    let effect = HttpGet::new(["example.com"])
        .unwrap()
        .allow_http(true)
        .unwrap();
    let msg = effect_error(effect.run(json!({"url": format!("http://{addr}/")})).await);
    assert!(msg.contains("not on the allowlist"), "{msg}");
    assert_no_connection(&l).await;
}

#[tokio::test]
async fn non_get_methods_fail_before_sending() {
    let (l, addr) = listener().await;
    let url = format!("http://{addr}/");
    for method in [json!("POST"), json!("DELETE"), json!("put"), json!(1)] {
        let msg = effect_error(local().run(json!({"url": url, "method": method})).await);
        assert!(msg.contains("is not allowed"), "{msg}");
    }
    assert_no_connection(&l).await;
}

#[tokio::test]
async fn http_without_opt_in_and_userinfo_fail_before_sending() {
    let (l, addr) = listener().await;
    let https_only = HttpGet::new(["127.0.0.1"]).unwrap();
    let msg = effect_error(
        https_only
            .run(json!({"url": format!("http://{addr}/")}))
            .await,
    );
    assert!(msg.contains("uses http"), "{msg}");

    let msg = effect_error(
        local()
            .run(json!({"url": format!("http://user:pw@{addr}/")}))
            .await,
    );
    assert!(msg.contains("credentials"), "{msg}");
    assert_no_connection(&l).await;
}

#[test]
fn check_rejects_bad_input_without_io() {
    let effect = HttpGet::new(["Example.COM", " api.example.com "]).unwrap();
    assert!(
        effect
            .check(&json!({"url": "https://EXAMPLE.com/a"}))
            .is_ok()
    );
    assert!(
        effect
            .check(&json!({"url": "https://api.example.com"}))
            .is_ok()
    );
    assert!(
        effect
            .check(&json!({"url": "https://example.com:8443/x"}))
            .is_ok()
    );

    let rejected = [
        json!({}),
        json!({"url": 5}),
        json!({"url": "not a url"}),
        json!({"url": "ftp://example.com/file"}),
        json!({"url": "https://example.com.evil.net/"}),
        json!({"url": "https://sub.example.com/"}),
        json!({"url": "https://example.co/"}),
        json!({"url": "file:///etc/passwd"}),
    ];
    for input in rejected {
        assert!(
            matches!(effect.check(&input), Err(HarnessError::Effect(_))),
            "accepted {input}"
        );
    }

    let local = local();
    assert!(local.check(&json!({"url": "http://127.0.0.10/"})).is_err());
}

#[tokio::test]
async fn redirect_to_an_allowlisted_host_is_followed() {
    let (first, first_addr) = listener().await;
    let (second, second_addr) = listener().await;
    let target = format!("http://{second_addr}/final");
    let first_server = serve_once(
        first,
        response(
            "302 Found",
            &[("location", &target), ("content-length", "0")],
            b"",
        ),
    );
    let second_server = serve_once(second, ok("text/plain", "arrived"));
    let out = local()
        .run(json!({"url": format!("http://{first_addr}/start")}))
        .await
        .unwrap();
    first_server.await.unwrap();
    assert!(second_server.await.unwrap().starts_with("GET /final"));
    assert_eq!(out["url"], target);
    assert_eq!(out["body"], "arrived");
}

#[tokio::test]
async fn redirect_off_the_allowlist_is_refused() {
    let (first, first_addr) = listener().await;
    let (second, second_addr) = listener().await;
    let target = format!("http://localhost:{}/stolen", second_addr.port());
    let first_server = serve_once(
        first,
        response(
            "302 Found",
            &[("location", &target), ("content-length", "0")],
            b"",
        ),
    );
    let msg = effect_error(
        local()
            .run(json!({"url": format!("http://{first_addr}/start")}))
            .await,
    );
    first_server.await.unwrap();
    assert!(msg.contains("redirect refused"), "{msg}");
    assert_no_connection(&second).await;
}

#[tokio::test]
async fn oversized_and_non_utf8_bodies_are_errors() {
    let (l, addr) = listener().await;
    let server = serve_once(l, ok("text/plain", &"x".repeat(64)));
    let msg = effect_error(
        local()
            .max_bytes(16)
            .run(json!({"url": format!("http://{addr}/")}))
            .await,
    );
    server.await.unwrap();
    assert!(msg.contains("limit"), "{msg}");

    let (l, addr) = listener().await;
    let server = serve_once(l, response("200 OK", &[], &[b'y'; 64]));
    let msg = effect_error(
        local()
            .max_bytes(16)
            .run(json!({"url": format!("http://{addr}/")}))
            .await,
    );
    server.await.unwrap();
    assert!(msg.contains("limit"), "{msg}");

    let (l, addr) = listener().await;
    let server = serve_once(
        l,
        response("200 OK", &[("content-length", "3")], &[0xff, 0xfe, 0x00]),
    );
    let msg = effect_error(local().run(json!({"url": format!("http://{addr}/")})).await);
    server.await.unwrap();
    assert!(msg.contains("UTF-8"), "{msg}");
}

#[test]
fn construction_name_and_capability() {
    assert!(matches!(
        HttpGet::new(Vec::<String>::new()),
        Err(HarnessError::Effect(_))
    ));
    assert!(HttpGet::new(["", "  "]).is_err());
    let effect = HttpGet::new(["example.com"]).unwrap();
    assert_eq!(effect.name(), "http_get");
    assert_eq!(effect.capability(), Capability::HttpGet);
    assert_eq!(effect.named("weather").name(), "weather");
}

fn transition(_node: &NodeId, _outcome: &Outcome, _state: &State) -> Next {
    Next::End
}

fn workflow() -> Workflow {
    let nodes = vec![Node::Effect {
        id: "fetch".into(),
        name: "http_get".into(),
        reads: vec!["url".into()],
    }];
    Workflow::new("fetch".into(), nodes, transition).unwrap()
}

#[tokio::test]
async fn machine_refuses_the_effect_without_the_capability() {
    let (l, addr) = listener().await;
    let machine = Machine::builder().effect(local()).build();
    let state = State::from([("url".into(), json!(format!("http://{addr}/")))]);
    assert_eq!(
        machine.run(&workflow(), state).await.unwrap_err(),
        HarnessError::CapabilityDenied(Capability::HttpGet)
    );
    assert_no_connection(&l).await;
}

#[tokio::test]
async fn machine_runs_the_effect_with_the_capability() {
    let (l, addr) = listener().await;
    let server = serve_once(l, ok("application/json", r#"{"temp":21}"#));
    let machine = Machine::builder()
        .effect(local())
        .grant(Capability::HttpGet)
        .build();
    let state = State::from([("url".into(), json!(format!("http://{addr}/weather")))]);
    let run = machine.run(&workflow(), state).await.unwrap();
    server.await.unwrap();
    assert_eq!(run.state["fetch"]["status"], 200);
    assert_eq!(run.state["fetch"]["body"], r#"{"temp":21}"#);
}
