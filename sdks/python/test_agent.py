import unittest
from pathlib import Path

from agent import Agent

BIN = Path(__file__).resolve().parents[2] / "target" / "debug" / "seaotter-agent"


class AgentTest(unittest.TestCase):
    def test_prompt_and_continue(self):
        agent = Agent.start(str(BIN))
        try:
            events = agent.prompt("hello")
            self.assertEqual(
                [event["kind"] for event in events],
                ["decision", "effect", "reply", "response"],
            )
            self.assertEqual(events[0]["action"], "read")
            self.assertEqual(events[1]["name"], "read")
            self.assertEqual(events[2]["text"], "hello")
            self.assertEqual(events[3]["command"], "prompt")

            continued = agent.continue_()
            reply = next(event for event in continued if event["kind"] == "reply")
            self.assertEqual(reply["text"], "hello")
            self.assertEqual(continued[-1]["command"], "continue")
        finally:
            agent.close()

    def test_continue_without_prompt(self):
        agent = Agent.start(str(BIN))
        try:
            events = agent.continue_()
            self.assertEqual(len(events), 1)
            self.assertEqual(events[0]["kind"], "response")
            self.assertEqual(events[0]["error"], "no prompt")
            self.assertFalse(any(event["kind"] == "decision" for event in events))
        finally:
            agent.close()

    def test_source_does_not_mention_an_action_loop(self):
        source = Path(__file__).with_name("agent.py").read_text()
        self.assertNotIn("action loop", source)


if __name__ == "__main__":
    unittest.main()
