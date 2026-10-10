# ticket-router

System One classifies a support ticket. Rust routes on the chosen label and on confidence. The model does not pick a tool or the next step.

`normalize` trims the subject and body. `classify` asks a `Choice` over `billing`, `technical`, and `account`, and reads only the normalized ticket. At confidence 0.7 or above, a Rust match on the top label sends the ticket to that team. Below 0.7, the machine takes the safe-default edge to `queue` and skips the match.

## Checked-in sample

`cargo run -p ticket-router` loads `fixtures/billing.json` and does not call the network. That sample costs **$0.00012** and takes **140 ms**.

```
destination: billing
route: billing
confidence: 0.88
latency_ms: 140
cost_usd: 0.00012
```

A low score uses the same route set and lands in the queue:

```sh
cargo run -p ticket-router -- fixtures/low_confidence.json
```

## Refresh the numbers

`--live` classifies the same ticket with TypeSafe and ignores the recorded answer. Copy the printed latency and cost into `fixtures/billing.json` under `usage` and into the sample above.

```sh
TYPESAFE_API_KEY=... cargo run -p ticket-router -- --live
```

`TYPESAFE_BASE_URL` and `TYPESAFE_MODEL` override the default endpoint and model.
