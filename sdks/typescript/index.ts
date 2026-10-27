import { spawn, type ChildProcessWithoutNullStreams } from "node:child_process";
import { createInterface } from "node:readline";

export type AgentEvent = {
  kind: string;
  action?: string;
  name?: string;
  text?: string;
  command?: string;
  error?: string;
};

export class Agent {
  readonly #child: ChildProcessWithoutNullStreams;
  readonly #lines: AsyncIterator<string>;

  static start(bin = "seaotter-agent"): Agent {
    return new Agent(bin);
  }

  private constructor(bin: string) {
    this.#child = spawn(bin, ["--mode", "rpc"], {
      stdio: ["pipe", "pipe", "inherit"],
    });
    this.#lines = createInterface({ input: this.#child.stdout })[Symbol.asyncIterator]();
  }

  prompt(message: string): Promise<AgentEvent[]> {
    return this.#round({ command: "prompt", message });
  }

  continue(): Promise<AgentEvent[]> {
    return this.#round({ command: "continue" });
  }

  close(): void {
    this.#child.kill();
  }

  async #round(command: { command: string; message?: string }): Promise<AgentEvent[]> {
    this.#child.stdin.write(`${JSON.stringify(command)}\n`);
    const events: AgentEvent[] = [];
    while (true) {
      const next = await this.#lines.next();
      if (next.done) {
        throw new Error("seaotter-agent closed");
      }
      const event = JSON.parse(next.value) as AgentEvent;
      events.push(event);
      if (event.kind === "response") {
        return events;
      }
    }
  }
}
