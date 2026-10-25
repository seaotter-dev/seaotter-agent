use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::Path;
use std::process::{Command, ExitCode};

use harness_core::{RiskTier, WorkflowDocument, apply_reply};

const PUBLISH: &str = "127.0.0.1:8080:8080";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let (_workflow, bin, rest) = match parse(&args) {
        Ok(Request::Health) => return ExitCode::SUCCESS,
        Ok(Request::Page) => return serve_page(),
        Ok(Request::Workflow(workflow, bin, rest)) => (workflow, bin, rest),
        Err(err) => {
            eprintln!("{err}");
            return ExitCode::FAILURE;
        }
    };
    let code = match Command::new(bin).args(rest).status() {
        Ok(status) => u8::try_from(status.code().unwrap_or(1)).unwrap_or(1),
        Err(err) => {
            eprintln!("running {bin}: {err}");
            return ExitCode::FAILURE;
        }
    };
    if code == 0
        && let Err(err) = ensure_workspace(Path::new("/data"))
    {
        eprintln!("{err}");
        return ExitCode::FAILURE;
    }
    ExitCode::from(code)
}

#[derive(Debug)]
enum Request<'a> {
    Health,
    Page,
    Workflow(&'a str, &'static str, &'a [String]),
}

fn parse(args: &[String]) -> Result<Request<'_>, String> {
    if args.is_empty() {
        return Ok(Request::Page);
    }
    if args.len() == 1 && args[0] == "--health" {
        return Ok(Request::Health);
    }
    let (workflow, bin, rest) = parse_workflow(args)?;
    Ok(Request::Workflow(workflow, bin, rest))
}

fn listen_addr() -> SocketAddr {
    SocketAddr::from(([0, 0, 0, 0], 8080))
}

struct App {
    document: Option<WorkflowDocument>,
}

fn page() -> String {
    render(&App { document: None }, None)
}

fn render(app: &App, rejection: Option<&str>) -> String {
    let graph = app
        .document
        .as_ref()
        .map(seaotter_mcp::wireframe)
        .unwrap_or_default();
    let rejection = rejection.unwrap_or("");
    format!(
        "<!doctype html>
<html>
<body>
<section id=\"chat\">{rejection}</section>
<section id=\"graph\">{graph}</section>
<section id=\"run\"></section>
</body>
</html>
"
    )
}

fn submit(app: &mut App, message: &str) -> String {
    let rejection = match apply_reply(
        "fixture",
        RiskTier::Read,
        std::iter::empty::<String>(),
        message,
    ) {
        Ok(doc) => {
            app.document = Some(doc);
            None
        }
        Err(err) => Some(err.to_string()),
    };
    render(app, rejection.as_deref())
}

fn serve_page() -> ExitCode {
    let addr = listen_addr();
    let port = addr.port();
    if PUBLISH != format!("127.0.0.1:{port}:{port}") {
        eprintln!("publish {PUBLISH} does not match {addr}");
        return ExitCode::FAILURE;
    }
    let listener = match TcpListener::bind(addr) {
        Ok(listener) => listener,
        Err(err) => {
            eprintln!("listening on {addr}: {err}");
            return ExitCode::FAILURE;
        }
    };
    if let Err(err) = serve(listener) {
        eprintln!("{err}");
        return ExitCode::FAILURE;
    }
    ExitCode::SUCCESS
}

fn serve(listener: TcpListener) -> Result<(), String> {
    let mut app = App { document: None };
    for stream in listener.incoming() {
        let mut stream = stream.map_err(|err| format!("accepting a connection: {err}"))?;
        respond(&mut app, &mut stream)?;
    }
    Ok(())
}

fn respond(app: &mut App, stream: &mut TcpStream) -> Result<(), String> {
    let mut buf = [0; 16384];
    let n = stream
        .read(&mut buf)
        .map_err(|err| format!("reading a request: {err}"))?;
    let request = String::from_utf8_lossy(&buf[..n]).into_owned();
    let body = match posted_message(&request) {
        Some(message) => submit(app, message),
        None => render(app, None),
    };
    let response = format!(
        "HTTP/1.1 200 OK\r\ncontent-type: text/html; charset=utf-8\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
        body.len()
    );
    stream
        .write_all(response.as_bytes())
        .map_err(|err| format!("writing the page: {err}"))
}

fn posted_message(request: &str) -> Option<&str> {
    let body = request.split_once("\r\n\r\n")?.1;
    let body = body.strip_prefix("message=").unwrap_or(body).trim();
    if body.is_empty() { None } else { Some(body) }
}

fn ensure_workspace(root: &Path) -> Result<(), String> {
    let workspace = root.join("workspace");
    std::fs::create_dir_all(&workspace)
        .map_err(|err| format!("creating {}: {err}", workspace.display()))
}

fn parse_workflow(args: &[String]) -> Result<(&str, &'static str, &[String]), String> {
    if args.first().map(String::as_str) != Some("--workflow") {
        return Err("--workflow is required".into());
    }
    let name = args.get(1).ok_or("--workflow needs a name")?;
    let bin = match name.as_str() {
        "examples/ticket-router" => "ticket-router",
        "examples/gated-generate" => "gated-generate",
        "examples/effects" => "effects",
        other => return Err(format!("unknown workflow `{other}`")),
    };
    Ok((name.as_str(), bin, &args[2..]))
}

#[cfg(test)]
mod tests {
    use std::io::{Read, Write};
    use std::net::TcpListener;

    use super::{App, PUBLISH, Request, ensure_workspace, page, parse, respond};

    fn args(values: &[&str]) -> Vec<String> {
        values.iter().copied().map(str::to_string).collect()
    }

    #[test]
    fn ticket_router() {
        let args = args(&["--workflow", "examples/ticket-router"]);
        let Request::Workflow(workflow, bin, rest) = parse(&args).unwrap() else {
            panic!("expected a workflow");
        };
        assert_eq!(workflow, "examples/ticket-router");
        assert_eq!(bin, "ticket-router");
        assert!(rest.is_empty());
    }

    #[test]
    fn gated_generate_forwards_later_args() {
        let args = args(&["--workflow", "examples/gated-generate", "--live", "openai"]);
        let Request::Workflow(workflow, bin, rest) = parse(&args).unwrap() else {
            panic!("expected a workflow");
        };
        assert_eq!(workflow, "examples/gated-generate");
        assert_eq!(bin, "gated-generate");
        assert_eq!(rest, ["--live", "openai"]);
    }

    #[test]
    fn unknown_workflow() {
        let args = args(&["--workflow", "no-such"]);
        let err = parse(&args).unwrap_err();
        assert!(err.contains("no-such"), "{err}");
    }

    #[test]
    fn no_arguments_select_the_page() {
        assert!(matches!(parse(&[]).unwrap(), Request::Page));
    }

    #[test]
    fn the_page_has_chat_graph_and_run() {
        let body = page();
        assert!(body.contains("chat"), "{body}");
        assert!(body.contains("graph"), "{body}");
        assert!(body.contains("run"), "{body}");
    }

    #[test]
    fn a_request_on_localhost_returns_the_page() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut app = App { document: None };
            respond(&mut app, &mut stream).unwrap();
        });
        let mut stream = std::net::TcpStream::connect(addr).unwrap();
        stream
            .write_all(b"GET / HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n")
            .unwrap();
        let mut response = String::new();
        stream.read_to_string(&mut response).unwrap();
        server.join().unwrap();
        assert!(addr.ip().is_loopback());
        assert!(response.contains(&page()), "{response}");
    }

    #[test]
    fn a_message_builds_the_graph_and_a_rejected_call_keeps_it() {
        let mut app = App { document: None };
        let reply = include_str!("../../../crates/harness-core/fixtures/consultant-reply.json");
        let built = post(&mut app, reply);
        let graph = section(&built, "graph");
        assert!(graph.contains("classify"), "{built}");

        let rejected = r#"[
  {
    "op": "add_decide",
    "id": "other",
    "reads": ["ticket"],
    "questions": {
      "route": {
        "kind": "choice",
        "instructions": "Which team should handle this support ticket?",
        "criteria": {
          "billing": "Charges, refunds, invoices, and payment methods."
        }
      }
    },
    "policy": { "min_confidence": 0.7, "on_low": "safe_default" }
  },
  { "op": "add_grant", "name": "file_read" }
]"#;
        let kept = post(&mut app, rejected);
        assert!(
            section(&kept, "chat").contains("grant add is not a catalog call"),
            "{kept}"
        );
        let graph = section(&kept, "graph");
        assert!(graph.contains("classify"), "{kept}");
        assert!(!graph.contains("other"), "{kept}");
        assert!(section(&kept, "run").is_empty(), "{kept}");
    }

    fn post(app: &mut App, message: &str) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let mut client = std::net::TcpStream::connect(addr).unwrap();
        let (mut server, _) = listener.accept().unwrap();
        let payload = format!("message={message}");
        let request = format!(
            "POST / HTTP/1.1\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{payload}",
            payload.len()
        );
        client.write_all(request.as_bytes()).unwrap();
        respond(app, &mut server).unwrap();
        drop(server);
        let mut response = String::new();
        client.read_to_string(&mut response).unwrap();
        response
    }

    fn section(body: &str, id: &str) -> String {
        let start = body
            .find(&format!("<section id=\"{id}\">"))
            .unwrap_or_else(|| panic!("missing {id} in {body}"));
        let rest = &body[start..];
        let open = rest.find('>').unwrap() + 1;
        let close = rest.find("</section>").unwrap();
        rest[open..close].to_owned()
    }

    #[test]
    fn publish_is_localhost_only() {
        assert_eq!(PUBLISH, "127.0.0.1:8080:8080");
    }

    #[test]
    fn image_stays_non_root_without_key_env() {
        let dockerfile = include_str!("../../../Dockerfile");
        assert!(dockerfile.contains("USER seaotter"), "{dockerfile}");
        assert!(
            dockerfile.contains("[\"/usr/local/bin/seaotter-agent\", \"--health\"]"),
            "{dockerfile}"
        );
        for key in ["OPENAI_API_KEY", "ANTHROPIC_API_KEY", "TYPESAFE_API_KEY"] {
            assert!(
                !dockerfile.contains(&format!("ENV {key}")),
                "{key} in {dockerfile}"
            );
        }
    }

    #[test]
    fn effects_workflow() {
        let args = args(&["--workflow", "examples/effects"]);
        let Request::Workflow(workflow, bin, rest) = parse(&args).unwrap() else {
            panic!("expected a workflow");
        };
        assert_eq!(workflow, "examples/effects");
        assert_eq!(bin, "effects");
        assert!(rest.is_empty());
    }

    #[test]
    fn health_does_not_select_a_binary() {
        let args = args(&["--health"]);
        assert!(matches!(parse(&args).unwrap(), Request::Health));
    }

    #[test]
    fn workspace_is_created_without_a_log() {
        let root = std::env::temp_dir().join(format!("seaotter-record-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        ensure_workspace(&root).unwrap();
        assert!(root.join("workspace").is_dir());
        assert!(!root.join("log").exists());
        std::fs::remove_dir_all(&root).unwrap();
    }
}
