# Design

This note contrasts the harness with a traditional model-owned agent loop (ReAct and its cousins). [SPEC.md](SPEC.md) is the source of truth for behavior. Measurements live with the examples and in [`paper/`](paper/).

## Who owns the next step

The next node is a Rust [`Transition`](crates/harness-core/src/workflow.rs): a function of the node that just ran, its outcome, and the state. The machine stores model output under the node id and then asks that function where to go.

When confidence is below the node's minimum, the machine skips the transition. [`Workflow::on_low`](crates/harness-core/src/workflow.rs) sends `SafeDefault`, `Escalate`, or `Human` to a handler registered in Rust. An action with no handler halts the run.

In a ReAct loop the model owns that choice. Each round it emits a thought and a tool call, or it decides to stop. The harness treats that choice as code.

## Where tokens are spent

A decision call sends the state slice named by `reads` and a fixed set of typed questions (`Choice`, `Score`, or `Noul`). The response is probabilities over those labels plus a confidence. The call is one round trip.

A generative call is a separate node. It is one `complete` with a task, the same kind of state slice, a JSON schema (or an explicit free-text opt-out), and `max_tokens`. A [`Gate`](crates/harness-core/src/node.rs) usually sits in front of it: the model runs only when a prior decision's top label matches the allowed value and the confidence policy continued.

A ReAct loop spends another round for every thought and every tool result. The prompt grows with the transcript, and the model decides how many rounds to take.

## Latency

A typed decision is one call. A gated draft is one decision plus one generative call. A ReAct run is a chain of tool rounds, and the tail of that chain is chosen at runtime.

Checked-in samples for the two shapes live in the [ticket-router](examples/ticket-router/README.md) and [gated-generate](examples/gated-generate/README.md) notes. The comparison against a ReAct baseline will live in [`paper/`](paper/).

## Failure modes

A wrong branch is a bad match on a typed answer. The event log records the node, the output, and the confidence. Replay and `diff_runs` can show where two runs diverged.

Low confidence takes the registered edge, or it halts when that edge has no handler. `RetryStronger` makes one more call to a stronger decision model. If that call is still low, the edge is `Escalate`.

A generative body that fails its schema returns `SchemaInvalid` before the transition runs. The body is data that did not validate. It never selects a node.

A ReAct failure is often another tool round: a bad call, a retry, and a longer transcript. This harness ends the run on the cases above.

## Security

Effects run in Rust, and only after the machine has granted a [`Capability`](crates/harness-core/src/effect.rs): `FileRead`, `HttpGet`, or `DangerousShell`. The node names the effect. The effect's scope (a path, a host, or a fixed argv) is set when Rust builds it.

Decision and generative requests carry the state slice and the question or task. They carry no tool list. The model cannot name an effect, invent a command, or write memory.

## Why System One is the default

System One is the call that returns a typed answer and a confidence the machine can branch on. That is the ordinary AI step: classify, score, or allow.

A generative call is the escape hatch for prose or a document. It stays inside one node, behind a gate, with a schema. [`gated-generate`](examples/gated-generate) is that shape. [`ticket-router`](examples/ticket-router) is a decision with no generative call at all: Rust matches the label and the confidence edge.
