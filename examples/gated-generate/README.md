# gated-generate

A System One yes/no gate decides whether a reply may be drafted. Rust then runs one schema-bound generative call, or takes a safe default. The model does not pick a tool or the next step.

`normalize` trims the request. `gate` asks "Is it safe to draft a reply to this request?" and reads only that request. At confidence 0.7 or above, the run continues to `draft`. The gate on `draft` calls the model only when the top label is `true`. A `false` answer skips the call and lands in `safe`. Below 0.7, the machine takes the safe-default edge to `safe` and never visits `draft`.

`draft` asks for JSON with a string `reply` and a limit of 128 tokens. Rust validates that body before the transition runs. A body that fails the schema stops the run, so it cannot choose a node.

`--live openai` and `--live anthropic` use the same graph. Only the generative client changes.

## Checked-in sample

`cargo run -p gated-generate` loads `fixtures/allow.json` and does not call the network. That allow path costs **$0.00052** and takes **410 ms**: the decision is $0.00004 / 90 ms, and the draft is $0.00048 / 320 ms.

```
destination: drafted
allowed: true
confidence: 0.86
decision_latency_ms: 90
decision_cost_usd: 0.00004
generation_latency_ms: 320
generation_cost_usd: 0.00048
reply: I refunded the duplicate charge on invoice 1042.
```

A deny answer visits `draft` and skips the model:

```sh
cargo run -p gated-generate -- fixtures/deny.json
```

A low score never visits `draft`:

```sh
cargo run -p gated-generate -- fixtures/low_confidence.json
```

## Refresh the numbers

Either command classifies the allow fixture with TypeSafe and drafts with the named client. Copy the printed latency and cost into `fixtures/allow.json` and into the sample above.

```sh
TYPESAFE_API_KEY=... OPENAI_API_KEY=... OPENAI_MODEL=... cargo run -p gated-generate -- --live openai
TYPESAFE_API_KEY=... ANTHROPIC_API_KEY=... ANTHROPIC_MODEL=... cargo run -p gated-generate -- --live anthropic
```

`OPENAI_BASE_URL` and `ANTHROPIC_BASE_URL` override the default endpoints. `TYPESAFE_BASE_URL` and `TYPESAFE_MODEL` override the gate.
