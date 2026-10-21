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

## Container

`docker build -t seaotter-agent:dev .` produces an image that runs as `seaotter` (uid 10001), not root. `/data` is the writable path and is meant to be a host mount.

No local effect needs root. `FileRead` and `HttpGet` run as that user. `DangerousShell` stays off unless Rust grants it. The command is fixed when the effect is built, and it runs without a shell.

Edit a workflow, build the image, and run it offline. A bare run uses the fixtures baked into the image and does not call a model.

```sh
docker build -t seaotter-agent:dev .
docker run --rm seaotter-agent:dev --workflow examples/ticket-router
docker run --rm seaotter-agent:dev --workflow examples/gated-generate
```

A change to one example can reuse that image. Rebuild the binary with the Alpine musl toolchain and bind-mount it over `/usr/local/bin/ticket-router`:

```sh
docker run --rm -v "$(pwd)":/src -w /src -e CARGO_TARGET_DIR=/src/target/docker rust:1-alpine \
  sh -c "apk add --no-cache musl-dev && cargo build --locked --release -p ticket-router"
docker run --rm \
  -v "$(pwd)/target/docker/release/ticket-router:/usr/local/bin/ticket-router" \
  seaotter-agent:dev --workflow examples/ticket-router
```

`gated-generate` is the same shape with `-p gated-generate` and `target/docker/release/gated-generate`.

Mount a host directory at `/data`. `/data/log` and `/data/workspace` stay on that directory for the next container. Keys arrive through `-e` or an env file: `OPENAI_API_KEY`, `OPENAI_MODEL`, `ANTHROPIC_API_KEY`, and `TYPESAFE_API_KEY`. `USER_ID` is recorded in `/data/log` and is not a credential.

```sh
docker run --rm -v "$(pwd)/data:/data" \
  -e OPENAI_API_KEY -e OPENAI_MODEL -e ANTHROPIC_API_KEY -e TYPESAFE_API_KEY -e USER_ID \
  seaotter-agent:dev --workflow examples/ticket-router
docker run --rm -v "$(pwd)/data:/data" --env-file .env \
  seaotter-agent:dev --workflow examples/gated-generate
```
