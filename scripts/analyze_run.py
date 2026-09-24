#!/usr/bin/env python3
"""过程病理分析器：对一场评测的 eval DB 做自动解剖，输出可归类的病理清单。
用法: python3 scripts/analyze_run.py <eval_db> [session_id]
只输出事实与计数，不做语义判决。
"""
import sqlite3, sys, json, collections

db_path = sys.argv[1]
conn = sqlite3.connect(db_path)
conn.row_factory = sqlite3.Row

row = conn.execute(
    "SELECT session_id, status, steps_json, completion_json FROM agent_run_traces "
    "WHERE session_id LIKE 'agent-eval%' ORDER BY started_at DESC LIMIT 1").fetchone()
if not row:
    print("no eval session found"); sys.exit(1)
sid = sys.argv[2] if len(sys.argv) > 2 else row[0]
steps = json.loads(row[2] or "[]")
print(f"=== session {sid} ===")
print(f"status={row[1]}  steps={len(steps)}")

def q(sql, args=(sid,)):
    return conn.execute(sql, args).fetchall()

# 1. 门禁风暴
gates = q("SELECT gate, route, COUNT(*) c FROM agent_gate_fires WHERE session_id=? GROUP BY gate, route ORDER BY c DESC")
storm = [g for g in gates if g[2] >= 4]
if storm:
    print(f"[GATE_STORM] " + "; ".join(f"{g[0]}/{g[1]}x{g[2]}" for g in storm))
elif gates:
    print("gates: " + "; ".join(f"{g[0]}x{g[2]}" for g in gates[:5]))

# 2. 重复派发循环（同 action+完整 params ≥3——截断比对会把不同内容的编辑误判成循环）
seq = [(s.get("action",""), json.dumps(s.get("params") or {},sort_keys=True)) for s in steps]
c = collections.Counter(seq)
loops = [(k,v) for k,v in c.items() if v>=3 and k[0]]
if loops:
    print("[DISPATCH_LOOP] " + "; ".join(f"{k[0]} x{v} {k[1][:70]}" for k,v in loops[:6]))

# 3. LLM 调用健康度
calls = q("SELECT outcome, COUNT(*) c, AVG(total_duration_ms) d, AVG(cache_hit_rate) hr, SUM(prompt_tokens) pt FROM agent_llm_calls WHERE session_id=? GROUP BY outcome")
tot = {}
for o in calls:
    tot[o[0]] = o[1]
    print(f"calls[{o[0]}]: n={o[1]} avg_dur={int(o[2] or 0)}ms cache_hit={round((o[3] or 0),2)} prompt_tok={o[4]}")
pe, ok = tot.get('parse_error',0), tot.get('instruction_ready',0)
if pe >= 5 or (ok+pe>0 and pe/(ok+pe) > 0.25):
    print(f"[PARSE_STORM] parse_error={pe}/{ok+pe}")

# 4. 执行者替身
ownq = q("SELECT model_id, COUNT(*) c FROM agent_llm_calls WHERE session_id=? AND step_id>0 AND outcome='instruction_ready' GROUP BY model_id ORDER BY c DESC")
if len(ownq) > 1:
    print("[EXECUTOR_MIX] " + "; ".join(f"{r[0][:8]}x{r[1]}" for r in ownq))

# 5. 慢步（metrics.total_duration_ms > 120s）
slow = [(s.get("metrics",{}).get("total_duration_ms") or 0, s.get("id"), s.get("action")) for s in steps]
slow = [x for x in slow if x[0] > 120000]
if slow: print("[SLOW_STEP] " + "; ".join(f"step{x[1]}({x[2]}) {int(x[0]/1000)}s" for x in slow[:6]))

# 6. 缓存命中率分布
hrs = [s.get("metrics",{}).get("cache_hit_rate") for s in steps if s.get("metrics",{}).get("cache_hit_rate") is not None]
if hrs:
    low = [h for h in hrs if h < 40]
    print(f"cache_hit_rate: avg={round(sum(hrs)/len(hrs),1)}% min={round(min(hrs),1)}% <40%: {len(low)}/{len(hrs)}")

# 7. 冻结现场
try:
    marks = conn.execute("SELECT session_id, stage, updated_at FROM agent_stage_marks").fetchall()
    for m in marks: print(f"stage[{m[0][:28]}] = {m[1]} @ {m[2][11:19]}")
except Exception: pass

# 8. 子任务结局
kids = q("SELECT status, COUNT(*) FROM agent_child_runs WHERE parent_session_id=? GROUP BY status")
for k in kids: print(f"child[{k[0]}] x{k[1]}")

# 9. finish 挑战
fr = q("SELECT route, COUNT(*) FROM agent_gate_fires WHERE session_id=? AND gate LIKE '%finish%' GROUP BY route")
for f in fr: print(f"finish[{f[0]}] x{f[1]}")

# 10. 空转检测：零写入且步数不少（写动作按本仓工具集实际名收录）
acts = [s.get("action","") for s in steps]
writes = [a for a in acts if a in ("write_file","edit_file","apply_patch","edit","write",
          "local_file","shell","run_command","command","python","bash")]
if len(steps) >= 30 and not writes:
    print(f"[NO_WRITE] {len(steps)} 步零写入动作")

print("=== end ===")
