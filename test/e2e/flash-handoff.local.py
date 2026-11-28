"""Exercise delegation through the real desktop binary with a loopback-only model.

From the repo root, run:
    cargo build --locked --manifest-path src-tauri/Cargo.toml --bin free-api-chat
    python3 test/e2e/flash-handoff.local.py
An optional positional argument selects an already-built desktop binary.
"""

import http.server
import json
import os
import pathlib
import re
import socket
import sqlite3
import subprocess
import sys
import tempfile
import threading
import time
import urllib.request
import uuid


PARENT_GOAL = "LOCAL_HANDOFF_PARENT: 查调用点、复现已知问题、跑回归，然后根据回执确认策略"
PRO = "fixture-primary-id"
FLASH = "fixture-fast-id"
CASES = {
    "callsite": ("查询 caller.py 中的 tracked_call 调用点", "到达调用点", None),
    "repro": ("运行 python3 -B reproduce.py，核对已知失败", "REPRODUCED_CASE", 7),
    "regression": ("运行 python3 -B regression.py，核对指定回归", "REGRESSION_PASSED", 0),
}
INHERIT_GOAL = "核对省略 model_id 时继承当前步骤"
CONTROL_RESULT = "INHERITED_PARENT_MODEL"
FINAL_MARKER = "HANDOFF_APPLIED"


def encode(value):
    return json.dumps(value, ensure_ascii=False)


def instruction(tool, **params):
    return {"tool": tool, "params": params}


def child_task(action, **params):
    return instruction("core", action=action, **params) | {"action": "child_task"}


def has_exit_code(text, code):
    return re.search(rf'(?:exit_code"?|命令退出码):\s*{code}\b', text)


def wait_until(predicate, reason, timeout=35):
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        result = predicate()
        if result:
            return result
        time.sleep(0.05)
    raise AssertionError(reason)


def post_channel(endpoint, path, body):
    request = urllib.request.Request(
        f"http://127.0.0.1:{endpoint['port']}{path}",
        data=encode(body).encode(),
        headers={"X-Channel-Token": endpoint["token"], "Content-Type": "application/json"},
    )
    opener = urllib.request.build_opener(urllib.request.ProxyHandler({}))
    with opener.open(request, timeout=10) as result:
        return json.load(result)


class Fixture:
    def __init__(self, root):
        self.session = None
        self.folder = root
        self.workspace = root / "workspace"
        self.workspace.mkdir()
        self.db = root / "fixture.sqlite"
        self.policy_name = f"policy-{uuid.uuid4().hex}.env"
        self.policy_value = f"POLICY_CONFIRMED_{uuid.uuid4().hex}"
        (self.workspace / self.policy_name).write_text(self.policy_value + "\n")
        (self.workspace / "caller.py").write_text(
            f"tracked_call('{self.policy_name}')  # 到达调用点\n"
        )
        (self.workspace / "reproduce.py").write_text(
            "import sys\nprint('REPRODUCED_CASE')\nsys.exit(7)\n"
        )
        (self.workspace / "regression.py").write_text(
            "import pathlib\n"
            "import re\n"
            "source = pathlib.Path('caller.py').read_text()\n"
            "policy, = re.findall(r\"tracked_call\\('([^']+)'\\)\", source)\n"
            "assert pathlib.Path(policy).read_text().startswith('POLICY_CONFIRMED_')\n"
            "print('REGRESSION_PASSED')\n"
        )
        self.counts = {}
        self.errors = []
        self.child_summaries = {}
        self.parent_result_request = None
        self.lock = threading.Lock()
        self.parent_waiting = threading.Event()

    def query(self, sql, args=()):
        with sqlite3.connect(self.db, timeout=5) as connection:
            return connection.execute(sql, args).fetchall()

    def provision_models(self, port):
        self.query(
            "INSERT INTO platforms(id,name,base_url,api_key,created_at) "
            "VALUES ('fixture','fixture',?,'local-fixture','now')",
            (f"http://127.0.0.1:{port}/v1",),
        )
        for model, label in [(PRO, "Primary fixture"), (FLASH, "Sidecar fixture")]:
            self.query(
                "INSERT INTO models(id,platform_id,name,display_name,max_tokens,context_window,created_at) "
                "VALUES (?,'fixture',?,?,8192,131072,'now')",
                (model, model, label),
            )

    def child_rows(self):
        return self.query(
            "SELECT id,model_id,status,result_json,applied_at,execution_checkpoint_json "
            "FROM agent_child_runs WHERE parent_session_id=? ORDER BY rowid",
            (self.session,),
        )

    def respond(self, request):
        messages = request["messages"]
        contents = "\n".join(str(message["content"]) for message in messages)
        goals = {
            key: re.search(rf"【任务目标】\s*【子任务目标】\s*FLASH_HANDOFF_{key}\b", contents)
            for key in (*CASES, "inherit")
        }
        is_triage = "quality_complexity" in contents and "coordination_scope" in contents
        lane = next((key for key, present in goals.items() if present), "parent")
        if is_triage:
            lane = f"triage-{request['model']}"
        with self.lock:
            count = self.counts[lane] = self.counts.get(lane, 0) + 1
        (self.folder / f"request-{lane}-{count}.json").write_text(encode(request))
        if is_triage:
            assert request["model"] in (PRO, FLASH)
            return {
                "quality_complexity": "low", "asset_risk": "none", "coordination_scope": "isolated",
                "requirement_ambiguity": "low", "route": "direct", "stage_review_mode": "none",
                "final_review_mode": "mechanical_only", "evidence_policy": "none", "confidence": 1,
                "reason": "loopback fixture",
            }
        expected_model = FLASH if lane in CASES else PRO
        assert request["model"] == expected_model, (lane, request["model"], expected_model)
        if lane == "inherit":
            assert count == 1
            return instruction("finish", summary=CONTROL_RESULT)
        if lane in CASES:
            if count == 1:
                if lane == "callsite":
                    return instruction("search", query="tracked_call", kinds=["code"], path="caller.py")
                if lane == "regression":
                    assert self.parent_waiting.wait(15), "parent did not reach the wait for child results"
                script = "reproduce.py" if lane == "repro" else "regression.py"
                return instruction("local_shell", command="python3", args=["-B", script], background=True)
            _, marker, exit_code = CASES[lane]
            if lane == "callsite":
                assert count == 2, (lane, count, contents[-6000:])
                output_ids = re.findall(r"(?m)^output_id: ([\w-]+)", contents)
                assert len(set(output_ids)) == 1, (lane, output_ids, contents[-4500:])
                target = re.search(r"tracked_call\('(policy-[0-9a-f]+\.env)'\)", contents)
                assert target and "caller.py:1" in contents and marker in contents, contents[-4500:]
                evidence = f"caller.py:1 -> {target[1]}"
                source = output_ids[0]
                citation = "output_id"
            else:
                job_id = re.search(r"(?m)^job_id: (job-[\w-]+)", contents)
                assert job_id, (lane, contents[-6000:])
                source = job_id[1]
                if count == 2:
                    return instruction("wait_jobs", job_ids=[source], timeout_secs=30)
                assert count == 3, (lane, count, contents[-6000:])
                assert marker in contents and has_exit_code(contents, exit_code), (lane, contents[-4500:])
                evidence = f"{marker} exit_code:{exit_code}"
                citation = "job_id"
            summary = f"DELIVERY_{lane} {evidence} {citation}:{source}"
            with self.lock:
                self.child_summaries[lane] = (summary, source)
            return instruction("finish", summary=summary)
        assert PARENT_GOAL in contents, contents[-4500:]
        if count == 1:
            return child_task("models")
        if 2 <= count <= 5:
            key = (*CASES, "inherit")[count - 2]
            params = {
                "task": f"FLASH_HANDOFF_{key} " + (CASES[key][0] if key in CASES else INHERIT_GOAL),
                "execution": {
                    "workflow": {"completion": "handoff", "plan_mode": "auto"},
                    "stages": {"plan": False, "plan_review": False, "stage_review": False, "final_review": False},
                },
            }
            if key in CASES:
                configured = re.search(r'"configured_flash_model_id"\s*:\s*"([^"]+)"', contents)
                assert configured, contents[-4500:]
                params["model_id"] = configured[1]
            return child_task("dispatch", **params)
        if self.parent_result_request is None:
            rows = self.child_rows()
            assert len(rows) == 4, rows
            ids = [row[0] for row in rows]
            assert all(child_id in contents for child_id in ids)
            markers = [*(f"DELIVERY_{key}" for key in CASES), CONTROL_RESULT]
            pending = [child_id for child_id, marker in zip(ids, markers) if marker not in contents]
            if count == 6:
                assert pending
                self.parent_waiting.set()
            if pending:
                assert count < 11, (pending, contents[-8000:])
                return instruction("wait_jobs", job_ids=pending or ids, timeout_secs=30)
            # On the first request with all receipts after a wait, derive the
            # next action from the delivered payload.
            for key, row in zip((*CASES, "inherit"), rows):
                collected = row[4]
                summary = self.child_summaries[key][0] if key in CASES else CONTROL_RESULT
                assert collected and summary in contents, (key, contents[-8000:])
            self.parent_result_request = count
            target = re.search(r"DELIVERY_callsite caller.py:1 -> (policy-[0-9a-f]+\.env)", contents)
            assert target, contents[-6000:]
            return instruction("local_file", path=target[1]) | {"action": "read"}
        assert count == self.parent_result_request + 1, count
        assert self.policy_value in contents, contents[-4500:]
        return instruction("finish", summary=f"{FINAL_MARKER} {self.policy_value}")

    def verify(self, completion):
        assert FINAL_MARKER in (completion or ""), completion
        assert 7 <= self.parent_result_request <= 10
        execution_counts = {lane: count for lane, count in self.counts.items() if not lane.startswith("triage-")}
        assert execution_counts == {"parent": self.parent_result_request + 1, "callsite": 2, "repro": 3, "regression": 3, "inherit": 1}, self.counts
        for key, row in zip((*CASES, "inherit"), self.child_rows(), strict=True):
            child_id, model, status, result, collected, checkpoint = row
            assert status == "succeeded" and collected, (key, status, collected)
            selection = json.loads(checkpoint)["state"]["dispatch_input"]["model_selection"]
            if key in CASES:
                assert model == FLASH and selection == {"model_id": FLASH, "source": "dispatch_override"}
                summary, source = self.child_summaries[key]
                assert summary in result
                _, marker, exit_code = CASES[key]
                if key == "callsite":
                    [(output,)] = self.query(
                        "SELECT raw_output FROM agent_step_logs WHERE session_id=? AND output_id=?",
                        (f"task-{child_id}", source),
                    )
                    assert marker in output and "caller.py:1" in output, output
                    print(f"{key}: {model}, caller.py:1, output_id={source}, collected")
                else:
                    [(job_id, result, consumed)] = self.query(
                        "SELECT id,result_json,consumed FROM agent_background_jobs "
                        "WHERE parent_session_id=? AND job_type='shell'",
                        (f"task-{child_id}",),
                    )
                    receipt = json.loads(result)
                    assert job_id == source and consumed and receipt["exit_code"] == exit_code, receipt
                    assert marker in receipt["tail"], receipt
                    print(f"{key}: {model}, exit_code={exit_code}, job_id={job_id}, collected")
            else:
                assert model == PRO and selection == {"model_id": PRO, "source": "parent_execution"}
                print(f"inherit: {model}, collected")


def serve_model(fixture):
    class MockModel(http.server.BaseHTTPRequestHandler):
        def log_message(self, *_):
            pass

        def do_POST(self):
            assert self.path == "/v1/chat/completions", self.path
            request = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
            try:
                result = fixture.respond(request)
            except Exception as error:
                fixture.errors.append(error)
                self.send_error(400, "loopback fixture assertion failed")
                return
            packet = {"choices": [{"delta": {"content": encode(result)}, "finish_reason": "stop"}]}
            body = f"data: {encode(packet)}\n\ndata: [DONE]\n\n".encode()
            self.send_response(200)
            self.send_header("Content-Type", "text/event-stream")
            self.send_header("Content-Length", str(len(body)))
            self.end_headers()
            self.wfile.write(body)

    server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), MockModel)
    threading.Thread(target=server.serve_forever, daemon=True).start()
    return server


def run(binary, root):
    fixture = Fixture(root)
    with serve_model(fixture) as server, (root / "desktop.log").open("w+") as log:
        home = root / "home"
        home.mkdir()
        config = root / "channel-config.json"
        endpoint_file = config.with_suffix(".acp.json")
        with socket.socket() as reserved:
            reserved.bind(("127.0.0.1", 0))
            port = reserved.getsockname()[1]
        config.write_text(encode({
            "port": port, "token": uuid.uuid4().hex, "model_id": PRO, "pro": PRO, "flash": FLASH,
            "vision": PRO, "auto_start_bridge": False, "auto_start_tunnel": False, "llm_retries": 1,
        }))
        env = dict(os.environ, HOME=str(home), XDG_DATA_HOME=str(home), XDG_CONFIG_HOME=str(home),
                   CHANNEL_CONFIG_PATH=str(config), FREE_API_DB_PATH=str(fixture.db),
                   FREE_API_DISABLE_SCHEDULER="1")
        for key in list(env):
            if key in ("SANDWICH_ACP_CHANNEL_URL", "SANDWICH_ACP_CHANNEL_TOKEN") or key.lower() in (
                "http_proxy", "https_proxy", "all_proxy",
            ):
                del env[key]
        process = subprocess.Popen([binary], cwd=fixture.workspace, env=env, stdout=log, stderr=subprocess.STDOUT)
        try:
            wait_until(endpoint_file.is_file, "desktop did not publish its local ACP endpoint")
            fixture.provision_models(server.server_port)
            endpoint = json.loads(endpoint_file.read_text())
            fixture.session = post_channel(endpoint, "/channel/session", {
                "root_path": str(fixture.workspace), "model_id": PRO, "title": "Local Flash handoff fixture",
            })["session_id"]
            run_id = str(uuid.uuid4())
            accepted = post_channel(endpoint, f"/channel/acp/task/{run_id}", {
                "session_id": fixture.session, "goal": PARENT_GOAL, "plan_mode": "auto", "caller_notifies": True,
                "model_routing": {"pro": PRO, "flash": FLASH, "role_models": {"planner": "pro", "executor": "pro"}},
            })
            assert accepted["run_id"] == run_id

            def terminal():
                if fixture.errors:
                    raise fixture.errors[0]
                rows = fixture.query("SELECT status,completion_json FROM agent_run_traces WHERE id=?", (run_id,))
                return rows[0] if rows and rows[0][0] in ("succeeded", "failed", "cancelled") else None

            status, completion = wait_until(terminal, "parent never reached a terminal state")
            assert status == "succeeded", (status, completion)
            fixture.verify(completion)
            print("Pro received the four deliveries, read the path supplied by Flash, and completed.")
        except Exception:
            log.seek(0)
            print(log.read()[-12000:], file=sys.stderr)
            raise
        finally:
            process.terminate()
            try:
                process.wait(timeout=5)
            except subprocess.TimeoutExpired:
                process.kill()
                process.wait()


if __name__ == "__main__":
    repo = pathlib.Path(__file__).resolve().parents[2]
    binary = pathlib.Path(sys.argv[1]).resolve() if len(sys.argv) > 1 else repo / "src-tauri/target/debug/free-api-chat"
    with tempfile.TemporaryDirectory(prefix="free-api-flash-handoff-") as folder:
        run(binary, pathlib.Path(folder).resolve())
