#!/usr/bin/env python3
"""Bounded macOS comparison: 20 shared Host agents vs 20 native RPC processes.

Explicit artifacts only; no installed binary, credentials, extensions or user history.
The HTTP fixture/controller is outside the measured set. phys_footprint is a
per-process ledger metric, not a deduplicated system-wide physical memory union.
Only fixture-owned processes/files are cleaned up. No production source edits.
"""
import argparse
import ctypes
import hashlib
import http.server
import json
import os
from pathlib import Path
import platform
import signal
import socket
import statistics
import struct
import subprocess
import tempfile
import threading
import time
import uuid

COUNT = 20
ANSWER = "memory-comparison-answer"
PROMPT = "Reply with a short answer. Identical 4096-byte payload: " + "x" * 4096


def uid():
    return str(uuid.uuid4())


def sha(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def private_json(path, value):
    path.write_text(json.dumps(value))
    path.chmod(0o600)


def completion(delta, finish="stop", model="native"):
    chunks = [
        {"id": "fixture", "object": "chat.completion.chunk", "created": 1,
         "model": model, "choices": [{"index": 0, "delta": delta, "finish_reason": None}]},
        {"id": "fixture", "object": "chat.completion.chunk", "created": 1,
         "model": model, "choices": [{"index": 0, "delta": {}, "finish_reason": finish}],
         "usage": {"prompt_tokens": 1, "completion_tokens": 1, "total_tokens": 2}},
    ]
    return ("".join("data: " + json.dumps(c) + "\n\n" for c in chunks) + "data: [DONE]\n\n").encode()


class Rusage(ctypes.Structure):
    _fields_ = [("uuid", ctypes.c_uint8 * 16)] + [(name, ctypes.c_uint64) for name in (
        "user_time", "system_time", "pkg_idle_wkups", "interrupt_wkups", "pageins",
        "wired_size", "resident_size", "phys_footprint", "proc_start_abstime", "proc_exit_abstime")]


class TaskInfo(ctypes.Structure):
    _fields_ = [("times", ctypes.c_uint64 * 6), ("counters", ctypes.c_int32 * 12)]


def resource(pid):
    lib = ctypes.CDLL("/usr/lib/libproc.dylib", use_errno=True)
    usage = lib.proc_pid_rusage
    usage.argtypes = [ctypes.c_int, ctypes.c_int, ctypes.c_void_p]
    info = Rusage()
    assert usage(pid, 0, ctypes.byref(info)) == 0, (pid, ctypes.get_errno())
    pidinfo = lib.proc_pidinfo
    pidinfo.argtypes = [ctypes.c_int, ctypes.c_int, ctypes.c_uint64, ctypes.c_void_p, ctypes.c_int]
    task = TaskInfo()
    assert pidinfo(pid, 4, 0, ctypes.byref(task), ctypes.sizeof(task)) == ctypes.sizeof(task)
    size = pidinfo(pid, 1, 0, None, 0)
    assert size > 0
    buffer = ctypes.create_string_buffer(size + 256)
    count = pidinfo(pid, 1, 0, buffer, len(buffer))
    assert 0 < count < len(buffer), "FD sample overflow"
    return {"pid": pid, "footprint": info.phys_footprint, "rss": info.resident_size,
            "threads": task.counters[9], "fds": count // 8}


def samples(processes):
    rows = []
    for _ in range(7):
        assert all(p.poll() is None for p in processes), "measured process exited"
        values = [resource(p.pid) for p in processes]
        rows.append({"time_ns": time.monotonic_ns(), "processes": values,
                     **{k: sum(p[k] for p in values) for k in ("footprint", "rss", "threads", "fds")}})
        time.sleep(0.1)
    return {"count": len(processes), "samples": rows,
            "median": {k: statistics.median(r[k] for r in rows) for k in ("footprint", "rss", "threads", "fds")},
            "range": {k: [min(r[k] for r in rows), max(r[k] for r in rows)] for k in ("footprint", "rss", "threads", "fds")}}


class Rpc:
    def __init__(self, command, environment, workspace, log):
        self.stderr = log.open("w")
        self.log = log
        self.is_host = len(command) > 1 and command[1] == "host"
        try:
            self.process = subprocess.Popen(command, env=environment, cwd=workspace,
                                            stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                                            stderr=self.stderr, text=True, start_new_session=True)
        except BaseException:
            self.stderr.close()
            raise
        self.events = []
        self.condition = threading.Condition()
        self.eof = False
        self.reader = threading.Thread(target=self.read)
        self.reader.start()
        try:
            self.wait(lambda e: e.get("type") == "ready" or e.get("event") == "host_ready")
        except BaseException:
            try:
                self.close()
            except Exception:
                pass  # Original readiness error retains the diagnostic.
            raise

    def read(self):
        try:
            for line in self.process.stdout:
                try:
                    value = json.loads(line)
                except ValueError:
                    value = {"non_json": line[:200]}
                with self.condition:
                    self.events.append(value)
                    self.condition.notify_all()
        finally:
            with self.condition:
                self.eof = True
                self.condition.notify_all()

    def send(self, value):
        self.process.stdin.write(json.dumps(value) + "\n")
        self.process.stdin.flush()

    def wait(self, predicate, timeout=60):
        deadline = time.monotonic() + timeout
        with self.condition:
            while True:
                for event in self.events:
                    if predicate(event):
                        return event
                assert not self.eof, ("RPC EOF", self.process.poll(), self.events[-4:], self.log.read_text()[-2000:])
                remaining = deadline - time.monotonic()
                assert remaining > 0, ("RPC deadline", self.events[-4:], self.log.read_text()[-2000:])
                self.condition.wait(remaining)

    def done(self):
        self.wait(lambda e: e.get("type") == "agent_end")
        assert ANSWER in json.dumps(self.events) or "manager-complete" in json.dumps(self.events)

    def close(self):
        if self.process.poll() is None:
            if self.is_host:
                # Only the PID owned by this Popen, never a pattern-based kill.
                self.process.send_signal(signal.SIGTERM)
            else:
                try:
                    self.send({"id": "fixture-stop", "type": "stop"})
                except BrokenPipeError:
                    pass
            try:
                self.process.wait(timeout=20)
            except subprocess.TimeoutExpired:
                self.process.kill()
                self.process.wait(timeout=5)
                raise AssertionError("fixture process needed SIGKILL")
        self.reader.join(timeout=5)
        assert not self.reader.is_alive()
        self.process.stdin.close()
        self.process.stdout.close()
        self.stderr.close()
        assert self.process.returncode == 0, self.process.returncode


class Fixture(http.server.ThreadingHTTPServer):
    request_queue_size = 128
    daemon_threads = False

    def __init__(self):
        super().__init__(("127.0.0.1", 0), Provider)
        self.release = threading.Event()
        self.condition = threading.Condition()
        self.native = []
        self.errors = []
        self.pending = []
        self.manager_results = []
        self.issued = []
        self.probing = False
        self.seen_tool_results = set()
        self.thread = threading.Thread(target=self.serve_forever)
        self.thread.start()

    def barrier(self):
        deadline = time.monotonic() + 60
        with self.condition:
            while len(self.native) < COUNT and not self.errors:
                remaining = deadline - time.monotonic()
                assert remaining > 0, ("20-request barrier deadline", len(self.native), self.errors)
                self.condition.wait(remaining)
        assert not self.errors, self.errors
        assert len(self.native) == COUNT, len(self.native)

    def close(self):
        self.release.set()
        self.shutdown()
        self.thread.join(timeout=5)
        assert not self.thread.is_alive()
        self.server_close()  # joins all request threads


class Provider(http.server.BaseHTTPRequestHandler):
    def setup(self):
        super().setup()
        self.connection.settimeout(90)

    def log_message(self, *_):
        pass

    def do_POST(self):
        fixture = self.server
        try:
            size = int(self.headers["Content-Length"])
            assert 0 < size <= 2 * 1024 * 1024
            body = json.loads(self.rfile.read(size))
            assert self.headers["Authorization"] == "Bearer memory-fixture-key"
            if body["model"] == "native":
                assert body["messages"][-1]["content"] == PROMPT
                assert not body.get("tools"), "native tool sets must both be empty"
                with fixture.condition:
                    fixture.native.append(body)
                    fixture.condition.notify_all()
                assert fixture.release.wait(90), "fixture release deadline"
                response = completion({"role": "assistant", "content": ANSWER})
            else:
                assert body["model"] == "manager"
                tools = [m for m in body["messages"] if m.get("role") == "tool"]
                if tools and tools[-1]["tool_call_id"] not in fixture.seen_tool_results:
                    fixture.seen_tool_results.add(tools[-1]["tool_call_id"])
                    raw = tools[-1]["content"]
                    if isinstance(raw, list):
                        raw = "".join(x.get("text", "") for x in raw)
                    result = json.loads(raw)
                    result = result.get("details", result)
                    fixture.manager_results.append(result)
                    if fixture.issued[-1].get("probe"):
                        assert result["error"] is True and result["status"] == "rejected", result
                    else:
                        assert result["error"] is False and result["data"]["durable"] is True, result
                if fixture.pending:
                    command = fixture.pending.pop(0)
                    fixture.issued.append({"command": command, "probe": fixture.probing})
                    response = completion({"role": "assistant", "tool_calls": [{"index": 0,
                        "id": "host-" + str(len(fixture.issued)), "type": "function",
                        "function": {"name": "host", "arguments": json.dumps({"command": command})}}]},
                        "tool_calls", "manager")
                else:
                    response = completion({"role": "assistant", "content": "manager-complete"}, model="manager")
            self.send_response(200)
            self.send_header("Content-Type", "text/event-stream")
            self.send_header("Content-Length", str(len(response)))
            self.send_header("Connection", "close")
            self.end_headers()
            self.wfile.write(response)
        except Exception as error:
            with fixture.condition:
                fixture.errors.append(repr(error))
                fixture.condition.notify_all()
            self.send_error(500, "fixture assertion failed")


def host_query(config, command):
    """Fixture-only bounded wire reader, never a production client or fallback."""
    def receive(stream, count):
        data = b""
        while len(data) < count:
            chunk = stream.recv(count - len(data))
            assert chunk, "unexpected EOF"
            data += chunk
        return data

    def read(stream):
        size = struct.unpack("!I", receive(stream, 4))[0]
        assert 0 < size <= 1024 * 1024
        return json.loads(receive(stream, size))

    def write(stream, value):
        data = json.dumps(value).encode()
        stream.sendall(struct.pack("!I", len(data)) + data)

    with socket.socket(socket.AF_UNIX) as stream:
        stream.settimeout(5)
        stream.connect(config["socket_path"])
        write(stream, {"protocol": "rpi-shared-host", "version": {"major": 1, "minor": 4},
                       "host_id": config["host_id"], "token": config["token"]})
        welcome = read(stream)
        assert welcome["host_id"] == config["host_id"]
        call = uid()
        write(stream, {"call_id": call, "epoch": welcome["epoch"], "command": command})
        response = read(stream)
        assert response["call_id"] == call and response["epoch"] == welcome["epoch"]
        return response["reply"]


def run_case(binary, plugin, root, workspace, environment, agents, mode, round_number):
    case = root / (mode + "-" + str(round_number))
    case.mkdir(mode=0o700)
    fixture = Fixture()
    processes = []
    backend = []
    manager = None
    try:
        url = "http://127.0.0.1:" + str(fixture.server_port) + "/v1"
        model = {"api": "openai-completions", "baseUrl": url, "apiKey": "memory-fixture-key",
                 "models": [{"id": name, "contextWindow": 32768, "maxTokens": 1024} for name in ("native", "manager")]}
        private_json(agents / "models.json", {"providers": {"fixture": model}})
        common = [str(binary), "--mode", "rpc", "--local-only", "--no-context-files", "--no-skills",
                  "--no-prompt-templates", "--provider", "fixture", "--model", "native", "--thinking", "off", "--approve"]
        identities = [uid() for _ in range(COUNT)]
        request_ids = [uid() for _ in range(COUNT)]
        if mode == "shared":
            profiles = []
            for index, agent in enumerate(identities):
                path = case / (str(index) + ".json")
                private_json(path, {"agent": {"id": agent, "name": "memory-agent", "cwd": str(workspace),
                    "session": {"id": agent, "path": str(case / (agent + ".jsonl"))},
                    "model": {"provider": "fixture", "model": "native"}, "tools": []},
                    "runtime": {"providers": {"fixture": {**model, "apiKey": "$FIXTURE_KEY"}}},
                    "providerValues": {"FIXTURE_KEY": "memory-fixture-key", "NO_PROXY": "*"},
                    "toolEnvironment": {"HOME": str(workspace), "PATH": "/usr/bin:/bin"},
                    "shell": "/bin/sh", "approvedTools": [], "thinking": "off", "timeoutMs": 120000})
                profiles.append(str(path))
            config = {"directory": str(case / "d"), "socket_path": str(case / "s"), "host_id": uid(),
                      "token": uuid.uuid4().hex, "host": {"max_agents": COUNT}, "agents": profiles}
            config_path = case / "c.json"
            private_json(config_path, config)
            host = Rpc([str(binary), "host", "serve", "--config", str(config_path)], environment, workspace, case / "host.log")
            processes.append(host)
            backend.append(host.process)
            manager_environment = {**environment, "RPI_SHARED_HOST_CONFIG": str(config_path)}
            command = [*common, "--no-session", "--tools", "host", "--extension", str(plugin)]
            command[command.index("--model") + 1] = "manager"
            manager = Rpc(command, manager_environment, workspace, case / "manager.log")
            processes.append(manager)
            fixture.pending = [{"op": "submit", "agent_id": agent, "request_id": request, "prompt": PROMPT}
                               for agent, request in zip(identities, request_ids)]
        else:
            for index, agent in enumerate(identities):
                rpc = Rpc([*common, "--no-tools", "--session-id", agent, "--session-dir", str(case / "sessions")],
                          environment, workspace, case / (str(index) + ".log"))
                processes.append(rpc)
                backend.append(rpc.process)
        time.sleep(2)
        stages = {"idle": samples(backend)}
        manager_stages = {"idle": samples([manager.process])} if manager else None
        total_stages = {"idle": samples(backend + [manager.process])} if manager else None
        if manager:
            manager.send({"id": "run", "type": "prompt", "message": "Submit the 20 explicit fixture requests"})
        else:
            for rpc in processes:
                rpc.send({"id": "run", "type": "prompt", "message": PROMPT})
        fixture.barrier()
        if manager:
            manager.done()
            assert len(fixture.manager_results) == COUNT
        time.sleep(2)
        assert len(fixture.native) == COUNT and not fixture.release.is_set()
        stages["provider_barrier"] = samples(backend)
        if manager:
            manager_stages["provider_barrier"] = samples([manager.process])
            total_stages["provider_barrier"] = samples(backend + [manager.process])
        fixture.release.set()
        if manager:
            deadline = time.monotonic() + 40
            for agent, request in zip(identities, request_ids):
                while True:
                    reply = host_query(config, {"op": "receipt", "agent_id": agent, "request_id": request})
                    if reply.get("data", {}).get("execution", {}).get("finished") is True:
                        break
                    assert time.monotonic() < deadline, reply
                    time.sleep(0.05)
            # Progress journals also end in .jsonl; inspect only the 20 exact
            # authoritative history paths instead of counting a glob.
            histories = [case / (agent + ".jsonl") for agent in identities]
            assert all(p.is_file() and ANSWER in p.read_text() for p in histories)
        else:
            for rpc in processes:
                rpc.done()
        time.sleep(3)
        stages["completed_retained"] = samples(backend)
        if manager:
            manager_stages["completed_retained"] = samples([manager.process])
            total_stages["completed_retained"] = samples(backend + [manager.process])
        probe = None
        if manager:
            before = len(manager.events)
            fixture.probing = True
            # Valid schema, nonexistent Agent: reach the real DLL/server rather
            # than being rejected by the Harness's argument validator.
            fixture.pending = [{"op": "submit", "agent_id": uid(), "request_id": uid(), "prompt": PROMPT}]
            manager.send({"id": "probe", "type": "prompt", "message": "Probe a nonexistent Agent"})
            manager.wait(lambda e: e.get("type") == "response" and e.get("id") == "probe")
            deadline = time.monotonic() + 10
            while not any(e.get("type") == "agent_end" for e in manager.events[before:]):
                assert time.monotonic() < deadline
                time.sleep(0.05)
            ends = [e for e in manager.events[before:] if e.get("type") == "tool_execution_end"]
            assert len(ends) == 1, manager.events[before:]
            probe = {"details_error": fixture.manager_results[-1]["error"],
                     "details_status": fixture.manager_results[-1]["status"], "native_event": ends[0]}
        assert not fixture.errors, fixture.errors
        signatures = [hashlib.sha256(json.dumps(b, sort_keys=True).encode()).hexdigest() for b in fixture.native]
        messages = [hashlib.sha256(json.dumps(b["messages"], sort_keys=True).encode()).hexdigest() for b in fixture.native]
        result = {"mode": mode, "round": round_number, "agent_count": COUNT, "backend": stages,
                  "manager": manager_stages, "with_manager": total_stages, "native_provider_calls": len(fixture.native),
                  "provider_payload_sha256": signatures, "provider_messages_sha256": messages,
                  "first_provider_payload": fixture.native[0], "error_flag_probe": probe}
        return result
    finally:
        fixture.release.set()
        errors = []
        for process in reversed(processes):
            try:
                process.close()
            except Exception as error:
                errors.append(repr(error))
        fixture.close()
        assert not errors, errors


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", required=True, type=Path)
    parser.add_argument("--plugin", required=True, type=Path)
    parser.add_argument("--output", required=True, type=Path)
    parser.add_argument("--rounds", type=int, choices=(1, 2, 3), default=3)
    args = parser.parse_args()
    assert platform.system() == "Darwin", "libproc measurement requires macOS"
    assert args.binary.is_absolute() and args.binary.is_file()
    assert args.plugin.is_absolute() and args.plugin.is_file()
    assert not args.output.exists(), "never overwrite earlier evidence"
    result = {"binary_sha256": sha(args.binary), "plugin_sha256": sha(args.plugin),
              "script_sha256": sha(Path(__file__)), "machine": platform.platform(),
              "logical_cpus": os.cpu_count(), "agents": COUNT, "rounds": args.rounds,
              "metric": "sum of libproc RUSAGE_INFO_V0 phys_footprint ledgers; RSS auxiliary, not unique physical union",
              "scope": "backend-only primary; actual DLL management CLI measured separately for shared case; Python/HTTP excluded",
              "cases": []}
    with tempfile.TemporaryDirectory(prefix="m20", dir="/private/tmp") as temporary:
        root = Path(temporary)
        root.chmod(0o700)
        workspace = root / "w"
        workspace.mkdir()
        home = root / "h"
        home.mkdir()
        agents = home / "agent"
        agents.mkdir()
        environment = {"HOME": str(home), "PATH": "/usr/bin:/bin", "RPI_CODING_AGENT_DIR": str(agents),
                       "NO_PROXY": "*", "NO_COLOR": "1", "TERM": "dumb"}
        for round_number in range(args.rounds):
            order = ("shared", "rpc") if round_number % 2 == 0 else ("rpc", "shared")
            for mode in order:
                print("start", round_number, mode, flush=True)
                case = run_case(args.binary, args.plugin, root, workspace, environment, agents, mode, round_number)
                result["cases"].append(case)
                args.output.write_text(json.dumps(result, indent=2) + "\n")
                print("passed", round_number, mode, {stage: values["median"] for stage, values in case["backend"].items()}, flush=True)
    assert sha(args.binary) == result["binary_sha256"] and sha(args.plugin) == result["plugin_sha256"]
    result["passed"] = True
    result["all_fixture_processes_and_threads_reaped"] = True
    args.output.write_text(json.dumps(result, indent=2) + "\n")


if __name__ == "__main__":
    main()
