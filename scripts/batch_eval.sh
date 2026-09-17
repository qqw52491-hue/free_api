#!/bin/bash
# 批量实弹评测队列：交替双模型跑代表性考题，串行避免资源争抢
cd "$(dirname "$0")/.."
FLASH="34217be8-d0a3-45fe-aba5-d62a4d21c486"   # gemini-3.8-flash-high
PRO="bcb44ed7-65b3-4856-9829-13f966d6cdc9"     # gemini-3.1-pro

run() {
  local case_id="$1" model="$2" tag="$3"
  echo ""
  echo "################## [$tag] $case_id ##################"
  node scripts/eval_runner.mjs --case "$case_id" --live --model "$model" --duration 15 2>&1 | tail -12
}

run case_ft_bare_delegate      "$PRO"   "3.1pro|裸委托"
run case_hd_false_premise      "$FLASH" "3.8flash|误报性报障"
run case_hd_run_binding        "$PRO"   "3.1pro|跨run索引绑定"
run case_ft_forensic_lie       "$FLASH" "3.8flash|审查拒伪"
run case_03_blind_write_interception "$PRO" "3.1pro|盲写拦截"
run case_ft_forensic_phantom   "$FLASH" "3.8flash|幽灵产物"
run case_hd_summary_tail       "$PRO"   "3.1pro|交付文本清洗"
run case_02_readonly_diagnosis "$FLASH" "3.8flash|只读诊断"

echo ""
echo "################## 全部完成 ##################"
