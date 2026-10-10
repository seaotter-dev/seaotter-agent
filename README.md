# seaotter-agent

A small Rust library for code-owned agent harnesses. Code owns control flow, System One models make atomic typed decisions with confidence, and generative LLMs run only inside explicit, schema-bounded nodes.

The section explaining how this inverts Hermes and OpenClaw is coming in SEA-356.

* [`SPEC.md`](SPEC.md): v1 spec and source of truth
* [`DESIGN.md`](DESIGN.md): design versus a traditional agent loop
* [`examples/ticket-router`](examples/ticket-router) and [`examples/gated-generate`](examples/gated-generate)
* [`paper/`](paper): arXiv paper and measurements

## Build

```sh
cargo test --workspace
```
