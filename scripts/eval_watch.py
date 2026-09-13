#!/usr/bin/env python3
"""实时监听器：5 秒采样最新评测会话（状态/调用数/门禁触发/最新步骤）落盘到日志。

收编自 /tmp/eval_watch.py。
⚠️ 连续同描述 ≥5 次标记疑似死循环——但 edit 步骤执行超 5 秒会误报，属正常现象。

用法：
    python3 scripts/eval_watch.py
环境变量：
    EVAL_SESSION_GLOB  会话文件 glob（默认 test-artifacts/eval-session-*.txt）
    EVAL_DB            应用库路径（默认 ~/Library/Application Support/free-api-chat/data_v2.db）
    EVAL_WATCH_LOG     输出日志路径（默认 test-artifacts/eval_watch.log）
"""
import datetime
import glob
import json
import os
import sqlite3
import time

REPO_ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
DB = os.environ.get(
    "EVAL_DB",
    os.path.expanduser("~/Library/Application Support/free-api-chat/data_v2.db"),
)
SESSION_GLOB = os.environ.get(
    "EVAL_SESSION_GLOB",
    os.path.join(REPO_ROOT, "test-artifacts", "eval-session-*.txt"),
)
LOG = os.environ.get(
    "EVAL_WATCH_LOG",
    os.path.join(REPO_ROOT, "test-artifacts", "eval_watch.log"),
)
open(LOG, "w").close()
last_desc = None
streak = 0


def ts():
    return datetime.datetime.now().strftime("%H:%M:%S")


def newest_session_file():
    fs = [f for f in glob.glob(SESSION_GLOB) if not f.endswith(".status")]
    return max(fs, key=os.path.getmtime) if fs else None


def q(db, sql):
    try:
        return db.execute(sql).fetchone()
    except Exception:
        return None


while True:
    f_ = newest_session_file()
    sid = open(f_).read().strip() if f_ else ""
    if not sid:
        with open(LOG, "a") as f:
            f.write(f"{ts()} | 等待会话产生...\n")
        time.sleep(5)
        continue
    db = sqlite3.connect(f"file:{DB}?mode=ro", uri=True)
    status_row = q(db, f"SELECT status FROM agent_run_traces WHERE session_id='{sid}'")
    status = status_row[0] if status_row else "starting"
    calls = (q(db, f"SELECT COUNT(*) FROM agent_llm_calls WHERE session_id='{sid}'") or [0])[0]
    fires = (q(db, f"SELECT count(*) FROM agent_gate_fires WHERE session_id='{sid}'") or [0])[0]
    row = q(db, f"SELECT steps_json FROM agent_run_traces WHERE session_id='{sid}'")
    last_desc_s, n = "无轨迹", 0
    if row and row[0]:
        try:
            steps = json.loads(row[0])
            n = len(steps)
            s = steps[-1] if steps else None
            if s:
                last_desc_s = f"{s.get('id')}.{(s.get('action') or '?')[-16:]} {(s.get('description') or '')[:34]}"
        except Exception:
            last_desc_s = "解析失败"
    line = f"{ts()} | {status} | calls={calls} | fires={fires} | {n}步 | 最新: {last_desc_s}"
    if last_desc_s == last_desc:
        streak += 1
    else:
        streak = 0
        last_desc = last_desc_s
    if streak >= 4:
        line += f" ⚠️疑似循环(同描述连续{streak + 1}次)"
    with open(LOG, "a") as f:
        f.write(line + "\n")
    db.close()
    if status in ("succeeded", "failed", "cancelled"):
        with open(LOG, "a") as f:
            f.write(f"{ts()} 🏁 运行终态: {status}\n")
        break
    time.sleep(5)
