# seaotter-agent

A small Rust library for code-owned agent harnesses.

## Inversion

[Hermes](https://github.com/NousResearch/hermes-agent/blob/main/website/docs/developer-guide/agent-loop.md) (Nous Research) and [OpenClaw](https://docs.openclaw.ai/concepts/agent-loop) both run a model-owned loop. The model sees tools, calls them, and decides when to stop. Hermes centers that loop on `AIAgent`, which can write memory and create skills. OpenClaw centers it on a gateway that loads skills into the prompt and runs an embedded agent per session.

This library inverts that. Code owns the next step: a Rust match or a confidence edge. System One models answer typed questions (`Choice`, `Score`, `Noul`) and return probabilities and confidence. Generative models run only inside an explicit node with a schema and `max_tokens`, usually behind a gate. Effects run in Rust after a capability grant (`FileRead`, `HttpGet`, `DangerousShell`). Requests carry the state slice and the question or task. They carry no tool list.

v1 is not a general agent framework. It has no skill marketplace, no model-owned memory writes, no multi-agent routing or gateway, no open tool list, and no loop where the model picks the next step.

* [SPEC.md](SPEC.md) is the contract.
* [DESIGN.md](DESIGN.md) compares this with a ReAct loop.
* [ticket-router](examples/ticket-router) routes a ticket from a typed choice.
* [gated-generate](examples/gated-generate) drafts only after a yes/no gate.
* [paper/](paper/) holds the measurements.

## Build

```sh
cargo build --workspace
cargo test --workspace
```
