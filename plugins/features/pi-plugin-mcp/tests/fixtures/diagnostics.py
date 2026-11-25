"""Local stdio fixture. Protocol stdout and diagnostic stderr are deliberately separate."""
import json
import os
import sys

mode = sys.argv[1]
secret = os.environ.get("FIXTURE_SECRET", "fixture-secret")


def diagnostic():
    # Exceed pipe capacity and retained-tail capacity before sending an actionable ending.
    sys.stderr.write("x" * (1024 * 1024))
    sys.stderr.write("\nconfiguration unavailable\ncredential=" + secret + "\nlast diagnostic line\n")
    sys.stderr.flush()


def send(message):
    print(json.dumps(message), flush=True)


if mode == "boot":
    diagnostic()
    sys.exit(1)

for line in sys.stdin:
    request = json.loads(line)
    method = request.get("method")
    identifier = request.get("id")
    if identifier is None:
        continue
    if method == "server/discover":
        send({"jsonrpc": "2.0", "id": identifier, "error": {"code": -32601, "message": "unsupported"}})
        continue
    if method == "initialize":
        result = {"protocolVersion": "2025-11-25", "capabilities": {"tools": {}, "logging": {}},
                  "serverInfo": {"name": "diagnostics-fixture", "version": "1"}}
    elif method == "tools/list":
        if mode == "discovery":
            diagnostic()
            sys.exit(3)
        send({"jsonrpc": "2.0", "method": "notifications/message", "params": {
            "level": "warning", "logger": "fixture", "data": "first line\ncredential=" + secret}})
        result = {"tools": [{"name": "close", "description": "Close the fixture", "inputSchema": {"type": "object"}}]}
    elif method == "tools/call":
        diagnostic()
        sys.exit(2)
    else:
        result = {}
    send({"jsonrpc": "2.0", "id": identifier, "result": result})
