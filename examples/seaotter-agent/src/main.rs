use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::Path;
use std::process::{Command, ExitCode};

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

fn page() -> &'static str {
    "<!doctype html>
<html>
<body>
<section id=\"chat\"></section>
<section id=\"graph\"></section>
<section id=\"run\"></section>
</body>
</html>
"
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
    for stream in listener.incoming() {
        let mut stream = stream.map_err(|err| format!("accepting a connection: {err}"))?;
        respond(&mut stream)?;
    }
    Ok(())
}

fn respond(stream: &mut TcpStream) -> Result<(), String> {
    let mut buf = [0; 1024];
    let _ = stream.read(&mut buf);
    let body = page();
    let response = format!(
        "HTTP/1.1 200 OK\r\ncontent-type: text/html; charset=utf-8\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
        body.len()
    );
    stream
        .write_all(response.as_bytes())
        .map_err(|err| format!("writing the page: {err}"))
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

    use super::{PUBLISH, Request, ensure_workspace, page, parse, respond};

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
            respond(&mut stream).unwrap();
        });
        let mut stream = std::net::TcpStream::connect(addr).unwrap();
        stream
            .write_all(b"GET / HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n")
            .unwrap();
        let mut response = String::new();
        stream.read_to_string(&mut response).unwrap();
        server.join().unwrap();
        assert!(addr.ip().is_loopback());
        assert!(response.contains(page()), "{response}");
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
