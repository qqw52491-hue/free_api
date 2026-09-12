#!/usr/bin/env python3
"""评测过程分析器：对指定会话输出全量过程态报告（轨迹/思维/协作/门禁/收敛性）。
用法: python3 scripts/eval_analyze.py <session_id> [<session_id2> ...]
数据源: data_v2.db 的 agent_run_traces / agent_llm_calls / agent_child_runs / agent_gate_fires
"""
import json, sqlite3, sys, os, collections

DB = os.path.expanduser('~/Library/Application Support/free-api-chat/data_v2.db')


def load(sid):
    db = sqlite3.connect(f'file:{DB}?mode=ro', uri=True)
    row = db.execute(
        "SELECT steps_json, status FROM agent_run_traces WHERE session_id=?", (sid,)
    ).fetchone()
    if not row:
        db.close()
        return None
    steps = json.loads(row[0])
    calls = db.execute(
        "SELECT COUNT(*), coalesce(sum(prompt_tokens),0), coalesce(sum(completion_tokens),0) "
        "FROM agent_llm_calls WHERE session_id=?", (sid,)
    ).fetchone()
    children = db.execute(
        "SELECT status, duration_ms FROM agent_child_runs WHERE parent_session_id=?", (sid,)
    ).fetchall()
    fires = db.execute(
        "SELECT gate, route, count(*) FROM agent_gate_fires WHERE session_id=? "
        "GROUP BY gate, route ORDER BY 3 DESC", (sid,)
    ).fetchall()
    db.close()
    return {'steps': steps, 'status': row[1], 'calls': calls[0], 'prompt': calls[1],
            'completion': calls[2], 'children': children, 'fires': fires}


def ms(v):
    return f"{v / 1000:.0f}s" if v else "0s"


def analyze(sid):
    d = load(sid)
    if not d:
        return f"── {sid} ── 无轨迹记录"
    steps, calls = d['steps'], d['calls']
    acts = collections.Counter((s.get('action') or '').split('/')[-1] for s in steps)

    think = sum((s.get('metrics') or {}).get('thinking_duration_ms', 0) for s in steps)
    tool = sum((s.get('metrics') or {}).get('tool_duration_ms', 0) for s in steps)
    max_think = max(((s.get('metrics') or {}).get('thinking_duration_ms', 0) for s in steps), default=0)

    reads = [s for s in steps if 'read' in (s.get('action') or '')]
    read_paths = collections.Counter((s.get('command') or {}).get('path', '?') for s in reads)
    searches = [s for s in steps if (s.get('action') or '') == 'search']
    s_queries = collections.Counter(json.dumps(s.get('command') or {}, sort_keys=True) for s in searches)
    s_dup = sum(v - 1 for v in s_queries.values())

    finishes = [s for s in steps if 'finish' in (s.get('action') or '')]
    ok_child = sum(1 for c in d['children'] if c[0] == 'succeeded')
    to_child = sum(1 for c in d['children'] if c[0] == 'timed_out')

    out = [f"── {sid[:30]} ── 终态: {d['status']}"]
    out.append(f"步数 {len(steps)} | LLM调用 {calls} | prompt {d['prompt']:,} tok | completion {d['completion']:,} tok")
    out.append(f"动作画像: {dict(acts.most_common(7))}")
    out.append(f"思维程度: 总思考 {ms(think)} (步均 {ms(think // max(len(steps), 1))}, 单步峰值 {ms(max_think)}) | 工具执行 {ms(tool)}")
    out.append(f"读取: {len(reads)} 次 / {len(read_paths)} 文件 (top2: {read_paths.most_common(2)})")
    out.append(f"检索: {len(searches)} 次 / {len(s_queries)} 种 (重复 {s_dup})")
    out.append(f"交付尝试: {len(finishes)} 次")
    out.append(f"多agent协作: 派发 {len(d['children'])} 子agent (成功 {ok_child}, 超时 {to_child})")
    out.append("门禁触发: " + ("; ".join(f"{g}/{r}×{n}" for g, r, n in d['fires']) if d['fires'] else "0 次"))
    return "\n".join(out)


if __name__ == '__main__':
    sids = [a for a in sys.argv[1:] if not a.startswith('--')]
    if not sids:
        print(__doc__)
        sys.exit(1)
    print("\n\n".join(analyze(s) for s in sids))
