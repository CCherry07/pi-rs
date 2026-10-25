"""Real Node/NAPI + Rust terminal acceptance. Requires npm run build and build:native."""
import base64
import errno
import fcntl
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import json
import os
from pathlib import Path
import pty
import select
import shutil
import signal
import struct
import subprocess
import tempfile
import termios
import threading
import time

import pyte


package = Path(__file__).resolve().parents[1]
node = shutil.which("node")
assert node, "Node is required"
assert (package / "dist/bin/pi.js").exists(), "Run npm run build first"


class ProviderHandler(BaseHTTPRequestHandler):
    requests = []
    tool_name = "ui_tool"
    tool_arguments = {"value": "streamed"}

    def log_message(self, *_args):
        pass

    def do_POST(self):
        length = int(self.headers.get("content-length", "0"))
        ProviderHandler.requests.append(json.loads(self.rfile.read(length) or b"{}"))
        request_index = len(ProviderHandler.requests)
        time.sleep(0.45)
        self.send_response(200)
        self.send_header("content-type", "text/event-stream")
        self.end_headers()
        if request_index == 1:
            chunk = {
                "id": "chatcmpl-ui-tool",
                "object": "chat.completion.chunk",
                "created": 0,
                "model": "gpt-4o-mini",
                "choices": [{
                    "index": 0,
                    "delta": {
                        "role": "assistant",
                        "tool_calls": [{
                            "index": 0,
                            "id": "ui-call-1",
                            "type": "function",
                            "function": {"name": ProviderHandler.tool_name, "arguments": json.dumps(ProviderHandler.tool_arguments)},
                        }],
                    },
                    "finish_reason": "tool_calls",
                }],
            }
        else:
            chunk = {
                "id": "chatcmpl-ui-final",
                "object": "chat.completion.chunk",
                "created": 0,
                "model": "gpt-4o-mini",
                "choices": [{
                    "index": 0,
                    "delta": {"role": "assistant", "content": "UI_PTY_MARKDOWN_BEFORE"},
                    "finish_reason": "stop",
                }],
                "usage": {"prompt_tokens": 8, "completion_tokens": 2, "total_tokens": 10},
            }
        self.wfile.write(f"data: {json.dumps(chunk)}\n\ndata: [DONE]\n\n".encode())
        self.wfile.flush()


class TerminalHarness:
    def __init__(self, directory: Path, env: dict[str, str], extensions=None):
        self.directory = directory
        self.output = directory / "evidence.json"
        self.pid, self.master = pty.fork()
        if self.pid == 0:
            os.chdir(directory)
            fcntl.ioctl(0, termios.TIOCSWINSZ, struct.pack("HHHH", 24, 80, 0, 0))
            os.execve(
                node,
                [
                    node,
                    str(package / "dist/bin/pi.js"),
                    "--agent-dir",
                    str(directory / "agent"),
                    "--no-extensions",
                    *[argument for extension in (extensions or [package / "test/fixtures/ui-runtime.ts"])
                      for argument in ("--extension", str(extension))],
                ],
                env,
            )
        self.transcript = bytearray()
        self.screen_pending = b""
        self.screen = pyte.Screen(80, 24)
        self.stream = pyte.ByteStream(self.screen)
        self.cursor_queries = 0
        self.color_queries = 0
        self.scheme_queries = 0
        self.cell_queries = 0
        self.exited = False

    def record(self, data: bytes):
        self.transcript.extend(data)
        # pyte does not implement Kitty images. Preserve raw evidence, but consume the APC
        # control string as a real terminal does instead of painting its base64 payload as text.
        self.screen_pending += data
        while True:
            start = self.screen_pending.find(b"\x1b_G")
            if start < 0:
                keep = 2 if self.screen_pending.endswith(b"\x1b_") else 1 if self.screen_pending.endswith(b"\x1b") else 0
                visible = self.screen_pending[:-keep] if keep else self.screen_pending
                self.stream.feed(visible)
                self.screen_pending = self.screen_pending[-keep:] if keep else b""
                break
            self.stream.feed(self.screen_pending[:start])
            self.screen_pending = self.screen_pending[start:]
            end = self.screen_pending.find(b"\x1b\\", 3)
            if end < 0:
                break
            self.screen_pending = self.screen_pending[end + 2:]
        cursor_queries = self.transcript.count(b"\x1b[6n")
        if cursor_queries > self.cursor_queries:
            os.write(self.master, b"\x1b[1;1R" * (cursor_queries - self.cursor_queries))
            self.cursor_queries = cursor_queries
        color_queries = self.transcript.count(b"\x1b]10;?\x07")
        while self.color_queries < color_queries:
            # Match the light appearance reported by the scheme notification below.
            replies = [
                b"\x1b]10;rgb:0000/0000/0000\x07",
                b"\x1b]11;rgb:ffff/ffff/ffff\x07",
            ]
            replies.extend(
                f"\x1b]4;{index};rgb:{index:02x}{index:02x}/{index:02x}{index:02x}/{index:02x}{index:02x}\x07".encode()
                for index in range(16)
            )
            replies.append(b"\x1b[?1;2c")
            os.write(self.master, b"".join(replies))
            self.color_queries += 1
        scheme_queries = self.transcript.count(b"\x1b[?2031h")
        if scheme_queries > self.scheme_queries:
            os.write(self.master, b"\x1b[?997;2n" * (scheme_queries - self.scheme_queries))
            self.scheme_queries = scheme_queries
        cell_queries = self.transcript.count(b"\x1b[16t")
        if cell_queries > self.cell_queries:
            os.write(self.master, b"\x1b[6;20;10t" * (cell_queries - self.cell_queries))
            self.cell_queries = cell_queries

    def screen_text(self):
        return "\n".join(
            "".join(self.screen.buffer[y][x].data or " " for x in range(self.screen.columns))
            for y in range(self.screen.lines)
        )

    def until(self, marker: bytes, timeout=30):
        deadline = time.monotonic() + timeout
        while marker not in self.transcript and marker.decode(errors="replace") not in self.screen_text():
            assert time.monotonic() < deadline, (
                f"Timeout waiting for {marker!r}; "
                f"evidence={self.output.read_text() if self.output.exists() else 'none'}: "
                f"screen={self.screen_text()!r}"
            )
            if not select.select([self.master], [], [], 0.1)[0]:
                continue
            try:
                data = os.read(self.master, 65536)
            except OSError as error:
                if error.errno != errno.EIO:
                    raise
                data = b""
            assert data, f"Terminal closed before {marker!r}: {bytes(self.transcript[-6000:])!r}"
            self.record(data)

    def send(self, data: bytes):
        os.write(self.master, data)

    def point(self, label: str, offset=0):
        for row, line in enumerate(self.screen_text().splitlines()):
            column = line.find(label)
            if column >= 0:
                return column + offset, row
        raise AssertionError(f"Mouse target {label!r} is not visible: {self.screen_text()}")

    @staticmethod
    def mouse_packet(code: int, point, release=False):
        column, row = point
        return f"\x1b[<{code};{column + 1};{row + 1}{'m' if release else 'M'}".encode()

    def click(self, label: str, offset=0, after=b""):
        point = self.point(label, offset)
        self.send(self.mouse_packet(0, point) + self.mouse_packet(0, point, True) + after)

    def until_evidence(self, predicate, timeout=30):
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            if self.output.exists():
                try:
                    evidence = json.loads(self.output.read_text())
                    if predicate(evidence):
                        return evidence
                except json.JSONDecodeError:
                    pass
            if select.select([self.master], [], [], 0.05)[0]:
                try:
                    self.record(os.read(self.master, 65536))
                except OSError as error:
                    if error.errno != errno.EIO:
                        raise
        raise AssertionError(f"Timed out waiting for evidence: {self.output.read_text() if self.output.exists() else 'none'}")

    def resize(self, lines: int, columns: int):
        fcntl.ioctl(self.master, termios.TIOCSWINSZ, struct.pack("HHHH", lines, columns, 0, 0))
        self.screen.resize(lines, columns)
        os.kill(self.pid, signal.SIGWINCH)
        deadline = time.monotonic() + 0.25
        while time.monotonic() < deadline:
            if select.select([self.master], [], [], 0.02)[0]:
                self.record(os.read(self.master, 65536))

    def finish(self, timeout=20):
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            completed, status = os.waitpid(self.pid, os.WNOHANG)
            if completed:
                self.exited = True
                assert os.waitstatus_to_exitcode(status) == 0, bytes(self.transcript[-6000:])
                return
            if select.select([self.master], [], [], 0.05)[0]:
                try:
                    self.record(os.read(self.master, 65536))
                except OSError as error:
                    if error.errno != errno.EIO:
                        raise
        raise AssertionError("CLI did not shut down after the extension completed")

    def close(self):
        if not self.exited:
            os.kill(self.pid, signal.SIGKILL)
        # Release the PTY before waiting: a writer can still be blocked on its master.
        os.close(self.master)
        if not self.exited:
            os.waitpid(self.pid, 0)


with tempfile.TemporaryDirectory(prefix="pi-ui-pty-") as temporary:
    directory = Path(temporary)
    agent = directory / "agent"
    agent.mkdir(parents=True)
    editor_marker = directory / "editor-invoked"
    editor = directory / "external-editor"
    editor.write_text(
        "#!/bin/sh\n"
        "printf 'UI_PTY_EXTERNAL_EDITED' > \"$1\"\n"
        "printf invoked > \"$UI_ACCEPTANCE_EDITOR_MARKER\"\n"
    )
    editor.chmod(0o755)
    (agent / "settings.json").write_text(json.dumps({
        "theme": "system",
        "externalEditor": str(editor),
        "outputPad": 0,
        "terminal": {"showImages": True, "imageWidthCells": 7},
        "retry": {"enabled": False},
    }))
    subprocess.run(["git", "init", "-q"], cwd=directory, check=True)
    subprocess.run(["git", "symbolic-ref", "HEAD", "refs/heads/ui-branch"], cwd=directory, check=True)

    ProviderHandler.requests = []
    server = ThreadingHTTPServer(("127.0.0.1", 0), ProviderHandler)
    server_thread = threading.Thread(target=server.serve_forever, daemon=True)
    server_thread.start()
    (agent / "models.json").write_text(json.dumps({"providers": {"openai": {
        "baseUrl": f"http://127.0.0.1:{server.server_address[1]}/v1",
        "apiKey": "ui-fixture-key", "api": "openai-completions",
        "models": [{"id": "gpt-4o-mini", "name": "Fixture", "contextWindow": 200000, "maxTokens": 8192}],
    }}}))
    env = {
        key: os.environ[key]
        for key in ("PATH", "TMPDIR", "PI_RS_NATIVE_BINDING")
        if key in os.environ
    }
    env.update({
        "HOME": str(directory),
        "USERPROFILE": str(directory),
        "XDG_CONFIG_HOME": str(directory / "config"),
        "TERM": "xterm-256color",
        "TERM_PROGRAM": "kitty",
        "COLORTERM": "truecolor",
        "LANG": "en_US.UTF-8",
        "PI_OFFLINE": "1",
        "PI_IMAGE_PROTOCOL": "kitty",
        "PI_HYPERLINKS": "1",
        "PI_TRUE_COLOR": "1",
        "SSH_CONNECTION": "127.0.0.1 1 127.0.0.1 2",
        "PI_TUI_ESC_TIMEOUT": "20",
        "OPENAI_API_KEY": "ui-fixture-key",
        "OPENAI_MODEL": "gpt-4o-mini",
        "OPENAI_BASE_URL": f"http://127.0.0.1:{server.server_address[1]}/v1",
        "UI_ACCEPTANCE_OUTPUT": str(directory / "evidence.json"),
        "UI_ACCEPTANCE_EDITOR_MARKER": str(editor_marker),
        "UI_ACCEPTANCE_TOOL_CONTINUE": str(directory / "continue-tool"),
    })
    terminal = TerminalHarness(directory, env)
    try:
        terminal.until(b"UI_PTY_STARTUP")
        # Native decoder must ignore key releases and preserve Unicode bracketed paste.
        terminal.send(b"\x1b[97;5:3u")
        terminal.send("\x1b[200~你好，Pi\x1b[201~\r".encode())
        terminal.until(b"UI_PTY_MAIN_READY")
        terminal.until(b"UI_PTY_HEADER")
        terminal.until(b"UI_PTY_FOOTER")
        terminal.until(b"UI_PTY_LINK")
        terminal.until(b"UI_PTY_STRING_WIDGET")
        terminal.until_evidence(lambda evidence: evidence.get("footerBranch") == "ui-branch")
        subprocess.run(["git", "symbolic-ref", "HEAD", "refs/heads/ui-second"], cwd=directory, check=True)
        terminal.until_evidence(lambda evidence: evidence.get("footerBranch") == "ui-second")
        terminal.click("UI_PTY_MAIN_READY")
        terminal.until_evidence(lambda evidence: evidence.get("widgetMouse") == 1)

        terminal.send(b"/ui-smoke\r")
        terminal.until(b"UI_PTY_SELECT")
        terminal.send(b"\r")
        terminal.until(b"UI_PTY_CONFIRM")
        terminal.send(b"\r")
        terminal.until(b"UI_PTY_INPUT")
        terminal.send("\x1b[200~typed input\x1b[201~\r".encode())
        terminal.until(b"UI_PTY_EDITOR")
        # Editor uses native multiline editing; Ctrl+Enter submits.
        terminal.send(b"\x1b[13;5u")
        terminal.until(b"UI_PTY_EXTERNAL_DONE")
        assert editor_marker.exists(), "configured external editor was not launched by Rust"
        terminal.until(b"UI_PTY_OVERLAY")
        terminal.resize(20, 60)
        # An unrecognized terminal report next to input must not consume the input.
        terminal.send("\x1b[6;20;10t\x1b[200~覆盖成功\x1b[201~\r".encode())
        terminal.until(b"UI_PTY_INTERACTION_DONE")

        terminal.send(b"\x0b")
        terminal.until(b"UI_PTY_SHORTCUT")
        terminal.send(b"/ui-update\r")
        terminal.until(b"UI_PTY_UPDATE_AFTER")
        terminal.send(b"\r")
        terminal.until(b"UI_PTY_UPDATE_DONE")
        terminal.send(b"/ui-complete\r")
        terminal.until(b"UI_PTY_COMPLETE")
        terminal.send(b"\t\r")
        terminal.until(b"UI_PTY_COMPLETE_DONE")

        terminal.send(b"/ui-custom\r")
        terminal.until(b"UI_PTY_CUSTOM")
        terminal.resize(20, 48)
        terminal.until_evidence(lambda evidence: 48 in evidence.get("customWidths", []))
        terminal.send("\x1b[200~你好\x1b[201~\x1b[D啊\r".encode())
        terminal.until(b"UI_PTY_CUSTOM_DONE")

        terminal.resize(24, 80)
        terminal.send(b"/ui-overlay-controls\r")

        def overlay_state(row, col, width, focused):
            def matches(evidence):
                state = evidence.get("overlayState", {})
                return (
                    state.get("bounds") == {"row": row, "col": col, "width": width, "height": 2}
                    and state.get("focused") is focused and state.get("inputFocused") is focused
                    and terminal.screen_text().splitlines()[row][col:].startswith("UI_PTY_CONTROLLED")
                )
            return matches

        terminal.until_evidence(overlay_state(21, 38, 40, False))
        terminal.send(b"\x0c")  # Ctrl+L focuses the passive overlay.
        terminal.until_evidence(overlay_state(21, 38, 40, True))
        terminal.send("\x1b[200~保留\x1b[201~".encode())
        terminal.until_evidence(lambda evidence: evidence.get("overlayState", {}).get("value") == "保留")
        terminal.send(b"\x19")  # Ctrl+Y returns input to the composer.
        terminal.until_evidence(overlay_state(21, 38, 40, False))
        terminal.send(b"composer draft\x07")  # Ctrl+G temporarily hides the overlay.
        terminal.until_evidence(lambda evidence: evidence.get("overlayHidden") and "UI_PTY_CONTROLLED" not in terminal.screen_text())
        terminal.send(b"\x07")
        terminal.until_evidence(overlay_state(21, 38, 40, False))
        terminal.until_evidence(lambda evidence: evidence.get("overlayEditor") == "composer draft")
        terminal.resize(18, 50)  # visible(width) temporarily excludes the overlay.
        terminal.until_evidence(lambda _evidence: "UI_PTY_CONTROLLED" not in terminal.screen_text())
        terminal.resize(24, 80)
        terminal.until_evidence(overlay_state(21, 38, 40, False))
        terminal.send(b"\x0f")  # Ctrl+O changes the dynamic options function.
        terminal.until_evidence(overlay_state(1, 2, 40, False))
        terminal.resize(26, 100)
        terminal.until_evidence(overlay_state(1, 2, 50, False))
        terminal.send(b"\x0c")
        terminal.until_evidence(overlay_state(1, 2, 50, True))
        terminal.send(b"\r")
        terminal.until(b"UI_PTY_CONTROLLED_DONE")

        terminal.send(b"/ui-mouse\r")
        terminal.until(b"UI_PTY_MOUSE")
        terminal.until(b"Mouse mode")
        terminal.send(terminal.mouse_packet(65, terminal.point("Mouse mode")))
        terminal.until_evidence(lambda value: "wheel" in value.get("mouseEvents", []))
        terminal.click("Mouse mode")
        terminal.until_evidence(lambda value: value.get("mouseSetting") == "two")
        drag = terminal.point("UI_PTY_DRAG")
        terminal.send(terminal.mouse_packet(0, drag) + terminal.mouse_packet(32, (0, 0)) + terminal.mouse_packet(0, (0, 0), True))
        terminal.until_evidence(lambda value: (
            value.get("mouseDrag", {}).get("type") == "release"
            and value["mouseDrag"]["x"] < 0 and value["mouseDrag"]["y"] < 0
        ))
        terminal.click("abcd", 1, b"X\r")
        terminal.until(b"UI_PTY_MOUSE_DONE")
        terminal.until_evidence(lambda value: value.get("mouseDraft") == "aXbcd")

        terminal.send(b"/ui-working\r")
        terminal.until(b"UI_PTY_WORKING")
        terminal.until(b"UI_PTY_TOOL_CALL")
        terminal.until(b"UI_PTY_TOOL_PARTIAL")
        Path(env["UI_ACCEPTANCE_TOOL_CONTINUE"]).touch()
        terminal.until(b"UI_PTY_TOOL_RESULT")
        terminal.until(b"UI_PTY_MARKDOWN_AFTER")
        terminal.send(b"/ui-finish\r")
        terminal.finish()
        evidence = json.loads(terminal.output.read_text())
        assert evidence["startup"] == "你好，Pi", evidence
        assert evidence["selection"] == "alpha" and evidence["confirmed"] is True, evidence
        assert evidence["input"] == "typed input", evidence
        assert evidence["editor"] == "before external editor", evidence
        assert evidence["externalEditor"] == "UI_PTY_EXTERNAL_EDITED", evidence
        assert evidence["overlay"] == "覆盖成功" and evidence["interactionDone"], evidence
        assert evidence["messageDetails"] == {"retained": 42} and evidence["messageOutputPad"] == 0, evidence
        assert evidence["entryData"] == {"retained": 43} and evidence["entryId"], evidence
        assert evidence["shortcut"] and evidence["activated"], evidence
        assert evidence["completion"] == "hello from Rust", evidence
        assert evidence["custom"] == "你啊好" and evidence["customDisposals"] == 1, evidence
        assert 60 in evidence["customWidths"] and 48 in evidence["customWidths"], evidence
        assert evidence["slotFactories"] == {"header": 1, "footer": 1, "widget": 1}, evidence
        assert evidence["slotDisposals"] == {"widget": 1, "header": 1, "footer": 1}, evidence
        assert evidence["branchChanges"] >= 1 and evidence["footerProviderCount"] >= 1, evidence
        assert ["acceptance", "ready"] in evidence["footerStatuses"], evidence
        assert 80 in evidence["widgetWidths"] and 60 in evidence["widgetWidths"], evidence
        assert evidence["controlled"] == "保留" and evidence["overlayFactories"] == 1 and evidence["overlayDisposals"] == 1, evidence
        assert 40 in evidence["overlayWidths"] and 50 in evidence["overlayWidths"], evidence
        assert evidence["toolResolver"] and evidence["toolCall"]["argsComplete"], evidence
        assert evidence["toolCall"]["executionStarted"] and evidence["toolCall"]["outputPad"] == 0, evidence
        assert evidence["toolCall"]["showImages"] is True and evidence["toolCall"]["args"] == {"value": "streamed"}, evidence
        assert any(item["isPartial"] and item["details"]["phase"] == "partial" for item in evidence["toolResults"]), evidence
        final_tool = next(item for item in reversed(evidence["toolResults"]) if not item["isPartial"])
        assert final_tool["details"]["phase"] == "complete" and final_tool["sameArgs"], evidence
        assert isinstance(final_tool["durationMs"], (int, float)) and final_tool["durationMs"] > 0, evidence
        assert len(ProviderHandler.requests) == 2 and evidence["workingRequested"] and evidence["finished"], evidence
        raw = bytes(terminal.transcript)
        assert b"UI_PTY_TITLE" in raw
        assert b"\x1b]9;4;3;0\x07" in raw and b"\x1b]9;4;0;0\x07" in raw
        assert b"\x1b]7501;state=working:app=pi" in raw and b"\x1b]7501;state=clear\x1b\\" in raw
        assert b"\x1b]8;;https://example.com/pi-ui\x1b\\" in raw
        assert b"\x1b_Ga=T,f=100,q=2,C=1,c=2,r=1,i=" in raw
        clipboard = base64.b64encode(b"UI_PTY_CLIPBOARD")
        assert b"\x1b]52;c;" + clipboard + b"\x07" in raw
        print("PASS: Node/NAPI + Rust UI, Pi slot factories/footer data, mouse/capture/SettingsList, dynamic overlays/focus/visibility, input, completion, images and host operations")

        installed_tasks = os.environ.get("UI_ACCEPTANCE_TASKS_EXTENSION")
        if installed_tasks:
            task_directory = directory / "original-tasks"
            (task_directory / "agent").mkdir(parents=True)
            (task_directory / "agent/settings.json").write_text(json.dumps({"retry": {"enabled": False}}))
            task_env = {**env, "UI_ACCEPTANCE_OUTPUT": str(task_directory / "evidence.json")}
            ProviderHandler.requests = []
            ProviderHandler.tool_name = "TaskCreate"
            ProviderHandler.tool_arguments = {"subject": "UI_PTY_ORIGINAL_TASK", "description": "Verify the original package widget factory"}
            original = TerminalHarness(task_directory, task_env, [package / "test/fixtures/ui-tasks.ts", Path(installed_tasks).resolve()])
            try:
                original.until(b"UI_PTY_TASKS_READY")
                original.until(b"Ask pi")
                original.send(b"/ui-create-task\r")
                original.until(b"1 tasks (1 open)")
                original.until(b"UI_PTY_ORIGINAL_TASK")
                original.until(b"UI_PTY_MARKDOWN_BEFORE")
                result = original.until_evidence(lambda value: value.get("toolName") == "TaskCreate")
                assert not result.get("isError"), result
                assert "1 tasks (1 open)" in original.screen_text(), original.screen_text()
                original.send(b"/tasks\r")
                original.until(b"View all tasks (1)")
                original.send(b"\x1b[B" * 3 + b"\r")  # View, Create, Clear all, Settings.
                original.until(b"Task Settings")
                original.until(b"Task storage")
                original.click("Collapse completed tasks")
                tasks_config = task_directory / ".pi/tasks-config.json"
                original.until_evidence(lambda _: (
                    tasks_config.exists()
                    and json.loads(tasks_config.read_text()).get("collapseCompleted") is True
                    and any("Collapse completed tasks" in line and "on" in line for line in original.screen_text().splitlines())
                ))
                original.send(b"\x1b")
                original.until_evidence(lambda _: "View all tasks (1)" in original.screen_text())
                original.send(b"\x1b")
                original.until_evidence(lambda _: "View all tasks (1)" not in original.screen_text())
                original.send(b"/ui-finish\r")
                original.finish()
                print("PASS: original installed pi-tasks TaskCreate, widget, and /tasks Settings mouse edit/return")
            finally:
                if artifact_dir := os.environ.get("PI_UI_EVIDENCE_DIR"):
                    artifacts = Path(artifact_dir) / "original-tasks"
                    artifacts.mkdir(parents=True, exist_ok=True)
                    (artifacts / "terminal.raw").write_bytes(original.transcript)
                    (artifacts / "screen.txt").write_text(original.screen_text())
                    if original.output.exists():
                        shutil.copyfile(original.output, artifacts / "evidence.json")
                original.close()
    finally:
        if path := os.environ.get("PI_UI_EVIDENCE_DIR"):
            artifacts = Path(path)
            artifacts.mkdir(parents=True, exist_ok=True)
            (artifacts / "terminal.raw").write_bytes(terminal.transcript)
            (artifacts / "screen.txt").write_text(terminal.screen_text())
            if terminal.output.exists():
                shutil.copyfile(terminal.output, artifacts / "evidence.json")
        terminal.close()
        server.shutdown()
        server.server_close()
        server_thread.join(timeout=5)
