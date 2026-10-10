# Code-owned agent harness: v1 spec

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
