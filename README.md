# seaotter-agent

A code-owned agent harness. Rust owns the next step.

System One answers a typed question and returns probabilities and confidence. System Two writes only for reply and edit. Effects run in Rust after a capability grant. A request carries the state slice and the question. It carries no tool list.

[SPEC.md](SPEC.md) is the contract. [DESIGN.md](DESIGN.md) compares this with a ReAct loop.

Run a session in the working directory, automate it with `--print` or `--mode json`, control it over `--mode rpc`, or spawn the binary from the TypeScript or Python SDK. `--workflow` still runs the three examples. The MCP server stays.

## Getting started

Build the command:

```sh
cargo build -p seaotter-agent
```

Start it in the directory where you want it to work:

```sh
cd /path/to/project
/path/to/seaotter-agent/target/debug/seaotter-agent
```

With no arguments it prints the working directory, the text of `AGENTS.md` when that file is in the directory, and a `> ` prompt. It writes one JSONL line under `.seaotter/sessions/` and exits.

```text
/path/to/project
the text of AGENTS.md
>
```

`seaotter-agent --continue` prints the active branch of the newest session in that directory.

## Print and JSON

```sh
seaotter-agent --print "hello"
```

writes the message and a newline:

```text
hello
```

```sh
seaotter-agent --mode json "hello"
```

writes three JSON lines: a decision whose action is `read`, an effect record named `read`, and a reply whose text is the message. The effect line records the name. It leaves the filesystem alone.

```text
{"action":"read","kind":"decision"}
{"kind":"effect","name":"read"}
{"kind":"reply","text":"hello"}
```

## RPC

`seaotter-agent --mode rpc` reads one JSON command per stdin line and writes events through a response line.

```json
{"command":"prompt","message":"hello"}
{"command":"continue"}
{"command":"abort"}
```

`prompt` writes the same three events as `--mode json`, then a response whose command is `prompt`. `continue` repeats those events for the remembered message, then a response whose command is `continue`. With no prompt yet, the response carries `error` `no prompt` and no events. `abort` writes its response and the process exits.

## SDKs

Both packages spawn `seaotter-agent --mode rpc` and return the event list. Neither is published, and neither contains the loop.

TypeScript, in [sdks/typescript/index.ts](sdks/typescript/index.ts):

```ts
import { Agent } from "./index.ts";

const agent = Agent.start("target/debug/seaotter-agent");
const events = await agent.prompt("hello");
await agent.continue();
agent.close();
```

Python, in [sdks/python/agent.py](sdks/python/agent.py). The method is `continue_` because `continue` is a Python keyword. The JSON command is still `continue`.

```python
from agent import Agent

agent = Agent.start("target/debug/seaotter-agent")
events = agent.prompt("hello")
agent.continue_()
agent.close()
```

## Workflows

`seaotter-agent --workflow` runs one of the example binaries. Those binaries need to be on `PATH`. The container image puts them there.

```sh
seaotter-agent --workflow examples/ticket-router
seaotter-agent --workflow examples/gated-generate
seaotter-agent --workflow examples/effects
```

[ticket-router](examples/ticket-router) routes a ticket from a typed choice. [gated-generate](examples/gated-generate) drafts only after a yes/no gate. [effects](examples/effects) runs `FileRead`, an allowlisted `HttpGet`, and a shell whose argv is fixed in Rust. That shell stays off until the example calls `Shell::grant` and the machine holds `DangerousShell`.

## Container

```sh
docker build -t seaotter-agent:dev .
```

The image runs as `seaotter` (uid 10001). The entrypoint is `/usr/local/bin/seaotter`, which is a link to `seaotter-agent`. `/workspace` is the working directory and the volume. `/data` is the log path. The image has no API-key environment variable.

```sh
docker run --rm seaotter-agent:dev --health
docker run --rm -v "$PWD:/workspace" seaotter-agent:dev
docker run --rm seaotter-agent:dev --workflow examples/ticket-router
docker run --rm seaotter-agent:dev --workflow examples/gated-generate
docker run --rm -v "$(pwd)/data:/data" seaotter-agent:dev --workflow examples/effects
```

`--health` exits 0. A host directory mounted on `/workspace` is the session directory. Model workflows receive keys at `docker run`:

```sh
docker run --rm -v "$(pwd)/data:/data" \
  -e OPENAI_API_KEY -e OPENAI_MODEL -e ANTHROPIC_API_KEY -e TYPESAFE_API_KEY -e USER_ID \
  seaotter-agent:dev --workflow examples/ticket-router
```

`gated-generate` drafts when `/data/approval` exists or `APPROVAL_TOKEN` is set. A rebuilt example binary can be bind-mounted over its path in `/usr/local/bin` so that one example can change without a full image rebuild.

## Consultant MCP

`seaotter-mcp` speaks MCP over stdio. The tools are `workflow_propose`, `workflow_show`, `workflow_save`, `workflow_run`, and `grant_list`. A person adds a grant with `seaotter-mcp grant add <name>`. That subcommand is not an MCP tool.

The process inherits keys from the environment. The workflow file does not store them.

Claude:

```json
{
  "mcpServers": {
    "seaotter": {
      "command": "seaotter-mcp"
    }
  }
}
```

Cursor, in `.cursor/mcp.json`:

```json
{
  "mcpServers": {
    "seaotter": {
      "command": "seaotter-mcp"
    }
  }
}
```

## Development

```sh
cargo test --workspace
```

The SDK tests expect `target/debug/seaotter-agent` from `cargo build -p seaotter-agent`. `cargo test` does not run them.

```sh
node --test --experimental-strip-types sdks/typescript/index.test.ts
```

```sh
cd sdks/python && python3 -m unittest test_agent.py
```

## License

MIT OR Apache-2.0
