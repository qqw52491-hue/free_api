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
# 执行者/监督者须在模型表 enabled=1 才可被路由选中——临时启用并在结束时还原
EXEC_ORIG=$(sqlite3 "$DB" "SELECT enabled FROM models WHERE id='$EXEC_ID'")
PRO_ORIG=$(sqlite3 "$DB" "SELECT enabled FROM models WHERE id='$PRO_ID'")
sqlite3 "$DB" "UPDATE models SET enabled=1 WHERE id IN ('$EXEC_ID','$PRO_ID')"
[ -f "$OUT" ] || echo "run,executor,pro,passed,session,steps,own_ready,foreign_ready,tokens,report" > "$OUT"

for i in $(seq 1 "$N"); do
  echo "===== case_20 场次 $i/$N: $EN + $PN ====="
  BEFORE=$(date +%s)
  FREE_API_RETRIAGE_MILESTONE_STEPS="$MILESTONE" FREE_API_STALL_SECS=300 node scripts/eval_runner.mjs \
    --case case_20_img_upload_fromscratch --live --model "$EXEC_ID" --pro "$PRO_ID" --duration "$DUR" \
    > "$LOG_DIR/run-${i}.log" 2>&1
  REPORT=$(ls -t agent_evaluations/real-eval-case_20_img_upload_fromscratch-*.json 2>/dev/null | head -1)
  # 防陈旧报告污染：报告必须比本轮启动时间新
  if [ -n "$REPORT" ] && [ "$(stat -f %m "$REPORT")" -lt "$BEFORE" ]; then
    REPORT=""
  fi
  if [ -z "$REPORT" ]; then
    echo "$i,$EN,$PN,no_report,,0,0,0,0," >> "$OUT"
    echo "    -> no_report"
    continue
  fi
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
  # 磁盘保护：每场 ~480MB DB 复制。PASS 场删 DB（报告已够），FAIL 场保留供深挖，
  # 同时仅保留最近 5 个 FAIL DB 防堆积。
  NEWDB=$(ls -t test-artifacts/eval-dbs/eval-case_20_img_upload_fromscratch-*.db 2>/dev/null | head -1)
  if echo "$ROW" | grep -q "^True"; then
    [ -n "$NEWDB" ] && rm -f "$NEWDB"
  else
    ls -t test-artifacts/eval-dbs/eval-case_20_img_upload_fromscratch-*.db 2>/dev/null | tail -n +6 | xargs rm -f 2>/dev/null || true
  fi
  echo "    -> $ROW"
done
# 还原 enabled 原始状态
[ -n "$EXEC_ORIG" ] && sqlite3 "$DB" "UPDATE models SET enabled=$EXEC_ORIG WHERE id='$EXEC_ID'"
[ -n "$PRO_ORIG" ] && sqlite3 "$DB" "UPDATE models SET enabled=$PRO_ORIG WHERE id='$PRO_ID'"
echo "=== DONE -> $OUT ==="
