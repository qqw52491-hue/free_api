#!/usr/bin/env python3
"""E 项：零 token 成本解剖 —— 每题五段成本（探索/方案/写码/审查/返工）+ 门禁触发普查。

数据源：
- agent_evaluations/real-eval-*.json → case_id ↔ agent_session ↔ passed
- agent_run_traces.steps_json → 每步 tool/action/description
- agent_llm_calls → step_id → prompt/completion tokens
- agent_gate_fires → gate/route 触发计数

阶段分类（按步工具+时序）：
- plan:     submit_plan / plan 相关动作
- review:   wait_jobs / child dispatch / review 消费步
- code:     write/edit/受控 shell 写
- rework:   首个 review 结果落地之后发生的 code/search 步
- explore:  其余 search/read/list/shell 侦察步
"""
import json, os, glob, sqlite3, sys
from collections import defaultdict

DB = os.path.expanduser("~/Library/Application Support/free-api-chat/data_v2.db")
REPORTS = glob.glob(os.path.join(os.path.dirname(__file__), "..", "agent_evaluations", "real-eval-*.json"))

PLAN_ACTIONS = {"submit_plan"}
REVIEW_TOOLS = {"wait_jobs", "child_reviewer", "child_worker", "child_writer", "ask_human"}
WRITE_ACTIONS = {"edit", "write", "create", "patch", "delete"}
SHELL_WRITE_HINTS = ("cat >", "cat>", ">>", "apply_patch", "git apply", "tee ", "sed -i", "python3 - <<", "cargo test", "cargo build", "cargo check", "npm test")

def classify(tool, action, desc, params, seen_review, seen_code):
    t = (tool or "").lower()
    a = (action or "").lower()
    d = desc or ""
    p = str(params or "")
    if "submit_plan" in a or "submit_plan" in d:
        return "plan"
    if t in REVIEW_TOOLS:
        return "review"
    if t == "finish":
        return "finish"
    is_write = a in WRITE_ACTIONS or (t == "local_shell" and any(h in p for h in SHELL_WRITE_HINTS))
    if is_write:
        return "rework" if seen_review else "code"
    # 读/搜/侦察/其它（含 core/no-op 步）
    return "rework" if seen_review and seen_code else "explore"

def main():
    conn = sqlite3.connect(DB)
    conn.row_factory = sqlite3.Row

    # 1. case ↔ session 映射（取每 case 最新报告会话，逐 case 聚合全部）
    case_sessions = defaultdict(list)
    for rp in REPORTS:
        try:
            r = json.load(open(rp))
        except Exception:
            continue
        sid = r.get("agent_session")
        cid = r.get("case_id")
        if sid and cid:
            case_sessions[cid].append((sid, r.get("passed"), r.get("generated_at")))

    print("=" * 72)
    print("五段成本解剖（主循环 prompt tokens，按步工具归段）")
    print("=" * 72)
    print(f"{'case':<38} {'run':>10} {'explore':>8} {'plan':>8} {'code':>8} {'review':>8} {'rework':>8} {'finish':>8} {'total':>10}")

    case_phase_totals = defaultdict(lambda: defaultdict(list))
    for cid, sess in sorted(case_sessions.items()):
        for sid, passed, _ in sess:
            steps_row = conn.execute(
                "SELECT steps_json FROM agent_run_traces WHERE session_id=? ORDER BY started_at DESC LIMIT 1", (sid,)
            ).fetchone()
            if not steps_row or not steps_row[0]:
                continue
            steps = json.loads(steps_row[0])
            tok = {r["step_id"]: (r["prompt_tokens"] or 0) for r in conn.execute(
                "SELECT step_id, sum(prompt_tokens) prompt_tokens FROM agent_llm_calls WHERE session_id=? GROUP BY step_id", (sid,))}
            phases = defaultdict(int)
            seen_review = seen_code = False
            for s in steps:
                tool = s.get("tool") or ""
                act = s.get("action") or ""
                desc = s.get("description") or ""
                params = s.get("params") or s.get("command") or ""
                ph = classify(tool, act, desc, params, seen_review, seen_code)
                raw_id = str(s.get("id", "0"))
                sid_step = int(raw_id.split("-")[0]) if raw_id.split("-")[0].isdigit() else 0
                phases[ph] += tok.get(sid_step, 0)
                if ph == "code":
                    seen_code = True
                # 方案审查发生在写码之前，不应把全部实现计入 rework；
                # 只有"已有代码之后"的审查结果才开启返工段。
                if ph == "review" and seen_code:
                    seen_review = True
            total = sum(phases.values())
            for ph, v in phases.items():
                case_phase_totals[cid][ph].append(v)
            mark = "✅" if passed else "❌"
            print(f"{cid:<38} {sid[-8:]} {mark} {phases['explore']:>8,} {phases['plan']:>8,} {phases['code']:>8,} {phases['review']:>8,} {phases['rework']:>8,} {phases['finish']:>8,} {total:>10,}")

    print("\n" + "=" * 72)
    print("每题阶段占比（多次取均值）")
    print("=" * 72)
    print(f"{'case':<38} {'n':>3} {'explore%':>9} {'plan%':>7} {'code%':>7} {'review%':>8} {'rework%':>8}")
    for cid, ph_map in sorted(case_phase_totals.items()):
        n = max(len(v) for v in ph_map.values())
        totals = [sum(ph_map[ph][i] if i < len(ph_map[ph]) else 0 for ph in ph_map) for i in range(n)]
        mean_total = sum(totals) / n if n else 0
        row = [cid, str(n)]
        for ph in ["explore", "plan", "code", "review", "rework"]:
            m = sum(ph_map.get(ph, [0])) / n
            row.append(f"{100*m/mean_total:>7.1f}%" if mean_total else "  -")
        print(f"{row[0]:<38} {row[1]:>3} " + " ".join(row[2:]))

    print("\n" + "=" * 72)
    print("门禁触发普查（全部评测会话）")
    print("=" * 72)
    rows = conn.execute(
        "SELECT gate, route, count(*) c FROM agent_gate_fires WHERE session_id LIKE 'agent-evaluation-%' GROUP BY gate, route ORDER BY c DESC"
    ).fetchall()
    for r in rows:
        print(f"  {r['gate']:<38} {r['route']:<44} ×{r['c']}")

if __name__ == "__main__":
    main()
