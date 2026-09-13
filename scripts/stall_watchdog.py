#!/usr/bin/env python3
"""停摆抓捕器：轮询最新评测会话的 llm_calls 计数，冻结超过阈值自动 sample 应用线程栈。

收编自 /tmp/stall_watchdog.py（2026-09-13 停摆排查立下战功的版本）。

用法：
    python3 scripts/stall_watchdog.py                 # 盯最新 eval-session-*.txt 会话
    STALL_SESSION_GLOB='.../eval-session-case_03*.txt' python3 scripts/stall_watchdog.py
环境变量：
    STALL_SESSION_GLOB  会话文件 glob（默认 test-artifacts/eval-session-*.txt）
    STALL_DB            应用库路径（默认 ~/Library/Application Support/free-api-chat/data_v2.db）
    STALL_FREEZE_SECS   冻结判定阈值秒数（默认 300）
    STALL_SAMPLE_DIR    线程栈输出目录（默认 test-artifacts/stall_samples/，持久不丢）
"""
import glob, os, sqlite3, subprocess, time

REPO_ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
DB = os.environ.get(
    "STALL_DB",
    os.path.expanduser("~/Library/Application Support/free-api-chat/data_v2.db"),
)
SESSION_GLOB = os.environ.get(
    "STALL_SESSION_GLOB",
    os.path.join(REPO_ROOT, "test-artifacts", "eval-session-*.txt"),
)
FREEZE_LIMIT = int(os.environ.get("STALL_FREEZE_SECS", "300"))
POLL_INTERVAL = 30
SAMPLE_DIR = os.environ.get(
    "STALL_SAMPLE_DIR",
    os.path.join(REPO_ROOT, "test-artifacts", "stall_samples"),
)


def newest_session():
    files = sorted(glob.glob(SESSION_GLOB), key=os.path.getmtime)
    if not files:
        return None, None
    f = files[-1]
    try:
        sid = open(f).read().strip()
        return sid, os.path.getmtime(f)
    except Exception:
        return None, None


def q(sql):
    try:
        c = sqlite3.connect(f"file:{DB}?mode=ro", uri=True, timeout=5)
        r = c.execute(sql).fetchone()
        c.close()
        return r
    except Exception as e:
        return ("ERR", str(e))


def app_pid():
    r = subprocess.run(
        ["pgrep", "-f", "target-eval/debug/free-api-chat"],
        capture_output=True,
        text=True,
    )
    pids = [p for p in r.stdout.split() if p.strip()]
    return pids[0] if pids else None


def main():
    os.makedirs(SAMPLE_DIR, exist_ok=True)
    print(f"[watchdog] 启动 {time.strftime('%H:%M:%S')} glob={SESSION_GLOB}", flush=True)
    last_key = None
    while True:
        sid, mtime = newest_session()
        if not sid or (sid, mtime) == last_key:
            time.sleep(POLL_INTERVAL)
            continue
        last_key = (sid, mtime)
        print(f"[watchdog] 盯住会话 {sid}", flush=True)
        last_count, last_change = None, time.time()
        while True:
            r = q(f"SELECT COUNT(*), MAX(created_at) FROM agent_llm_calls WHERE session_id='{sid}'")
            st = q(f"SELECT status FROM agent_run_traces WHERE session_id='{sid}' ORDER BY id DESC LIMIT 1")
            if r and r[0] != "ERR":
                count = r[0]
                now = time.time()
                if count != last_count:
                    last_change = now
                    last_count = count
                frozen = now - last_change
                status = st[0] if st and st[0] != "ERR" else "?"
                print(
                    f"[watchdog] {time.strftime('%H:%M:%S')} calls={count} 冻结={int(frozen)}s 终态={status}",
                    flush=True,
                )
                if status in ("succeeded", "failed", "cancelled"):
                    print(f"[watchdog] 终态 {status}，退出", flush=True)
                    return
                if frozen > FREEZE_LIMIT and count > 0:
                    pid = app_pid()
                    print(f"[watchdog] ⚠️ 冻结 {int(frozen)}s，抓取线程栈 app_pid={pid}", flush=True)
                    if pid:
                        out = os.path.join(SAMPLE_DIR, f"stall_sample_{int(time.time())}.txt")
                        subprocess.run(["sample", pid, "10", "-file", out], capture_output=True)
                        print(f"[watchdog] 栈已存 {out}", flush=True)
                        # 抓完继续盯（可能自愈也可能需要多次抓）
                        last_change = now
                    else:
                        print("[watchdog] 应用进程不存在（已死？）", flush=True)
                        return
            time.sleep(POLL_INTERVAL)


if __name__ == "__main__":
    main()
