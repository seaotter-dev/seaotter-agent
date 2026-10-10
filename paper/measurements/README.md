# Measurements

These files are the raw runs for the paper evaluation. The harness side calls the ticket router and the gated-generation example. The baseline is a ReAct-style loop on the same OpenAI-compatible model the gated draft uses. `inspect` returns the fixture text. `finish` ends the loop. A loop that reaches the step cap is branch `unfinished`.

- OpenAI model: `gpt-4.1-mini-2025-04-14`
- TypeSafe model: `jev-latest`
- Ticket fixture: `examples/ticket-router/fixtures/billing.json`
- Gated fixture: `examples/gated-generate/fixtures/allow.json`
- Repeats: 10
- ReAct step cap: 4
- Ticket ReAct max tokens: 256
- Gated ReAct max tokens: 512

Rerun:

```sh
cargo run -p measure -- --repeats 10
```

The command reads `TYPESAFE_API_KEY`, `OPENAI_API_KEY`, and `OPENAI_MODEL` from the environment. `TYPESAFE_MODEL` overrides the System One model.
