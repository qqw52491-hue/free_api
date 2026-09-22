#!/usr/bin/env bash
# case_20 多模型轮换稳定性回归：每个候选执行者跑指定场次，汇总 CSV。
# 用法: ./scripts/case20_rotation.sh [每场限时分钟] [里程点步数]
set -u
cd "$(dirname "$0")/.."
DUR="${1:-30}"; MILESTONE="${2:-25}"
# Shantung 配额已耗尽（quota_exceeded_error 实测），改全 Antigravity 阵容。
PRO="bcb44ed7-65b3-4856-9829-13f966d6cdc9"   # gemini-3.1-pro supervisor
# (executor_id|场次) —— 探针验证过的可用模型
PLAN=(
  "81b32ced-ddac-44fa-ad6b-7b6c7c122b79|3"   # gemini-3.8-flash-medium
  "b55f70a6-7484-4294-8a3b-96c8038a0676|3"   # gemini-3.8-flash-low
  "2e82a4c8-8b5e-4076-a96b-b043a11107b8|3"   # gemini-3.7-flash-high
  "bcb44ed7-65b3-4856-9829-13f966d6cdc9|3"   # gemini-3.1-pro 自任执行者（pro级实测）
  "27d56872-9156-4751-bcdd-db9237b2222f|2"   # gemini-3.1-pro-high
  "34217be8-d0a3-45fe-aba5-d62a4d21c486|4"   # gemini-3.8-flash-high 补量（方差基线）
)
for item in "${PLAN[@]}"; do
  IFS='|' read -r MID N <<< "$item"
  bash scripts/batch_case20.sh "$MID" "$PRO" "$N" "$DUR" "$MILESTONE"
done
echo "=== ALL ROTATION DONE ==="
