#!/usr/bin/env bash
# 多题型轮换评测：每场跑完→解剖过程→一行入账。
# 用法: bash scripts/rotation.sh <exec_id> <pro_id> [场次列表文件] [时长min]
set -u
EXEC_ID="${1:?executor model id}"
PRO_ID="${2:?pro model id}"
LIST="${3:-scripts/rotation_cases.txt}"
DUR="${4:-20}"
DB="/Users/wx/Library/Application Support/free-api-chat/data_v2.db"
OUT="test-artifacts/rotation.csv"
LOG_DIR="test-artifacts/rotation-logs"
mkdir -p "$LOG_DIR"
EN=$(sqlite3 "$DB" "SELECT name FROM models WHERE id='$EXEC_ID'")
PN=$(sqlite3 "$DB" "SELECT name FROM models WHERE id='$PRO_ID'")
EXEC_ORIG=$(sqlite3 "$DB" "SELECT enabled FROM models WHERE id='$EXEC_ID'")
PRO_ORIG=$(sqlite3 "$DB" "SELECT enabled FROM models WHERE id='$PRO_ID'")
sqlite3 "$DB" "UPDATE models SET enabled=1 WHERE id IN ('$EXEC_ID','$PRO_ID')"
[ -f "$OUT" ] || echo "case,run,executor,pro,verdict,session,steps,pathologies,report" > "$OUT"

i=0
while IFS= read -r CASE; do
  [ -z "$CASE" ] && continue
  i=$((i+1))
  echo "===== 场次 $i: $CASE | $EN + $PN ====="
  BEFORE=$(date +%s)
  FREE_API_STALL_SECS=300 node scripts/eval_runner.mjs \
    --case "$CASE" --live --model "$EXEC_ID" --pro "$PRO_ID" --duration "$DUR" \
    > "$LOG_DIR/${CASE}-${i}.log" 2>&1
  # 找本场 eval DB（比启动时间新）
  EDB=$(ls -t test-artifacts/eval-dbs/eval-${CASE}-*.db 2>/dev/null | head -1)
  if [ -n "$EDB" ] && [ "$(stat -f %m "$EDB")" -lt "$BEFORE" ]; then EDB=""; fi
  REPORT=$(ls -t agent_evaluations/real-eval-${CASE}-*.json 2>/dev/null | head -1)
  if [ -n "$REPORT" ] && [ "$(stat -f %m "$REPORT")" -lt "$BEFORE" ]; then REPORT=""; fi
  # 解剖
  PATHO="$LOG_DIR/${CASE}-${i}.analysis.txt"
  SID=""
  VERDICT="no_report"
  STEPS=0
  if [ -n "$EDB" ]; then
    SID=$(sqlite3 "$EDB" "SELECT session_id FROM agent_run_traces WHERE session_id LIKE 'agent-eval%' ORDER BY started_at DESC LIMIT 1")
    python3 scripts/analyze_run.py "$EDB" > "$PATHO" 2>&1
    STEPS=$(sqlite3 "$EDB" "SELECT COUNT(*) FROM agent_step_logs WHERE session_id='$SID'")
    VERDICT=$(sqlite3 "$EDB" "SELECT status FROM agent_run_traces WHERE session_id='$SID' ORDER BY started_at DESC LIMIT 1")
    [ -n "$REPORT" ] && VERDICT="${VERDICT}/judge=$(python3 -c "import json;print(json.load(open('$REPORT')).get('passed'))" 2>/dev/null || echo '?')"
    # 判具污染红旗
    JM=$(python3 -c "import json;d=json.load(open('$REPORT'));print('jm' if d.get('judge_modified') else '')" 2>/dev/null || echo "")
    [ "$JM" = "jm" ] && VERDICT="${VERDICT}+jm"
  fi
  FLAGS=$(grep -oE "\[[A-Z_]+\]" "$PATHO" 2>/dev/null | tr '\n' ',' )
  echo "$CASE,$i,$EN,$PN,$VERDICT,$SID,$STEPS,$FLAGS,$REPORT" >> "$OUT"
  echo "    -> $VERDICT steps=$STEPS flags=$FLAGS"
  # 清理本场 DB 副本防磁盘爆（分析已落盘）
  [ -n "$EDB" ] && rm -f "$EDB" "$EDB-wal" "$EDB-shm"
done < "$LIST"
sqlite3 "$DB" "UPDATE models SET enabled=$EXEC_ORIG WHERE id='$EXEC_ID'"
sqlite3 "$DB" "UPDATE models SET enabled=$PRO_ORIG WHERE id='$PRO_ID'"
echo "===== 轮换完成 ====="
