#!/usr/bin/env python3
"""Real DLL/native error regression; no benchmark and no installed binary fallback.

Status/rejection use a real empty Host. Transport failures and local abort
use an authenticated fixture peer.
"""
import argparse
import hashlib
import http.server
import json
from pathlib import Path
import socket
import struct
import tempfile
import threading

# Reuse only lifecycle/framing helpers; never call its 20-Agent runner/fixture.
from compare_20_memory import Rpc, completion, private_json, uid


def frame_read(stream):
    def exact(count):
        result = b""
        while len(result) < count:
            chunk = stream.recv(count - len(result))
            assert chunk, "fixture EOF"
            result += chunk
        return result
    length = struct.unpack("!I", exact(4))[0]
    assert 0 < length <= 1024 * 1024
    return json.loads(exact(length))


def frame_write(stream, value):
    data = json.dumps(value).encode()
    assert len(data) <= 1024 * 1024
    stream.sendall(struct.pack("!I", len(data)) + data)


class Peer:
    def __init__(self, config, command, mode):
        self.config, self.command, self.mode = config, command, mode
        self.requests = []
        self.request_received = threading.Event()
        self.connection_closed = threading.Event()
        self.errors = []
        self.listener = socket.socket(socket.AF_UNIX)
        self.listener.bind(config["socket_path"])
        Path(config["socket_path"]).chmod(0o600)
        self.listener.listen(4)
        self.listener.settimeout(10)
        self.thread = threading.Thread(target=self.serve)
        self.thread.start()

    def serve(self):
        try:
            stream, _ = self.listener.accept()
            with stream:
                stream.settimeout(10)
                hello = frame_read(stream)
                assert hello["token"] == self.config["token"]
                assert hello["host_id"] == self.config["host_id"]
                epoch = self.command.get("request", {}).get("epoch", uid())
                capabilities = ["status", "submit", "administrative_close"]
                if self.mode == "result_closed":
                    capabilities.append("results")
                frame_write(stream, {"protocol": "rpi-shared-host", "version": {"major": 1, "minor": 5 if self.mode == "result_closed" else 4},
                    "host_id": self.config["host_id"], "epoch": epoch,
                    "capabilities": capabilities,
                    "limits": {"max_frame_bytes": 1048576, "max_in_flight": 64, "max_page_size": 128}})
                if self.mode == "unsupported_results":
                    assert stream.recv(1) == b"", "unsupported result must never be sent"
                    return
                request = frame_read(stream)
                assert request["command"] == self.command
                self.requests.append(request)
                self.request_received.set()
                if self.mode == "abort":
                    assert stream.recv(1) == b"", "abort must close the local connection"
                    self.connection_closed.set()
                    return
                if self.mode in {"closed", "result_closed"}:
                    return
                if self.mode == "large":
                    data = {"payload": "x" * (1024 * 1024 // 2)}
                else:
                    assert self.mode == "uncertain"
                    data = {"receipt": {"request": self.command["request"], "status": {"state": "uncertain"}}}
                frame_write(stream, {"call_id": request["call_id"], "epoch": epoch,
                                     "reply": {"kind": "data", "data": data}})
        except Exception as error:
            self.errors.append(repr(error))

    def close(self):
        self.thread.join(timeout=12)
        try:
            assert not self.thread.is_alive(), "peer join deadline"
            assert not self.errors, self.errors
            assert len(self.requests) == (0 if self.mode == "unsupported_results" else 1)
            self.listener.settimeout(0.1)
            try:
                stream, _ = self.listener.accept()
            except socket.timeout:
                pass
            else:
                stream.close()
                raise AssertionError("unexpected retry connection")
        finally:
            self.listener.close()


def run_case(binary, plugin, case):
    with tempfile.TemporaryDirectory(prefix="he", dir="/private/tmp") as temporary:
        root = Path(temporary)
        root.chmod(0o700)
        workspace = root / "w"
        workspace.mkdir()
        home = root / "h"
        home.mkdir()
        agents = home / "agent"
        agents.mkdir()
        command = case["command"]
        errors, results = [], []

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
                    assert body["model"] == "manager"
                    assert self.headers["Authorization"] == "Bearer error-fixture-key"
                    tools = [m for m in body["messages"] if m.get("role") == "tool"]
                    if tools:
                        raw = tools[-1]["content"]
                        if isinstance(raw, list):
                            raw = "".join(block.get("text", "") for block in raw)
                        details = json.loads(raw)
                        assert details["status"] == case["status"], details
                        assert details["error"] == case["error"], details
                        results.append(details)
                        response = completion({"role": "assistant", "content": "manager-complete"}, model="manager")
                    else:
                        response = completion({"role": "assistant", "tool_calls": [{"index": 0,
                            "id": "host-error-probe", "type": "function", "function": {
                                "name": "host", "arguments": json.dumps({"command": command})}}]}, "tool_calls", "manager")
                    self.send_response(200)
                    self.send_header("Content-Type", "text/event-stream")
                    self.send_header("Content-Length", str(len(response)))
                    self.send_header("Connection", "close")
                    self.end_headers()
                    self.wfile.write(response)
                except Exception as error:
                    errors.append(repr(error))
                    self.send_error(500, "error fixture assertion failed")

        server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Provider)
        server.daemon_threads = False
        serving = threading.Thread(target=server.serve_forever)
        serving.start()
        config = {"directory": str(root / "d"), "socket_path": str(root / "s"),
                  "host_id": uid(), "token": uid().replace("-", ""), "agents": []}
        config_path = root / "c.json"
        private_json(config_path, config)
        private_json(agents / "models.json", {"providers": {"fixture": {
            "api": "openai-completions", "baseUrl": "http://127.0.0.1:" + str(server.server_port) + "/v1",
            "apiKey": "error-fixture-key", "models": [{"id": "manager", "contextWindow": 32768, "maxTokens": 1024}]}}})
        environment = {"HOME": str(home), "PATH": "/usr/bin:/bin", "RPI_CODING_AGENT_DIR": str(agents),
                       "NO_PROXY": "*", "NO_COLOR": "1", "TERM": "dumb"}
        if case["peer"] != "missing_config":
            environment["RPI_SHARED_HOST_CONFIG"] = str(config_path)
        host = cli = peer = None
        try:
            if case["peer"] == "native":
                host = Rpc([str(binary), "host", "serve", "--config", str(config_path)],
                           environment, workspace, root / "host.log")
            elif case["peer"] != "missing_config":
                peer = Peer(config, command, case["peer"])
            cli = Rpc([str(binary), "--mode", "rpc", "--local-only", "--extension", str(plugin),
                       "--no-context-files", "--no-skills", "--no-prompt-templates", "--no-session",
                       "--tools", "host", "--provider", "fixture", "--model", "manager", "--thinking", "off", "--approve"],
                      environment, workspace, root / "cli.log")
            cli.send({"id": "run", "type": "prompt", "message": "Exercise the explicit Host fixture command"})
            if case["peer"] == "abort":
                assert peer.request_received.wait(10), ("request deadline", peer.errors)
                cli.send({"id": "abort-run", "type": "abort"})
                event = cli.wait(lambda e: e.get("type") == "tool_execution_end"
                                 and e.get("toolCallId") == "host-error-probe", timeout=10)
                cli.wait(lambda e: e.get("type") == "agent_end", timeout=10)
                assert event["toolName"] == "host" and event["isError"] is True, event
                assert event["result"]["details"] is None, event
                assert any("plugin tool cancelled" in block.get("text", "")
                           for block in event["result"]["content"]), event
                assert peer.connection_closed.wait(5), ("abort did not close the connection", peer.errors)
                assert config["token"] not in json.dumps(event)
                assert not errors and not results, (errors, results)
                return {"name": case["name"], "peer": case["peer"], "passed": True,
                        "status": "local_abort", "native_event": event}
            cli.done()
            assert not errors, errors
            assert len(results) == 1
            ends = [e for e in cli.events if e.get("type") == "tool_execution_end"]
            assert len(ends) == 1, ends
            event = ends[0]
            assert event["isError"] == case["error"], event
            assert event["result"]["details"] == results[0], event
            details = results[0]
            if case["status"] == "outcome_unknown":
                assert details["request_id"] == command["request_id"]
                assert details["reconcile"]["command"] == {"op": "receipt", "agent_id": command["agent_id"], "request_id": command["request_id"]}
                assert command["prompt"] not in json.dumps(details)
            if case["status"] == "native_not_succeeded":
                assert details["reconcile"]["command"] == {"op": "administrative_receipt", "request": command["request"]}
                assert details["data"]["receipt"]["status"]["state"] == "uncertain"
            if case["peer"] == "large":
                assert "budget" in details["message"] and "data" not in details
            assert config["token"] not in json.dumps(event)
            return {"name": case["name"], "peer": case["peer"], "passed": True,
                    "status": details["status"], "native_event": event}
        finally:
            # Always release every fixture resource even if another teardown fails.
            failures = []
            for process in (cli, host):
                if process is not None:
                    try:
                        process.close()
                    except Exception as error:
                        failures.append(repr(error))
            if peer is not None:
                try:
                    peer.close()
                except Exception as error:
                    failures.append(repr(error))
            server.shutdown()
            serving.join(timeout=5)
            server.server_close()
            assert not serving.is_alive()
            assert not failures, failures


def run(binary, plugin):
    assert binary.is_absolute() and binary.is_file()
    assert plugin.is_absolute() and plugin.is_file()
    submit = {"op": "submit", "agent_id": uid(), "request_id": uid(), "prompt": "private-unknown-fixture-prompt"}
    close = {"op": "administrative_close", "request": {"scope": uid(), "request_id": uid(), "epoch": uid(),
             "action": {"op": "close", "agent_id": uid(), "revision": 7}}}
    result_query = {"op": "result", "agent_id": uid(), "request_id": uid(), "run_id": None, "offset": 0, "max_bytes": 4096}
    cases = [
        {"name": "native_status_success", "peer": "native", "command": {"op": "status"}, "status": "reply_received", "error": False},
        {"name": "native_agent_not_found", "peer": "native", "command": submit, "status": "rejected", "error": True},
        {"name": "local_missing_config", "peer": "missing_config", "command": {"op": "status"}, "status": "not_sent", "error": True},
        {"name": "query_disconnected", "peer": "closed", "command": {"op": "status"}, "status": "query_failed", "error": True},
        {"name": "mutation_reply_lost", "peer": "closed", "command": submit, "status": "outcome_unknown", "error": True},
        {"name": "rpc_abort_after_submit", "peer": "abort", "command": submit},
        {"name": "native_receipt_uncertain_projection", "peer": "uncertain", "command": close, "status": "native_not_succeeded", "error": True},
        {"name": "query_envelope_overflow", "peer": "large", "command": {"op": "status"}, "status": "query_failed", "error": True},
        {"name": "mutation_envelope_overflow", "peer": "large", "command": submit, "status": "outcome_unknown", "error": True},
        {"name": "v1_4_result_capability_not_sent", "peer": "unsupported_results", "command": result_query, "status": "not_sent", "error": True},
        {"name": "result_reply_lost_is_query_failure", "peer": "result_closed", "command": result_query, "status": "query_failed", "error": True},
    ]
    hashes = {"binary_sha256": hashlib.sha256(binary.read_bytes()).hexdigest(),
              "plugin_sha256": hashlib.sha256(plugin.read_bytes()).hexdigest()}
    results = []
    for case in cases:
        result = run_case(binary, plugin, case)
        results.append(result)
        print("passed " + case["name"], flush=True)
    assert hashlib.sha256(binary.read_bytes()).hexdigest() == hashes["binary_sha256"]
    assert hashlib.sha256(plugin.read_bytes()).hexdigest() == hashes["plugin_sha256"]
    return {"passed": True, **hashes, "cases": results, "all_fixture_processes_and_threads_reaped": True}


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, required=True)
    parser.add_argument("--plugin", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    assert not args.output.exists(), "do not overwrite evidence"
    result = run(args.binary, args.plugin)
    args.output.write_text(json.dumps(result, indent=2) + "\n")
    print(json.dumps({k: v for k, v in result.items() if k != "cases"}))
