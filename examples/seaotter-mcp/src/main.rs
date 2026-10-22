use std::io::{BufRead, Write};

use seaotter_mcp::Server;
use serde_json::Value;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let result = match args.as_slice() {
        [] => serve(),
        [command, action, name] if command == "grant" && action == "add" => grant_add(name),
        _ => {
            eprintln!("usage: seaotter-mcp");
            eprintln!("       seaotter-mcp grant add <name>");
            std::process::exit(2);
        }
    };
    if let Err(err) = result {
        eprintln!("{err}");
        std::process::exit(1);
    }
}

fn grant_add(name: &str) -> Result<(), Box<dyn std::error::Error>> {
    let dir = std::env::current_dir()?;
    Server::open(dir).grant_add(name)?;
    Ok(())
}

fn serve() -> Result<(), Box<dyn std::error::Error>> {
    let runtime = tokio::runtime::Runtime::new()?;
    let dir = std::env::current_dir()?;
    let mut server = Server::open(dir);
    let stdin = std::io::stdin();
    let mut stdout = std::io::stdout();
    for line in stdin.lock().lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let message: Value = match serde_json::from_str(&line) {
            Ok(message) => message,
            Err(err) => {
                eprintln!("seaotter-mcp: {err}");
                continue;
            }
        };
        if let Some(response) = runtime.block_on(server.handle(&message)) {
            writeln!(stdout, "{}", serde_json::to_string(&response)?)?;
            stdout.flush()?;
        }
    }
    Ok(())
}
