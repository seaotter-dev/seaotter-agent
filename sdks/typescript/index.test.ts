import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { test } from "node:test";
import { fileURLToPath } from "node:url";

import { Agent } from "./index.ts";

const bin = fileURLToPath(new URL("../../target/debug/seaotter-agent", import.meta.url));

test("prompt returns the event stream and continue repeats it", async () => {
  const agent = Agent.start(bin);
  try {
    const events = await agent.prompt("hello");
    assert.deepEqual(
      events.map((event) => event.kind),
      ["decision", "effect", "reply", "response"],
    );
    assert.equal(events[0]?.action, "read");
    assert.equal(events[1]?.name, "read");
    assert.equal(events[2]?.text, "hello");
    assert.equal(events[3]?.command, "prompt");

    const continued = await agent.continue();
    assert.equal(continued.find((event) => event.kind === "reply")?.text, "hello");
    assert.equal(continued.at(-1)?.command, "continue");
  } finally {
    agent.close();
  }
});

test("continue with no prompt is an error and has no decision", async () => {
  const agent = Agent.start(bin);
  try {
    const events = await agent.continue();
    assert.equal(events.length, 1);
    assert.equal(events[0]?.kind, "response");
    assert.equal(events[0]?.error, "no prompt");
    assert.equal(
      events.some((event) => event.kind === "decision"),
      false,
    );
  } finally {
    agent.close();
  }
});

test("the package does not mention an action loop", () => {
  const source = readFileSync(new URL("./index.ts", import.meta.url), "utf8");
  assert.equal(source.includes("action loop"), false);
});
