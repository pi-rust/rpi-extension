#!/usr/bin/env python3
"""Native CLI/DLL result lifecycle regression, not a memory benchmark.

Only the management tool reads results. No JSONL inspection or replay is used.
An intentional SIGKILL is restricted to this fixture's own Host Popen.
"""
import argparse
import hashlib
import http.server
import json
from pathlib import Path
import signal
import tempfile
import threading
import time

from compare_20_memory import Rpc, completion, private_json, uid


class Fixture(http.server.ThreadingHTTPServer):
    daemon_threads = False

    def __init__(self, binary, plugin, root):
        super().__init__(("127.0.0.1", 0), Provider)
        self.binary, self.plugin, self.root = binary, plugin, root
        self.a, self.b, self.request, self.scope = uid(), uid(), uid(), uid()
        self.answer = "A中😀" * 2300 + "\nEND"
        self.gates = {name: threading.Event() for name in ["first", "cancel", "crash"]}
        self.native, self.errors, self.issued, self.results, self.checks = [], [], [], [], []
        self.host = self.cli = None
        self.seen = set()
        self.done = False
        self.driver = self.scenario()
        self.next_command = next(self.driver)
        self.serving = threading.Thread(target=self.serve_forever)
        self.serving.start()

    def result(self, agent, request, run=None, offset=0, max_bytes=4097):
        return {"op": "result", "agent_id": agent, "request_id": request,
                "run_id": run, "offset": offset, "max_bytes": max_bytes}

    def submit(self, agent, request, prompt):
        return {"op": "submit", "agent_id": agent, "request_id": request, "prompt": prompt}

    def poll(self, agent, request, state):
        for _ in range(100):
            details = yield self.result(agent, request)
            assert details["data"]["found"] is True, details
            result = details["data"]["result"]
            if result["state"] == state:
                assert details["error"] == (state in {"aborted", "failed", "unknown"})
                return result
            assert result["state"] in {"admitted", "running", "unknown"}, result["state"]
            time.sleep(0.02)
        raise AssertionError("bounded result polling deadline: " + state)

    def scenario(self):
        details = yield {"op": "status"}
        assert "results" in details["control"]["capabilities"]
        details = yield self.result(self.a, uid())
        assert details["data"]["found"] is False and details["error"] is False
        admission = yield self.submit(self.a, self.request, "first")
        assert admission["error"] is False
        running = yield from self.poll(self.a, self.request, "running")
        self.checks.append("real_running_native_run")
        self.gates["first"].set()
        first = yield from self.poll(self.a, self.request, "completed")
        assert first["run_id"] == running["run_id"]
        output, page = "", first["output"]
        pages = 0
        while True:
            assert page["bytes"] <= 4097
            assert page["bytes"] == len(page["text"].encode())
            assert page["offset"] == len(output.encode())
            output += page["text"]
            pages += 1
            if page["next_offset"] is None:
                break
            details = yield self.result(self.a, self.request, first["run_id"], page["next_offset"])
            assert details["error"] is False
            current = details["data"]["result"]
            assert current["run_id"] == first["run_id"] and current["state"] == "completed"
            page = current["output"]
        assert output == self.answer and pages > 1
        self.checks.append("exact_utf8_multipage_answer")
        for run, offset in [(uid(), 0), (first["run_id"], 2), (first["run_id"], 2**63)]:
            details = yield self.result(self.a, self.request, run, offset)
            assert details["error"] is True and details["status"] == "rejected"
            assert details["code"] == "invalid_request"
        self.checks.append("wrong_run_and_invalid_offsets_rejected")
        details = yield self.submit(self.a, self.request, "first")
        assert details["error"] is False and details["data"]["receipt"] == admission["data"]["receipt"]
        assert self.native.count("first") == 1
        self.checks.append("duplicate_and_result_queries_do_not_replay")
        details = yield self.result(self.b, self.request)
        assert details["data"]["found"] is False
        yield self.submit(self.b, self.request, "other-agent")
        other = yield from self.poll(self.b, self.request, "completed")
        assert other["output"]["text"] == "B-answer"
        details = yield self.result(self.b, self.request, first["run_id"])
        assert details["error"] and details["code"] == "invalid_request"
        self.checks.append("same_request_id_is_agent_scoped")
        later_request = uid()
        yield self.submit(self.a, later_request, "later")
        later = yield from self.poll(self.a, later_request, "completed")
        assert later["output"]["text"] == "later-answer"
        details = yield self.result(self.a, self.request)
        assert details["data"]["result"]["run_id"] == first["run_id"]
        assert details["data"]["result"]["output"] == first["output"]
        self.checks.append("old_answer_is_not_current_session_tip")
        failed_request = uid()
        yield self.submit(self.a, failed_request, "fail")
        failed = yield from self.poll(self.a, failed_request, "failed")
        assert failed["error"] and failed["error"]["message"]
        self.checks.append("real_provider_failure_outcome")
        cancelled_request = uid()
        yield self.submit(self.a, cancelled_request, "cancel")
        yield from self.poll(self.a, cancelled_request, "running")
        details = yield {"op": "inspect", "agent_id": self.a}
        cancel = {"scope": self.scope, "request_id": uid(), "epoch": details["control"]["epoch"],
                  "action": {"op": "cancel_submitted", "agent_id": self.a,
                             "revision": details["data"]["revision"], "submitted_request_id": cancelled_request}}
        details = yield {"op": "administrative_cancel", "request": cancel}
        assert details["error"] is False
        assert details["data"]["receipt"]["status"]["result"]["signalled"] is True
        aborted = yield from self.poll(self.a, cancelled_request, "aborted")
        assert aborted["state"] == "aborted"
        self.gates["cancel"].set()
        self.checks.append("real_running_cancel_then_aborted_outcome")
        crashed_request = uid()
        yield self.submit(self.a, crashed_request, "crash")
        before = yield from self.poll(self.a, crashed_request, "running")
        self.crash_and_restart()
        recovered = yield from self.poll(self.a, crashed_request, "unknown")
        assert recovered["run_id"] == before["run_id"]
        assert self.native.count("crash") == 1
        self.checks.append("crash_restart_unknown_without_replay")
        assert sorted(self.native) == sorted(["first", "other-agent", "later", "fail", "cancel", "crash"])

    def start_host(self, log):
        self.host = Rpc([str(self.binary), "host", "serve", "--config", str(self.config)],
                        self.environment, self.workspace, self.root / log)

    def crash_and_restart(self):
        old, self.host = self.host, None
        assert old is not None and old.process.poll() is None
        old.process.kill()  # ONLY this fixture's retained Popen, no process matching.
        assert old.process.wait(timeout=10) == -signal.SIGKILL
        old.reader.join(timeout=5)
        assert not old.reader.is_alive()
        old.process.stdin.close()
        old.process.stdout.close()
        old.stderr.close()
        # The product deliberately refuses stale endpoints. After reaping ONLY
        # our crashed Host, remove ONLY its socket inside our private temp root;
        # preserve the catalog/session bytes and do not weaken production checks.
        endpoint = self.root / "s"
        assert endpoint.is_socket() and not endpoint.is_symlink()
        assert endpoint.lstat().st_uid == self.root.stat().st_uid
        endpoint.unlink()
        self.gates["crash"].set()
        self.start_host("host-restarted.log")

    def run(self):
        self.workspace = self.root / "w"
        self.workspace.mkdir()
        home = self.root / "h"
        home.mkdir()
        agents = home / "agent"
        agents.mkdir()
        url = "http://127.0.0.1:" + str(self.server_port) + "/v1"
        models = [{"id": "manager", "contextWindow": 131072, "maxTokens": 1024}]
        private_json(agents / "models.json", {"providers": {"fixture": {
            "api": "openai-completions", "baseUrl": url, "apiKey": "manager-fixture-key", "models": models}}})
        profiles = []
        for index, agent in enumerate([self.a, self.b]):
            profile = self.root / ("p" + str(index) + ".json")
            private_json(profile, {
                "agent": {"id": agent, "name": "result-fixture", "cwd": str(self.workspace),
                          "session": {"id": "result-session-" + str(index), "path": str(self.root / ("s" + str(index) + ".jsonl"))},
                          "model": {"provider": "fixture", "model": "native"}, "tools": []},
                "runtime": {"providers": {"fixture": {"api": "openai-completions", "baseUrl": url, "apiKey": "$FIXTURE_KEY",
                    "models": [{"id": "native", "contextWindow": 131072, "maxTokens": 8192}]}}},
                "providerValues": {"FIXTURE_KEY": "native-fixture-key", "NO_PROXY": "*"},
                "toolEnvironment": {"HOME": str(self.workspace), "PATH": "/usr/bin:/bin"},
                "shell": "/bin/sh", "approvedTools": [], "thinking": "off", "timeoutMs": 120000})
            profiles.append(str(profile))
        self.token = uid().replace("-", "")
        self.config = self.root / "c.json"
        private_json(self.config, {"directory": str(self.root / "d"), "socket_path": str(self.root / "s"),
            "host_id": uid(), "token": self.token, "agents": profiles,
            "administration": {"scope": self.scope, "profiles": [], "allowed_agents": [self.a, self.b], "permissions": ["cancel"]}})
        self.environment = {"HOME": str(home), "PATH": "/usr/bin:/bin", "RPI_CODING_AGENT_DIR": str(agents),
                            "RPI_SHARED_HOST_CONFIG": str(self.config), "NO_PROXY": "*", "NO_COLOR": "1", "TERM": "dumb"}
        self.start_host("host.log")
        self.cli = Rpc([str(self.binary), "--mode", "rpc", "--local-only", "--extension", str(self.plugin),
                        "--no-context-files", "--no-skills", "--no-prompt-templates", "--no-session", "--tools", "host",
                        "--provider", "fixture", "--model", "manager", "--thinking", "off", "--approve"],
                       self.environment, self.workspace, self.root / "cli.log")
        self.cli.send({"id": "run", "type": "prompt", "message": "Exercise the request-linked native Host results"})
        self.cli.done()
        assert self.done and not self.errors, self.errors
        ends = [event for event in self.cli.events if event.get("type") == "tool_execution_end"]
        assert len(ends) == len(self.results) == len(self.issued)
        for event, details in zip(ends, self.results):
            assert event["isError"] == details["error"]
            assert event["result"]["details"] == details
        for secret in [self.token, "native-fixture-key", "manager-fixture-key"]:
            assert secret not in json.dumps(ends)
        return {"passed": True, "checks": self.checks, "management_tool_calls": len(ends),
                "native_provider_calls": len(self.native), "native_prompts": self.native,
                "native_events": ends, "fixture_only_expected_sigkill": True,
                "fixture_only_stale_socket_cleanup_after_reaping": True,
                "result_reads_use_tool_only_no_jsonl_inspection": True}

    def close(self):
        for gate in self.gates.values():
            gate.set()
        failures = []
        for process in [self.cli, self.host]:
            if process is not None:
                try:
                    process.close()
                except Exception as error:
                    failures.append(repr(error))
        self.shutdown()
        self.serving.join(timeout=5)
        self.server_close()
        assert not self.serving.is_alive()
        assert not failures, failures


class Provider(http.server.BaseHTTPRequestHandler):
    def setup(self):
        super().setup()
        self.connection.settimeout(130)

    def log_message(self, *_):
        pass

    def do_POST(self):
        fixture = self.server
        native_prompt = None
        try:
            size = int(self.headers["Content-Length"])
            assert 0 < size <= 2 * 1024 * 1024
            body = json.loads(self.rfile.read(size))
            if body["model"] == "native":
                assert self.headers["Authorization"] == "Bearer native-fixture-key"
                native_prompt = next(message["content"] for message in reversed(body["messages"]) if message["role"] == "user")
                assert isinstance(native_prompt, str)
                fixture.native.append(native_prompt)
                if native_prompt in fixture.gates:
                    assert fixture.gates[native_prompt].wait(120), "native fixture gate deadline"
                if native_prompt == "fail":
                    response = json.dumps({"error": {"message": "fixture-native-failure", "type": "invalid_request_error"}}).encode()
                    self.send_response(400)
                    self.send_header("Content-Type", "application/json")
                else:
                    answer = fixture.answer if native_prompt == "first" else "B-answer" if native_prompt == "other-agent" else "later-answer"
                    response = completion({"role": "assistant", "content": answer})
                    self.send_response(200)
                    self.send_header("Content-Type", "text/event-stream")
            else:
                assert body["model"] == "manager"
                assert self.headers["Authorization"] == "Bearer manager-fixture-key"
                tools = [message for message in body["messages"] if message.get("role") == "tool"]
                if tools and tools[-1]["tool_call_id"] not in fixture.seen:
                    fixture.seen.add(tools[-1]["tool_call_id"])
                    raw = tools[-1]["content"]
                    if isinstance(raw, list):
                        raw = "".join(block.get("text", "") for block in raw)
                    details = json.loads(raw)
                    fixture.results.append(details)
                    try:
                        fixture.next_command = fixture.driver.send(details)
                    except StopIteration:
                        fixture.done = True
                if fixture.done:
                    response = completion({"role": "assistant", "content": "manager-complete"}, model="manager")
                else:
                    command = fixture.next_command
                    fixture.issued.append(command)
                    response = completion({"role": "assistant", "tool_calls": [{"index": 0,
                        "id": "result-" + str(len(fixture.issued)), "type": "function", "function": {
                            "name": "host", "arguments": json.dumps({"command": command})}}]}, "tool_calls", "manager")
                self.send_response(200)
                self.send_header("Content-Type", "text/event-stream")
            self.send_header("Content-Length", str(len(response)))
            self.send_header("Connection", "close")
            self.end_headers()
            self.wfile.write(response)
        except (BrokenPipeError, ConnectionResetError) as error:
            if native_prompt not in {"cancel", "crash"}:
                fixture.errors.append(repr(error))
        except Exception as error:
            fixture.errors.append(repr(error))
            self.send_error(500, "result fixture failed")


def run(binary, plugin):
    assert binary.is_absolute() and binary.is_file()
    assert plugin.is_absolute() and plugin.is_file()
    hashes = {"binary_sha256": hashlib.sha256(binary.read_bytes()).hexdigest(),
              "plugin_sha256": hashlib.sha256(plugin.read_bytes()).hexdigest()}
    with tempfile.TemporaryDirectory(prefix="hr", dir="/private/tmp") as temporary:
        root = Path(temporary)
        root.chmod(0o700)
        fixture = Fixture(binary, plugin, root)
        try:
            result = fixture.run()
        except BaseException:
            diagnostic = json.dumps({"checks": fixture.checks, "provider_errors": fixture.errors,
                "last_commands": fixture.issued[-3:], "last_results": fixture.results[-2:],
                "cli_tail": fixture.cli.events[-5:] if fixture.cli else []}, ensure_ascii=False)
            for secret in [getattr(fixture, "token", ""), "native-fixture-key", "manager-fixture-key"]:
                if secret:
                    diagnostic = diagnostic.replace(secret, "[fixture-redacted]")
            print(diagnostic[:18000], flush=True)
            raise
        finally:
            fixture.close()
    assert hashlib.sha256(binary.read_bytes()).hexdigest() == hashes["binary_sha256"]
    assert hashlib.sha256(plugin.read_bytes()).hexdigest() == hashes["plugin_sha256"]
    return {**result, **hashes, "all_fixture_resources_joined": True}


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", required=True, type=Path)
    parser.add_argument("--plugin", required=True, type=Path)
    parser.add_argument("--output", required=True, type=Path)
    args = parser.parse_args()
    assert not args.output.exists(), "do not overwrite evidence"
    result = run(args.binary, args.plugin)
    args.output.write_text(json.dumps(result, indent=2) + "\n")
    print(json.dumps({key: value for key, value in result.items() if key != "native_events"}, ensure_ascii=False))
