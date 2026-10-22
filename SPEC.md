# Code-owned agent harness

This file is the source of truth for implementation. Changes to scope land here first.

## Goal

Open-source a small Rust library that inverts the normal agent harness:

* Code owns all control flow and deterministic work.
* System One models (TypeSafe Jev and compatible `/v1/systemone` endpoints) are the default AI primitive for atomic, typed decisions.
* Generative LLMs (Grok, Claude, Astra, OpenAI-compatible, Anthropic) are used only inside explicit, single-shot, schema-bounded nodes.
* Every AI call is atomic and constrained. No open-ended ReAct loop, no model-chosen tools, no model-written memory.

v1 also includes a short arXiv paper on the architecture, the cost and reliability motivation, and early measurements.

The issues listed under [v1 slices](#v1-slices) are the v1 work breakdown. Stay inside the non-goals.

## Non-goals (v1)

* Skill marketplace or markdown-as-instruction skills
* Model-owned memory writes
* Multi-agent routing or a gateway process
* Open tool lists the model can expand
* A full agent loop where the model decides the next step

## Core principles

1. The next step is always a Rust match or state-machine transition.
2. Decision-model calls take only the state slice they need plus typed questions (`Choice`, `Score`, `Noul`) and return probabilities and confidence.
3. Confidence thresholds are first-class control-flow edges.
4. Generative calls require a schema (or an explicit free-text opt-out) and are normally preceded by a System One gate.
5. Effects are capability-scoped and executed by Rust.
6. Every decide, generate, and effect step emits an append-only event for audit and replay.
7. Design-time use of generative models is allowed. Runtime never treats LLM output as control flow.

## Repo layout

```
/
├── crates/
│   ├── harness-core/
│   ├── harness-providers/
│   └── harness-effects/
├── examples/
│   ├── ticket-router/
│   └── gated-generate/
├── paper/
├── DESIGN.md
├── README.md
└── SPEC.md
```

## v1 acceptance

* `Machine` runs a workflow that mixes deterministic steps, System One decisions with confidence branching, and one schema-validated generative call.
* No path exists for a model to choose an arbitrary next tool or write memory.
* The event log captures every AI and effect step. Replay can diff two runs.
* Two examples build and run with documented cost and latency notes.
* The arXiv draft is complete enough to submit, with measurements included.
* README explains the inversion versus Hermes and OpenClaw in under 300 words.

## v1 slices

Tracked in Linear under the "Code-owned agent harness" project.

1. Core machine
   * SEA-339 Scaffold the Rust workspace and land SPEC.md
   * SEA-340 Define the public harness traits and request types
   * SEA-341 Run workflows on an explicit Machine node graph
   * SEA-342 Branch on confidence inside the Machine
2. Model providers
   * SEA-343 Add the TypeSafe System One decision client
   * SEA-344 Add an OpenAI-compatible generative client
   * SEA-345 Add an Anthropic generative client
3. Capability-scoped effects
   * SEA-346 Add a path-scoped file read effect
   * SEA-347 Add an allowlisted HTTP GET effect
   * SEA-348 Add an off-by-default shell effect
4. Event log and replay
   * SEA-349 Record every decide, generate, and effect step
   * SEA-350 Replay a trace and diff two runs
5. Fixture eval
   * SEA-351 Add question and state helpers
   * SEA-352 Assert choice and confidence ranges from fixtures
6. Reference examples
   * SEA-353 Ship the support-ticket router example
   * SEA-354 Ship the safety-gated generation example
7. Design docs
   * SEA-355 Write DESIGN.md against a traditional agent loop
   * SEA-356 Explain the inversion in the README
8. arXiv paper
   * SEA-357 Draft the arXiv paper
   * SEA-358 Measure tokens, latency, and branch stability against ReAct
   * SEA-359 Build the paper PDF and document regeneration

## Goal (v2)

The image is the local deployment unit. `docker build` and `docker run` are the iteration loop. The harness stays a library: code owns the next step, System One answers typed questions, and generative calls stay schema-bounded behind a gate.

The issues listed under [v2 slices](#v2-slices) are the v2 work breakdown. Stay inside the non-goals.

## Non-goals (v2)

* A full agent loop where the model decides the next step
* A multi-tenant control plane, queue, or fleet manager
* A skill marketplace or model-written memory
* Ambient credentials inside the container
* A tool list the model can expand
* Remote sandbox registration or production orchestration
* A third-party integration layer such as Composio

## Image requirements

* Multi-stage build. The runtime stage is alpine with ca-certificates, or distroless if that image still runs the binary and is smaller.
* Default user is not root.
* Binaries live in `/usr/local/bin`.
* `/data` is the writable path and is mounted from the host.
* No secrets are copied into the image. Keys arrive through `-e` or an env file: `OPENAI_API_KEY`, `OPENAI_MODEL`, `ANTHROPIC_API_KEY`, and `TYPESAFE_API_KEY`. `USER_ID` is recorded and is not a credential.
* ENTRYPOINT accepts `--workflow`.
* A healthcheck confirms the binary starts and does not call a model.

## v2 acceptance

* `docker build -t seaotter-agent:dev .` succeeds, and `docker run` with `-v "$(pwd)/data:/data"` runs `examples/ticket-router`.
* `gated-generate` has the same shape, including an offline fixture run that makes no network call.
* The log on `/data` survives a new container that mounts the same directory.
* Effects stay capability-scoped. The model receives no tool list.
* A missing human-gate approval does not call the generative model.

## v2 slices

Tracked in Linear under the [Code-owned harness v2 — local Docker](https://linear.app/seaotter-dev/project/code-owned-harness-v2-local-docker-dc206a39c00f) project.

1. Spec
   * SEA-380 Land the v2 local-container spec in SPEC.md
2. Image
   * SEA-381 Add a multi-stage Dockerfile and .dockerignore
   * SEA-382 Run the container as a non-root user
3. Entrypoint
   * SEA-383 Add a thin CLI with --workflow and use it as ENTRYPOINT
   * SEA-384 Add a healthcheck that confirms the binary starts
4. Local iteration
   * SEA-386 Mount /data for the event log and workspace
   * SEA-385 Run both examples from the container
5. Effects and network
   * SEA-387 Exercise FileRead, HttpGet, and shell inside the container
   * SEA-389 Add a test-only destination restriction
6. Log and human gate
   * SEA-388 Persist each run on the volume and replay it
   * SEA-390 Add a local human-gate node

## Goal (consultant MCP)

The user installs a stdio MCP. The host Allow only lets that chat call the server. Seaotter stores the owner, the tier, the tool grant, and the human gate. A consultant reply may call the catalog. The saved document is the transition. A run does not ask the consultant which node is next, and the model request carries no tool list. The tool result includes a Mermaid wireframe of the graph.

This phase grants one Composio effect on the workflow document. The v2 container image is unchanged.

The issues listed under [consultant MCP slices](#consultant-mcp-slices) are this phase's work breakdown. Stay inside the non-goals.

## Non-goals (consultant MCP)

* A host permission blob stored as a grant
* A `dangerous` tier, or a document path that loads `shell`
* An MCP tool that adds a grant (`grant add` is a CLI subcommand)
* Platform auth
* A hosted HTTP API
* The local page in the Local workflow app project

`DangerousShell` in `crates/harness-effects/src/shell.rs` stays for the effects example. The document loader does not call it.

## Consultant MCP acceptance

* The host Allow only lets the chat call the MCP.
* Seaotter stores the owner, the tier, the tool grant, and the human gate.
* Tiers are `read` and `write`. A document that names `shell` or `dangerous` fails to load.
* A tool result includes a Mermaid wireframe.
* Composio is an effect backend. The workflow file stores a connection id and an action name, and it stores no token.
* A later `platform` provider can replace Composio without a document change. This phase does not implement that provider.

## Consultant MCP slices

Tracked in Linear under the [Consultant MCP](https://linear.app/seaotter-dev/project/consultant-mcp-b6a29292e9de) project.

1. Spec
   * SEA-403 Land the consultant-MCP spec
2. Document
   * SEA-404 Add a serializable workflow document
   * SEA-405 Run a workflow document on the machine
3. Grants
   * SEA-406 Require an owner, a tier, and a tool grant
   * SEA-410 Skip a write effect until its human gate passes
   * SEA-408 Write the grant decision to the run log
4. Catalog
   * SEA-407 Freeze the builder catalog
   * SEA-409 Apply a consultant reply as catalog calls
5. MCP
   * SEA-412 Serve the stdio MCP
   * SEA-414 Return the wireframe in the tool result
6. Composio
   * SEA-411 Run a granted Composio action
   * SEA-413 Check the gates before the credential backend
