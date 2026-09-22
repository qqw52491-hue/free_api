#!/usr/bin/env bash
# 模型矩阵稳定性探针：对每个候选执行者跑最小真实任务 case_ft_model_probe，
# 用 executor_calls 分账验证"指定模型是否真在干活"（防升维救场静默替身）。
# 用法: ./scripts/probe_models.sh [轮数] [每轮单模型限时分钟]
set -u
cd "$(dirname "$0")/.."
DB="$HOME/Library/Application Support/free-api-chat/data_v2.db"
CASE="case_ft_model_probe"
PRO="1304de62-61b7-41f5-8c2b-29797b960a4f"   # glm-5.2 supervisor/triage
ROUNDS="${1:-3}"
DUR="${2:-8}"
OUT="test-artifacts/model-probe-matrix.csv"
LOG_DIR="test-artifacts/probe-logs"
mkdir -p "$LOG_DIR"

# 候选模型（Shantung + Antigravity 两可用平台），id|name|原 enabled 值
CANDIDATES=(
  "dbaa35c8-271d-4e39-b5ee-4925a6162531|sensenova-6.8-flash-lite"
  "e9ad127e-ef34-4b7b-9d12-5a17b09b2c7c|sensenova-6.7-flash-lite"
  "1304de62-61b7-41f5-8c2b-29797b960a4f|glm-5.2"
  "4b78ff71-0be8-43e1-a02c-5a5b77f7d6b4|kimi-k3"
  "c0bdd68c-2a08-429e-bea1-82915febc110|deepseek-v4-flash"
  "66936dd5-368a-4cd2-bb5e-ddbd0a9094c3|deepseek-v4-pro"
  "34217be8-d0a3-45fe-aba5-d62a4d21c486|gemini-3.8-flash-high"
  "81b32ced-ddac-44fa-ad6b-7b6c7c122b79|gemini-3.8-flash-medium"
  "b55f70a6-7484-4294-8a3b-96c8038a0676|gemini-3.8-flash-low"
  "bcb44ed7-65b3-4856-9829-13f966d6cdc9|gemini-3.1-pro"
  "27d56872-9156-4751-bcdd-db9237b2222f|gemini-3.1-pro-high"
  "75f2f755-544e-4579-9f2a-28e1f6129ced|claude-opus-4.6"
  "4e4e3721-2c9b-49d6-8e9d-ed48543e9083|claude-opus-4.6-thinking"
  "2e82a4c8-8b5e-4076-a96b-b043a11107b8|gemini-3.7-flash-high"
)

echo "round,model_name,model_id,passed,session,own_ready,foreign_ready,total_calls,report" > "$OUT"

for r in $(seq 1 "$ROUNDS"); do
  for entry in "${CANDIDATES[@]}"; do
    IFS='|' read -r MID MNAME <<< "$entry"
    ORIG=$(sqlite3 "$DB" "SELECT enabled FROM models WHERE id='$MID'")
    sqlite3 "$DB" "UPDATE models SET enabled=1 WHERE id='$MID'"
    echo "=== round $r | $MNAME ($MID) ==="
    BEFORE=$(date +%s)
    node scripts/eval_runner.mjs --case "$CASE" --live --model "$MID" --pro "$PRO" --duration "$DUR" \
      > "$LOG_DIR/${MNAME}-r${r}.log" 2>&1
    REPORT=$(ls -t agent_evaluations/real-eval-${CASE}-*.json 2>/dev/null | head -1)
    # 防陈旧报告污染：报告必须比本轮启动时间新，否则视为未产出
    if [ -n "$REPORT" ] && [ "$(stat -f %m "$REPORT")" -lt "$BEFORE" ]; then
      REPORT=""
    fi
    if [ -z "$REPORT" ]; then
      ROW="no_report,,0,0,0"
      echo "$r,$MNAME,$MID,$ROW," >> "$OUT"
      [ -n "$ORIG" ] && sqlite3 "$DB" "UPDATE models SET enabled=$ORIG WHERE id='$MID'"
      echo "    -> no_report ($(($(date +%s)-BEFORE))s)"
      continue
    fi
    ROW=$(python3 - "$REPORT" "$MID" <<'PY'
import json, sys
try:
    d = json.load(open(sys.argv[1]))
    t = d.get("telemetry") or {}
    calls = t.get("executor_calls") or []
    wanted = sys.argv[2]
    own = sum(c["count"] for c in calls if c["model_id"] == wanted and c["outcome"] == "instruction_ready")
    foreign = sum(c["count"] for c in calls if c["model_id"] != wanted and c["outcome"] == "instruction_ready")
    total = sum(c["count"] for c in calls)
    print(f"{d.get('passed')},{t.get('session_id','')},{own},{foreign},{total}")
except Exception as e:
    print(f"parse_err,,,,0")
PY
)
    echo "$r,$MNAME,$MID,$ROW,$REPORT" >> "$OUT"
    # 收完数据即删本场 eval DB（每场 ~480MB 全量复制，攒多了会塞满磁盘；
    # 结果已在报告 JSON + CSV 里，DB 只对失败深挖有用——探针一律删）
    NEWDB=$(ls -t test-artifacts/eval-dbs/eval-${CASE}-*.db 2>/dev/null | head -1)
    [ -n "$NEWDB" ] && rm -f "$NEWDB"
    # 还原 enabled（探针不该改动用户模型配置）
    [ -n "$ORIG" ] && sqlite3 "$DB" "UPDATE models SET enabled=$ORIG WHERE id='$MID'"
    echo "    -> $ROW ($(($(date +%s)-BEFORE))s)"
  done
done
echo "=== DONE -> $OUT ==="
