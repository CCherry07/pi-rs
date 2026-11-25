#!/usr/bin/env python3
"""Deterministic native OAuth terminal regression. No real browser or accounts.
Run after building pi: python3 apps/pi-cli/tests/fixtures/mcp_oauth_pty.py target/debug/pi
POSIX only; standard library only. The mock opener just records its fixture URL.
"""
import http.server
import json
import os
import pathlib
import pty
import select
import subprocess
import sys
import tempfile
import termios
import threading
import time
import urllib.parse
import urllib.request


class Server(http.server.BaseHTTPRequestHandler):
    def log_message(self, *_):
        pass

    def reply(self, code, payload=None):
        data = json.dumps(payload).encode() if payload is not None else b""
        self.send_response(code)
        self.send_header("content-type", "application/json")
        self.send_header("content-length", str(len(data)))
        if code == 401:
            self.send_header("www-authenticate", "Bearer realm=fixture")
        self.end_headers()
        self.wfile.write(data)

    def do_GET(self):
        if self.path == "/metadata":
            origin = f"http://127.0.0.1:{self.server.server_port}"
            self.reply(200, {"issuer": origin, "authorization_endpoint": origin + "/authorize", "token_endpoint": origin + "/token", "code_challenge_methods_supported": ["S256"]})
        else:
            self.reply(405)

    def do_POST(self):
        body = self.rfile.read(int(self.headers.get("content-length", "0")))
        if self.path == "/token":
            assert b"code=fixture-code" in body and b"code_verifier=" in body
            self.reply(200, {"access_token": "fixture-token", "token_type": "Bearer", "expires_in": 3600})
            return
        if self.headers.get("authorization") != "Bearer fixture-token":
            self.reply(401)
            return
        request = json.loads(body)
        if "id" not in request:
            self.reply(202)
            return
        if request["method"] == "initialize":
            result = {"protocolVersion": "2025-03-26", "capabilities": {"tools": {}}, "serverInfo": {"name": "fixture", "version": "1"}}
        elif request["method"] == "tools/list":
            result = {"tools": [{"name": "whoami", "description": "Fixture identity", "inputSchema": {"type": "object"}}]}
        else:
            self.reply(200, {"jsonrpc": "2.0", "id": request["id"], "error": {"code": -32601, "message": "Use initialize"}})
            return
        self.reply(200, {"jsonrpc": "2.0", "id": request["id"], "result": result})


def exercise(binary, origin, mode):
    with tempfile.TemporaryDirectory(prefix="pi-oauth-pty-") as directory:
        root = pathlib.Path(directory)
        agent = root / "agent"
        agent.mkdir()
        (agent / "mcp.json").write_text(json.dumps({"mcpServers": {"account": {"url": origin + "/mcp", "oauth": {"clientId": "fixture", "authServerMetadataUrl": origin + "/metadata"}}}}))
        opener = root / ("open" if sys.platform == "darwin" else "xdg-open")
        opener.write_text("#!/usr/bin/env python3\nimport os,pathlib,sys\npathlib.Path(os.environ['OAUTH_URL_FILE']).write_text(sys.argv[1])\n")
        opener.chmod(0o700)
        environment = dict(os.environ, PATH=str(root) + os.pathsep + os.environ["PATH"], OAUTH_URL_FILE=str(root / "authorization-url"), TERM="xterm-256color")
        master, slave = pty.openpty()
        original = termios.tcgetattr(slave)
        piped = mode.startswith("pipe-")
        timeout = "0.5" if mode == "pipe-timeout" else "1" if mode == "timeout" else "10"
        arguments = [str(binary), "--cwd", str(root), "--agent-dir", str(agent), "--no-approve", "mcp", "login", "account", "--timeout", timeout]
        process = subprocess.Popen(arguments, stdin=subprocess.DEVNULL if piped else slave, stdout=slave, stderr=slave, env=environment)
        output = bytearray()
        acted = False
        deadline = time.monotonic() + 15
        try:
            while process.poll() is None:
                assert time.monotonic() < deadline, (mode, "process hung", bytes(output))
                ready, _, _ = select.select([master], [], [], 0.05)
                if ready:
                    output.extend(os.read(master, 65536))
                input_ready = (root / "authorization-url").exists() if piped else b"Waiting for callback;" in output and not termios.tcgetattr(slave)[3] & termios.ICANON
                if acted or not input_ready:
                    continue
                params = urllib.parse.parse_qs(urllib.parse.urlsplit((root / "authorization-url").read_text()).query)
                if "state" not in params or "redirect_uri" not in params:
                    continue
                acted = True
                callback = params["redirect_uri"][0] + "?" + urllib.parse.urlencode({"code": "fixture-code", "state": params["state"][0]})
                if mode in ("callback", "pipe-callback"):
                    with urllib.request.urlopen(callback, timeout=3) as response:
                        assert response.status == 200
                elif mode == "paste":
                    os.write(master, callback.encode() + b"\r")
                elif mode == "cancel":
                    os.write(master, b"\x03")
            while select.select([master], [], [], 0)[0]:
                output.extend(os.read(master, 65536))
            assert acted, (mode, "never reached callback/input readiness", bytes(output))
            if piped:
                assert b"Waiting for callback;" not in output, "non-TTY login started a terminal reader"
            assert termios.tcgetattr(slave) == original, (mode, "terminal was not restored")
            assert process.returncode == (0 if mode in ("callback", "paste", "pipe-callback") else 1), (mode, process.returncode, bytes(output))
            assert b"fixture-code" not in output and b"fixture-token" not in output, (mode, "callback/token leaked")
            assert not (agent / "sessions").exists()
            if mode in ("callback", "paste", "pipe-callback"):
                assert b"Signed in" in output and b"1 tools" in output, bytes(output)
                if piped:
                    # An already-authenticated non-TTY probe must not open the browser again.
                    (root / "authorization-url").unlink()
                    again = subprocess.run(arguments, stdin=subprocess.DEVNULL, capture_output=True, env=environment, timeout=5)
                    assert again.returncode == 0, again.stderr
                    assert not (root / "authorization-url").exists()
                    assert b"fixture-token" not in again.stdout + again.stderr
            elif mode == "cancel":
                assert b"cancelled" in output, bytes(output)
            else:
                assert b"timed out" in output, bytes(output)
        finally:
            if process.poll() is None:
                process.kill()
            process.wait()
            os.close(master)
            os.close(slave)
        print(f"{mode}: native input, cleanup, redaction and no-session checks passed")


def main():
    binary = pathlib.Path(sys.argv[1]).resolve()
    server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Server)
    thread = threading.Thread(target=server.serve_forever, daemon=True)
    thread.start()
    try:
        for mode in ("callback", "paste", "cancel", "timeout", "pipe-callback", "pipe-timeout"):
            exercise(binary, f"http://127.0.0.1:{server.server_port}", mode)
    finally:
        server.shutdown()
        server.server_close()
        thread.join()


if __name__ == "__main__":
    main()
