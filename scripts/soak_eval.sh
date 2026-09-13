#!/bin/bash
# D 项浸泡测试：指定题集 ×N 轮串行实弹，逐轮检查状态泄漏/顺序依赖/flaky。
# 用法：scripts/soak_eval.sh [轮数，默认3] [题集，默认8道核心题]
#   模型/时长可用环境变量覆盖。
set -u
cd "$(dirname "$0")/.."
mkdir -p test-artifacts
LOG=test-artifacts/soak_eval.log
ROUNDS="${1:-3}"
CASES="${2:-case_01_subagent_failover case_02_readonly_diagnosis case_03_blind_write_interception case_04_mine_async_checkpoint case_05_mine_secret_in_logs case_06_mine_drop_transaction case_07_mine_unbounded_cache case_08_mine_retry_storm}"
MODEL="${EVAL_MODEL:-34217be8-d0a3-45fe-aba5-d62a4d21c486}"
REVIEWER="${EVAL_REVIEWER_MODEL:-27d56872-9156-4751-bcdd-db9237b2222f}"
DURATION="${EVAL_DURATION:-40}"

echo "===== 🌊 浸泡测试开始 $(date '+%m-%d %H:%M') rounds=$ROUNDS =====" >> "$LOG"
for i in $(seq 1 "$ROUNDS"); do
  echo "" >> "$LOG"
  echo "===== 🔄 第 $i/$ROUNDS 轮开始 $(date '+%H:%M') =====" >> "$LOG"
  for c in $CASES; do
    # 每题前探针：429/不可达时退避 20 分钟，禁止硬烧
    CODE=$(curl -s -o /dev/null -w '%{http_code}' --max-time 8 http://127.0.0.1:8045/v1/models || echo 000)
    if [ "$CODE" = "429" ] || [ "$CODE" = "000" ]; then
      echo "⏸️  探针异常($CODE) @ $c，退避 20 分钟 $(date '+%H:%M')" >> "$LOG"
      sleep 1200
    fi
    echo "▶ [$i] $c 开始 $(date '+%H:%M')" >> "$LOG"
    node scripts/eval_runner.mjs --live --case "$c" --model "$MODEL" --reviewer-model "$REVIEWER" --duration "$DURATION" >> "$LOG" 2>&1
    echo "■ [$i] $c 结束 $(date '+%H:%M')" >> "$LOG"
  done
  echo "===== ✅ 第 $i/$ROUNDS 轮完成 $(date '+%H:%M') =====" >> "$LOG"
done
echo "" >> "$LOG"
echo "===== 🌅 浸泡测试全部完成 $(date '+%m-%d %H:%M') =====" >> "$LOG"
