"""Check the event log emitted by a standalone Copilot CLI using a local provider."""

import json
import os
from datetime import datetime
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
import subprocess
import sys
import tempfile
import threading


MARKER = "EDITH_NATIVE_OK"
MODEL = "gpt-4.1"


class Provider(BaseHTTPRequestHandler):
    def do_POST(self):
        if self.path != "/v1/chat/completions":
            self.send_error(404)
            return

        payload = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
        if payload.get("model") != MODEL:
            self.send_error(400, "Unexpected model")
            return

        completion = {
            "id": "chatcmpl-edith",
            "object": "chat.completion.chunk",
            "created": 1,
            "model": MODEL,
        }
        chunks = [
            {
                "choices": [{
                    "index": 0,
                    "delta": {"role": "assistant", "content": MARKER},
                    "finish_reason": None,
                }],
            },
            {"choices": [{"index": 0, "delta": {}, "finish_reason": "stop"}]},
            {
                "choices": [],
                "usage": {
                    "prompt_tokens": 42,
                    "completion_tokens": 5,
                    "total_tokens": 47,
                    "prompt_tokens_details": {"cached_tokens": 0},
                    "completion_tokens_details": {"reasoning_tokens": 0},
                },
            },
        ]
        self.send_response(200)
        self.send_header("Content-Type", "text/event-stream")
        self.end_headers()
        for chunk in chunks:
            self.wfile.write(f"data: {json.dumps(completion | chunk)}\n\n".encode())
        self.wfile.write(b"data: [DONE]\n\n")

    def log_message(self, *_args):
        pass


def verify_records(home):
    events = []
    for log in (home / "session-state").glob("*/events.jsonl"):
        for line in log.read_text(encoding="utf-8").splitlines():
            event = json.loads(line)
            if event.get("type") == "session.usage_record":
                events.append(event)

    if len(events) != 1:
        raise ValueError(f"Expected one usage record, found {len(events)}")
    event, = events
    timestamp = datetime.fromisoformat(event["timestamp"].replace("Z", "+00:00"))
    if timestamp.utcoffset() is None or not event.get("id"):
        raise ValueError("Usage record needs a timestamp with offset and an ID")
    usage = event["data"]["usage"]
    expected = {
        "model": MODEL,
        "inputTokens": 42,
        "outputTokens": 5,
        "cacheReadTokens": 0,
        "reasoningTokens": 0,
    }
    if {key: usage.get(key) for key in expected} != expected:
        raise ValueError("Usage counters did not match the provider response")
    if type(usage.get("duration")) is not int or usage["duration"] < 0:
        raise ValueError("Usage record needs a nonnegative API duration")
    print("Copilot CLI logged 42 input tokens, 5 output tokens, and measured zeroes.")


def main(command):
    if not command:
        raise SystemExit("usage: copilot_native.py <copilot command> [command args]")

    with tempfile.TemporaryDirectory(prefix="edith-copilot-") as directory:
        workdir = Path(directory)
        home = workdir / "copilot"
        with ThreadingHTTPServer(("127.0.0.1", 0), Provider) as server:
            worker = threading.Thread(target=server.serve_forever, daemon=True)
            worker.start()
            try:
                env = {
                    key: value for key, value in os.environ.items()
                    if not key.upper().startswith(("COPILOT_", "OTEL_"))
                    and key.upper() not in {"GH_TOKEN", "GITHUB_TOKEN"}
                }
                env.update(
                    COPILOT_HOME=str(home),
                    COPILOT_OFFLINE="true",
                    COPILOT_PROVIDER_BASE_URL=f"http://127.0.0.1:{server.server_port}/v1",
                    COPILOT_MODEL=MODEL,
                    COPILOT_TELEMETRY_DISABLED="true",
                )
                result = subprocess.run(
                    [*command, "-p", f"Reply only with {MARKER}", "--no-color"],
                    cwd=workdir,
                    env=env,
                    capture_output=True,
                    text=True,
                    encoding="utf-8",
                    timeout=60,
                )
                if result.returncode or MARKER not in result.stdout:
                    raise RuntimeError(f"Copilot CLI failed ({result.returncode}):\n{result.stderr}")
                verify_records(home)
            finally:
                server.shutdown()
                worker.join()


if __name__ == "__main__":
    main(sys.argv[1:])
