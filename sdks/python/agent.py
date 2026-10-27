import json
import subprocess


class Agent:
    def __init__(self, bin):
        self._proc = subprocess.Popen(
            [bin, "--mode", "rpc"],
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            text=True,
        )

    @staticmethod
    def start(bin="seaotter-agent"):
        return Agent(bin)

    def prompt(self, message):
        return self._round({"command": "prompt", "message": message})

    def continue_(self):
        return self._round({"command": "continue"})

    def close(self):
        if self._proc.stdin is not None:
            self._proc.stdin.close()
        if self._proc.poll() is None:
            self._proc.kill()
        if self._proc.stdout is not None:
            self._proc.stdout.close()
        self._proc.wait()

    def _round(self, command):
        stdin = self._proc.stdin
        stdout = self._proc.stdout
        if stdin is None or stdout is None:
            raise RuntimeError("seaotter-agent closed")
        stdin.write(json.dumps(command) + "\n")
        stdin.flush()
        events = []
        while True:
            line = stdout.readline()
            if line == "":
                raise RuntimeError("seaotter-agent closed")
            event = json.loads(line)
            events.append(event)
            if event.get("kind") == "response":
                return events
