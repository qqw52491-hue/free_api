#!/usr/bin/env bash
# case_20 连场稳定性回归：指定 执行者/监督者 组合跑 N 场，每场独立报告+CSV 汇总。
# 用法: ./scripts/batch_case20.sh <executor_id> <pro_id> <场次> [限时分钟] [里程点步数或空]
set -u
cd "$(dirname "$0")/.."
EXEC_ID="$1"; PRO_ID="$2"; N="${3:-5}"; DUR="${4:-25}"; MILESTONE="${5:-25}"
DB="$HOME/Library/Application Support/free-api-chat/data_v2.db"
OUT="test-artifacts/case20-batch.csv"
LOG_DIR="test-artifacts/case20-logs"
mkdir -p "$LOG_DIR"
EN=$(sqlite3 "$DB" "SELECT name FROM models WHERE id='$EXEC_ID'")
PN=$(sqlite3 "$DB" "SELECT name FROM models WHERE id='$PRO_ID'")
echo "run,executor,pro,passed,session,steps,own_ready,foreign_ready,tokens,report" > "$OUT"

for i in $(seq 1 "$N"); do
  echo "===== case_20 场次 $i/$N: $EN + $PN ====="
  FREE_API_RETRIAGE_MILESTONE_STEPS="$MILESTONE" node scripts/eval_runner.mjs \
    --case case_20_img_upload_fromscratch --live --model "$EXEC_ID" --pro "$PRO_ID" --duration "$DUR" \
    > "$LOG_DIR/run-${i}.log" 2>&1
  REPORT=$(ls -t agent_evaluations/real-eval-case_20_img_upload_fromscratch-*.json 2>/dev/null | head -1)
  ROW=$(python3 - "$REPORT" "$EXEC_ID" <<'PY'
import json, sys
try:
    d = json.load(open(sys.argv[1]))
    t = d.get("telemetry") or {}
    calls = t.get("executor_calls") or []
    wanted = sys.argv[2]
    own = sum(c["count"] for c in calls if c["model_id"] == wanted and c["outcome"] == "instruction_ready")
    foreign = sum(c["count"] for c in calls if c["model_id"] != wanted and c["outcome"] == "instruction_ready")
    steps = len(t.get("traces") or [])
    tokens = (t.get("main_prompt") or 0) + (t.get("main_completion") or 0)
    print(f"{d.get('passed')},{t.get('session_id','')},{steps},{own},{foreign},{tokens}")
except Exception:
    print("parse_err,,0,0,0,0")
PY
)
  echo "$i,$EN,$PN,$ROW,$REPORT" >> "$OUT"
  echo "    -> $ROW"
done
echo "=== DONE -> $OUT ==="
