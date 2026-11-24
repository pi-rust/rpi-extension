#!/usr/bin/env python3
"""Required-artifact E2E: real CLI loads the DLL, whose tool controls a real Host.
No installed/PATH fallback. Only fixture-owned processes and temporary files.
"""
import argparse
import hashlib
import http.server
import json
from pathlib import Path
import selectors
import signal
import subprocess
import tempfile
import threading
import uuid


def identity():
    return str(uuid.uuid4())


def private_json(path, value):
    path.write_text(json.dumps(value))
    path.chmod(0o600)


def completion(delta, finish="stop", model="manager"):
    chunks = [
        {"id": "fixture", "object": "chat.completion.chunk", "created": 1,
         "model": model, "choices": [{"index": 0, "delta": delta, "finish_reason": None}]},
        {"id": "fixture", "object": "chat.completion.chunk", "created": 1,
         "model": model, "choices": [{"index": 0, "delta": {}, "finish_reason": finish}],
         "usage": {"prompt_tokens": 1, "completion_tokens": 1, "total_tokens": 2}},
    ]
    return ("".join("data: " + json.dumps(c) + "\n\n" for c in chunks) + "data: [DONE]\n\n").encode()


def run(binary, plugin, administrative=False):
    assert binary.is_absolute() and binary.is_file(), "explicit binary required"
    assert plugin.is_absolute() and plugin.is_file(), "explicit DLL required"
    with tempfile.TemporaryDirectory(prefix="hm", dir="/private/tmp" if Path("/private/tmp").is_dir() else None) as temporary:
        root = Path(temporary)
        root.chmod(0o700)
        workspace = root / "workspace"
        workspace.mkdir()
        home = root / "home"
        home.mkdir()
        agent_dir = home / "agent"
        agent_dir.mkdir()
        session = workspace / "native.jsonl"
        agent = identity()
        request_id = identity()
        commands = [
            {"op": "status"}, {"op": "list", "limit": 8}, {"op": "inspect", "agent_id": agent},
            {"op": "submit", "agent_id": agent, "request_id": request_id, "prompt": "one native execution"},
            {"op": "receipt", "agent_id": agent, "request_id": request_id},
            {"op": "submit", "agent_id": agent, "request_id": request_id, "prompt": "one native execution"},
        ]
        results = []
        issued = []
        scope = identity()
        phase = "create" if administrative else "wait"
        pending = [{"op": "status"}, {"op": "profiles", "limit": 8}] if administrative else commands.copy()

        def intent(action):
            return {"scope": scope, "request_id": identity(), "epoch": results[-1]["control"]["epoch"], "action": action}

        def next_command():
            nonlocal phase
            if not pending:
                if phase == "create":
                    reference = results[-1]["data"]["profiles"][0]
                    assert results[-1]["data"]["scope"] == scope
                    request = intent({"op": "create_profile", "profile": reference})
                    pending.extend([{"op": "administrative_create", "request": request},
                                    {"op": "administrative_receipt", "request": request}, *commands[1:]])
                    phase = "wait"
                elif phase == "wait":
                    if not results[-1]["data"].get("execution", {}).get("finished", False):
                        assert len(results) < 60, "native receipt never finished"
                        return {"op": "receipt", "agent_id": agent, "request_id": request_id}
                    if administrative:
                        pending.append({"op": "inspect", "agent_id": agent})
                        phase = "cancel"
                    else:
                        return None
                elif phase in ("cancel", "close", "restore"):
                    if phase == "cancel":
                        # Completed-input cancellation must be a no-op, not a
                        # signal to a later run. Active-run cancellation is a
                        # separate native/core gate, not claimed by this case.
                        action = {"op": "cancel_submitted", "agent_id": agent, "revision": results[-1]["data"]["revision"], "submitted_request_id": request_id}
                        operation = "administrative_cancel"
                        phase = "close"
                    elif phase == "close":
                        action = {"op": "close", "agent_id": agent, "revision": results[-1]["data"]["revision"]}
                        operation = "administrative_close"
                        phase = "restore"
                    else:
                        reference = results[1]["data"]["profiles"][0]
                        action = {"op": "restore_profile", "profile": reference, "revision": results[-1]["data"]["revision"]}
                        operation = "administrative_restore"
                        phase = "stop"
                    request = intent(action)
                    pending.extend([{"op": operation, "request": request}, {"op": "administrative_receipt", "request": request}, {"op": "inspect", "agent_id": agent}])
                elif phase == "stop":
                    pending.append({"op": "administrative_stop", "request": intent({"op": "stop"})})
                    phase = "done"
                else:
                    return None
            return pending.pop(0)

        errors = []
        native_calls = []
        manager_calls = []
        done = threading.Event()

        class Provider(http.server.BaseHTTPRequestHandler):
            def setup(self):
                super().setup()
                self.connection.settimeout(10)

            def log_message(self, *_):
                pass

            def do_POST(self):
                try:
                    size = int(self.headers["Content-Length"])
                    assert 0 < size <= 2 * 1024 * 1024
                    body = json.loads(self.rfile.read(size))
                    model = body["model"]
                    if model == "native":
                        assert self.headers["Authorization"] == "Bearer native-fixture-key"
                        native_calls.append(body)
                        response = completion({"role": "assistant", "content": "native-host-answer"}, model="native")
                    else:
                        assert model == "manager"
                        assert self.headers["Authorization"] == "Bearer manager-fixture-key"
                        manager_calls.append(body)
                        tools = [m for m in body["messages"] if m.get("role") == "tool"]
                        if tools:
                            raw = tools[-1]["content"]
                            if isinstance(raw, list):
                                raw = "".join(x.get("text", "") for x in raw)
                            result = json.loads(raw)
                            # The SDK tool adapter emits text content, not a second transcript.
                            if "details" in result:
                                result = result["details"]
                            assert result["error"] is False, result
                            assert result["status"] == "reply_received", result
                            assert "token" not in result["control"]
                            results.append(result)
                            original = issued[-1]
                            if original["op"].startswith("administrative_"):
                                assert result["data"]["receipt"]["request"] == original["request"]
                                assert result["data"]["receipt"]["status"]["state"] == "succeeded", result
                                if original["op"] == "administrative_cancel":
                                    assert result["data"]["receipt"]["status"]["result"]["signalled"] is False
                        step = len(results)
                        command = next_command()
                        if command is None:
                            done.set()
                            response = completion({"role": "assistant", "content": "management-smoke-complete"})
                        else:
                            issued.append(command)
                            response = completion({"role": "assistant", "tool_calls": [{"index": 0, "id": "host-" + str(step), "type": "function", "function": {"name": "host", "arguments": json.dumps({"command": command})}}]}, "tool_calls")
                    self.send_response(200)
                    self.send_header("Content-Type", "text/event-stream")
                    self.send_header("Content-Length", str(len(response)))
                    self.send_header("Connection", "close")
                    self.end_headers()
                    self.wfile.write(response)
                except Exception as error:
                    errors.append(repr(error))
                    self.send_error(500, "fixture assertion failed")

        server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Provider)
        server.daemon_threads = False  # server_close joins fixture request handlers.
        serving = threading.Thread(target=server.serve_forever)
        serving.start()
        url = "http://127.0.0.1:" + str(server.server_port) + "/v1"
        runtime_provider = {"api": "openai-completions", "baseUrl": url, "apiKey": "manager-fixture-key",
                            "models": [{"id": "manager", "contextWindow": 32768, "maxTokens": 1024}]}
        private_json(agent_dir / "models.json", {"providers": {"fixture": runtime_provider}})
        profile = root / "p.json"
        private_json(profile, {
            "agent": {"id": agent, "name": "native-fixture", "cwd": str(workspace),
                      "session": {"id": "native-fixture-session", "path": str(session)},
                      "model": {"provider": "fixture", "model": "native"}, "tools": []},
            "runtime": {"providers": {"fixture": {"api": "openai-completions", "baseUrl": url, "apiKey": "$FIXTURE_KEY",
                       "models": [{"id": "native", "contextWindow": 32768, "maxTokens": 1024}]}}},
            "providerValues": {"FIXTURE_KEY": "native-fixture-key", "NO_PROXY": "*"},
            "toolEnvironment": {"HOME": str(workspace), "PATH": "/usr/bin:/bin"},
            "shell": "/bin/sh", "approvedTools": [], "thinking": "off", "timeoutMs": 10000,
        })
        config = root / "c.json"
        token = uuid.uuid4().hex
        configuration = {"directory": str(root / "d"), "socket_path": str(root / "s"), "host_id": identity(), "token": token, "agents": [] if administrative else [str(profile)]}
        if administrative:
            configuration["administration"] = {"scope": scope, "profiles": [str(profile)], "allowed_agents": [agent], "permissions": ["create", "restore", "cancel", "close", "stop"]}
        private_json(config, configuration)
        environment = {"HOME": str(home), "PATH": "/usr/bin:/bin", "RPI_CODING_AGENT_DIR": str(agent_dir),
                       "RPI_SHARED_HOST_CONFIG": str(config), "NO_PROXY": "*", "NO_COLOR": "1", "TERM": "dumb"}
        daemon = None
        daemon_log = root / "daemon-stderr"
        try:
            with daemon_log.open("w+") as stderr:
                daemon = subprocess.Popen([str(binary), "host", "serve", "--config", str(config)],
                                          env=environment, cwd=workspace, stdin=subprocess.DEVNULL,
                                          stdout=subprocess.PIPE, stderr=stderr, text=True)
                with selectors.DefaultSelector() as selector:
                    selector.register(daemon.stdout, selectors.EVENT_READ)
                    assert selector.select(20), "Host readiness deadline"
                line = daemon.stdout.readline()
                assert line, daemon_log.read_text()
                ready = json.loads(line)
                assert ready["event"] == "host_ready"
                assert token not in line
                command = [str(binary), "--local-only", "--extension", str(plugin), "--no-context-files", "--no-skills",
                           "--no-prompt-templates", "--no-session", "--tools", "host", "--provider", "fixture", "--model", "manager",
                           "--thinking", "off", "--approve", "-p", "Exercise the explicitly configured Host using the fixture tool calls"]
                cli = subprocess.run(command, env=environment, cwd=workspace, stdin=subprocess.DEVNULL,
                                     stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True, timeout=90)
                assert cli.returncode == 0, cli.stderr + "\n" + cli.stdout
                assert not errors, errors
                assert done.is_set() and "management-smoke-complete" in cli.stdout, cli.stdout + cli.stderr
                assert len(native_calls) == 1, "duplicate submit must not repeat Provider execution"
                assert results[0]["data"]["registered"] == (0 if administrative else 1)
                paired = list(zip(issued, results))
                assert all(len(result["data"]["agents"]) == 1 for command, result in paired if command["op"] == "list")
                assert all(result["data"]["spec"]["id"] == agent for command, result in paired if command["op"] == "inspect")
                submissions = [result["data"] for command, result in paired if command["op"] == "submit"]
                assert len(submissions) == 2 and submissions[0]["durable"] is True
                assert submissions[1]["receipt"] == submissions[0]["receipt"]
                if administrative:
                    assert {c["op"] for c in issued} == {"status", "list", "inspect", "profiles", "submit", "receipt", "administrative_receipt", "administrative_create", "administrative_restore", "administrative_close", "administrative_cancel", "administrative_stop"}
                history = session.read_text()
                assert "native-host-answer" in history
                for secret in [token, "native-fixture-key", "manager-fixture-key"]:
                    assert secret not in cli.stdout + cli.stderr + history
                if not administrative:
                    daemon.send_signal(signal.SIGTERM)
                assert daemon.wait(timeout=20) == 0, daemon_log.read_text()
                assert not (root / "s").exists()
                print(json.dumps({"passed": True, "administrative": administrative, "management_tool_calls": len(results), "native_provider_calls": len(native_calls),
                                  "operations": sorted({c["op"] for c in issued}),
                                  "binary_sha256": hashlib.sha256(binary.read_bytes()).hexdigest(),
                                  "plugin_sha256": hashlib.sha256(plugin.read_bytes()).hexdigest()}))
        finally:
            if daemon is not None and daemon.poll() is None:
                daemon.terminate()
                try:
                    daemon.wait(timeout=10)
                except subprocess.TimeoutExpired:
                    daemon.kill()
                    daemon.wait(timeout=10)
            if daemon is not None and daemon.stdout:
                daemon.stdout.close()
            server.shutdown()
            serving.join()
            server.server_close()


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", required=True, type=Path)
    parser.add_argument("--plugin", required=True, type=Path)
    parser.add_argument("--administrative", action="store_true", help="also exercise create/completed-input-cancel/close/restore/stop using fixed owner grants")
    args = parser.parse_args()
    run(args.binary, args.plugin, args.administrative)
