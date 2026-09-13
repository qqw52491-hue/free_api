#!/usr/bin/env python3
"""功能性问题猎捕：把全部评测轨迹翻底朝天，四类模式逐实例五元组归因。

输出：每类模式的实例清单（session/step/行为/证据片段），供人工归因后写报告。
数据源：agent_run_traces.steps_json + agent_gate_fires + agent_child_runs + agent_llm_calls。
"""
import json, os, re, sqlite3, sys
from collections import defaultdict, Counter

DB = os.path.expanduser("~/Library/Application Support/free-api-chat/data_v2.db")
conn = sqlite3.connect(DB)
conn.row_factory = sqlite3.Row

EVAL = "agent-evaluation-%"

def sessions():
    rows = conn.execute(
        "SELECT session_id, steps_json, status FROM agent_run_traces WHERE session_id LIKE ? ORDER BY started_at",
        (EVAL,)).fetchall()
    out = []
    for r in rows:
        try:
            steps = json.loads(r["steps_json"] or "[]")
        except Exception:
            steps = []
        out.append((r["session_id"], steps, r["status"]))
    return out

def norm_params(step):
    """归一化调用签名：工具+关键参数。search 取 query/path，文件类取 path+行范围，shell 取 command。"""
    tool = (step.get("tool") or "").lower()
    p = step.get("params")
    try:
        pj = json.loads(p) if isinstance(p, str) else (p or {})
    except Exception:
        pj = {"_raw": str(p)[:200]}
    if not isinstance(pj, dict):
        pj = {"_raw": str(pj)[:200]}
    keep = {}
    for k in ("query", "path", "kinds", "start_line", "end_line", "startLine", "endLine",
              "command", "action", "file_path", "pattern", "job_ids", "jobIds", "run_id", "content"):
        if k in pj:
            v = str(pj[k])
            keep[k] = v[:120]
    if step.get("command") and "command" not in keep:
        keep["command"] = str(step["command"])[:120]
    if step.get("action") and "action" not in keep:
        keep["action"] = str(step["action"])[:60]
    return tool, json.dumps(keep, sort_keys=True, ensure_ascii=False)

def sid8(s):
    return s[-8:]

# ============ ① 步数重复 ============
def hunt_repetition():
    print("=" * 78)
    print("① 步数重复猎捕")
    print("=" * 78)
    consec = []      # 连续同签
    reread = []      # 同文件重复读（非连续）
    dup_banner = []  # 去重/零命中横幅命中
    gso_loops = []   # get_step_output 高频
    for sess, steps, _ in sessions():
        # 连续同签
        run_start, run_sig, run_len = 0, None, 0
        sigs = [norm_params(s) for s in steps]
        for i, sig in enumerate(sigs + [(None, None)]):
            if sig == run_sig and sig[0]:
                run_len += 1
            else:
                if run_len >= 2 and sigs[run_start][0] not in ("core",):
                    consec.append((sess, run_start + 1, run_len, sigs[run_start]))
                run_start, run_sig, run_len = i, sig, 1
        # 同文件重复读（search 同 query 也算）
        per_target = defaultdict(list)
        for i, s in enumerate(steps):
            t, pj = sigs[i]
            if t in ("search", "local_file", "remote-http-mcp/read_text_file",
                     "remote-http-mcp/read_file", "remote-http-mcp/ripgrep_search",
                     "get_step_output") and pj != "{}":
                per_target[(t, pj)].append(i + 1)
        for (t, pj), idxs in per_target.items():
            if len(idxs) >= 3:
                reread.append((sess, idxs, t, pj))
        # 去重横幅命中
        for i, s in enumerate(steps):
            o = str(s.get("output") or "")
            if ("去重" in o and "缓存" in o) or "重复检索" in o or "【检索去重】" in o:
                dup_banner.append((sess, i + 1, o[:120]))
        # get_step_output 高频
        gso = [i + 1 for i, s in enumerate(steps)
               if (s.get("tool") or "") == "get_step_output"]
        if len(gso) >= 3:
            gso_loops.append((sess, gso))

    print(f"\n[A] 连续同参调用（≥2 连）共 {len(consec)} 簇，按步数排序 top 20：")
    for sess, st, n, (t, pj) in sorted(consec, key=lambda x: -x[2])[:20]:
        print(f"  {sid8(sess)} step{st}-{st+n-1} ×{n}  {t}  {pj[:100]}")
    by_sess = Counter(sid8(s) for s, _, _, _ in consec)
    print(f"  按会话分布 top10: {by_sess.most_common(10)}")
    print(f"  按工具分布: {Counter(t for _,_,_,(t,_) in consec).most_common()}")

    print(f"\n[B] 同目标重复调用（≥3 次，允许非连续）共 {len(reread)} 组 top 15：")
    for sess, idxs, t, pj in sorted(reread, key=lambda x: -len(x[1]))[:15]:
        print(f"  {sid8(sess)} steps{idxs}  {t}  {pj[:90]}")

    print(f"\n[C] 去重横幅命中 {len(dup_banner)} 次，会话 {len(set(s for s,_,_ in dup_banner))} 个")
    for sess, st, o in dup_banner[:8]:
        print(f"  {sid8(sess)} step{st}  {o}")

    print(f"\n[D] get_step_output ≥3 次会话 {len(gso_loops)} 个：")
    for sess, idxs in gso_loops:
        print(f"  {sid8(sess)} steps{idxs}")

# ============ ② 拦截过多 ============
def hunt_gates():
    print("\n" + "=" * 78)
    print("② 拦截过多猎捕（单会话门禁 ≥5 次）")
    print("=" * 78)
    rows = conn.execute(
        "SELECT session_id, step_id, gate, route FROM agent_gate_fires WHERE session_id LIKE ? ORDER BY session_id, step_id",
        (EVAL,)).fetchall()
    by_sess = defaultdict(list)
    for r in rows:
        by_sess[r["session_id"]].append((r["step_id"], r["route"]))
    hot = {s: v for s, v in by_sess.items() if len(v) >= 5}
    print(f"≥5 次会话 {len(hot)} 个 / 总 {len(by_sess)} 个有门禁会话")
    for sess, fires in sorted(hot.items(), key=lambda kv: -len(kv[1])):
        rc = Counter(r for _, r in fires)
        print(f"\n  {sid8(sess)} 共 {len(fires)} 次: {dict(rc)}")
        for st, route in fires:
            # 拉该步证据
            step = conn.execute(
                """SELECT json_extract(value,'$') FROM agent_run_traces t, json_each(t.steps_json)
                   WHERE t.session_id=? AND CAST(json_extract(value,'$.step_id') AS INTEGER)=?""",
                (sess, st)).fetchone()
            if not step:
                step = conn.execute(
                    """SELECT json_extract(value,'$') FROM agent_run_traces t, json_each(t.steps_json)
                       WHERE t.session_id=? AND CAST(json_extract(value,'$.id') AS INTEGER)=?""",
                    (sess, st)).fetchone()
            ev = ""
            if step:
                sj = json.loads(step[0])
                ev = (str(sj.get("output") or "")[:150]).replace("\n", " ")
            print(f"    step{st:>3} {route:<52} {ev}")

# ============ ③ 错误风暴 ============
def hunt_storms():
    print("\n" + "=" * 78)
    print("③ 错误风暴猎捕（连续 ≥3 步 failed）")
    print("=" * 78)
    # gate 触发的 step 集合（剔除门禁拦截的 failed，只看真失败）
    gate_steps = defaultdict(set)
    for r in conn.execute("SELECT session_id, step_id FROM agent_gate_fires WHERE session_id LIKE ?", (EVAL,)):
        gate_steps[r["session_id"]].add(r["step_id"])
    storms = []
    for sess, steps, _ in sessions():
        run = []
        for i, s in enumerate(steps):
            st = s.get("status")
            step_no = i + 1
            # finish_challenged 属门禁（②覆盖），failed 且无 gate 记录 = 真失败
            if st == "failed" and step_no not in gate_steps[sess]:
                run.append(i)
            else:
                if len(run) >= 3:
                    storms.append((sess, run))
                run = []
        if len(run) >= 3:
            storms.append((sess, run))
    print(f"共 {len(storms)} 场 ≥3 连真失败")
    for sess, idxs in sorted(storms, key=lambda x: -len(x[1])):
        steps = conn.execute(
            "SELECT steps_json FROM agent_run_traces WHERE session_id=?", (sess,)).fetchone()
        sj = json.loads(steps[0]) if steps else []
        print(f"\n  {sid8(sess)} steps {idxs[0]+1}-{idxs[-1]+1} ×{len(idxs)}")
        for i in idxs[:6]:
            s = sj[i]
            print(f"    step{i+1} {s.get('tool')}: {str(s.get('description'))[:60]}")
            print(f"        out: {str(s.get('output'))[:140]}".replace("\n", " "))

# ============ ④ 超时清单 ============
def hunt_timeouts():
    print("\n" + "=" * 78)
    print("④ 超时清单（child_runs timed_out + llm_calls 重试/中断）")
    print("=" * 78)
    rows = conn.execute(
        """SELECT parent_session_id, id, kind, status, error_kind, duration_ms,
                  substr(result_json,1,200)
           FROM agent_child_runs WHERE parent_session_id LIKE ? AND status IN ('timed_out','failed')
           ORDER BY parent_session_id, started_at""", (EVAL,)).fetchall()
    print(f"child_runs 超时/失败 {len(rows)} 条")
    by_sess = defaultdict(list)
    for r in rows:
        by_sess[r["parent_session_id"]].append(r)
    for sess, rs in sorted(by_sess.items(), key=lambda kv: -len(kv[1])):
        print(f"\n  {sid8(sess)} ×{len(rs)}")
        for r in rs:
            # 容灾检查：同会话同 kind 之后是否有 succeeded
            nxt = conn.execute(
                """SELECT status FROM agent_child_runs WHERE parent_session_id=? AND kind=?
                   AND started_at > (SELECT started_at FROM agent_child_runs WHERE id=?) LIMIT 1""",
                (sess, r["kind"], r["id"])).fetchone()
            caught = nxt["status"] if nxt else "无后续"
            print(f"    {r['id'][:8]} {r['kind']:<8} {r['status']:<10} {r['error_kind'] or '-':<12} "
                  f"{round((r['duration_ms'] or 0)/1000)}s → 后续:{caught}")
    # llm_calls 传输层重试/中断
    tr = conn.execute(
        """SELECT session_id, count(*) c FROM agent_llm_calls
           WHERE session_id LIKE ? AND (outcome IN ('interrupted','request_error','parse_error') OR transport_attempt_count>1)
           GROUP BY session_id ORDER BY c DESC""", (EVAL,)).fetchall()
    print(f"\n  llm_calls 异常会话 top10（interrupted/request_error/parse_error/重试）:")
    for r in tr[:10]:
        print(f"    {sid8(r['session_id'])}: {r['c']} 次异常调用")

if __name__ == "__main__":
    which = sys.argv[1] if len(sys.argv) > 1 else "all"
    if which in ("all", "1"):
        hunt_repetition()
    if which in ("all", "2"):
        hunt_gates()
    if which in ("all", "3"):
        hunt_storms()
    if which in ("all", "4"):
        hunt_timeouts()
