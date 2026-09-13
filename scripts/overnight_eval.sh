#!/bin/bash
# 夜间连跑：套件 ×N 串行。收编自 /tmp/overnight_eval.sh。
# 用法：scripts/overnight_eval.sh [轮数，默认3]
#   模型可用环境变量覆盖（默认 gemini 双模型基线）。
set -u
cd "$(dirname "$0")/.."
mkdir -p test-artifacts
LOG=test-artifacts/overnight_eval.log
ROUNDS="${1:-3}"
MODEL="${EVAL_MODEL:-34217be8-d0a3-45fe-aba5-d62a4d21c486}"
REVIEWER="${EVAL_REVIEWER_MODEL:-27d56872-9156-4751-bcdd-db9237b2222f}"
DURATION="${EVAL_DURATION:-40}"
echo "===== 🌙 夜间评测开始 $(date '+%m-%d %H:%M') =====" >> "$LOG"
for i in $(seq 1 "$ROUNDS"); do
  echo "" >> "$LOG"
  echo "===== 📝 套件第 $i/$ROUNDS 轮开始 $(date '+%H:%M') =====" >> "$LOG"
  npm run eval:real -- --live --all --model "$MODEL" --reviewer-model "$REVIEWER" --duration "$DURATION" >> "$LOG" 2>&1
  echo "===== ✅ 套件第 $i/$ROUNDS 轮结束 $(date '+%H:%M') =====" >> "$LOG"
done
echo "" >> "$LOG"
echo "===== 🌅 夜间评测全部完成 $(date '+%m-%d %H:%M') =====" >> "$LOG"
