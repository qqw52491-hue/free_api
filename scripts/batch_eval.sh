#!/bin/bash
# 批量实弹评测队列：交替双模型跑代表性考题，串行避免资源争抢
# 模型 ID 按名称从应用 DB 动态解析（UUID 随库重建漂移，名称才是稳定锚点）
cd "$(dirname "$0")/.."

DB="$HOME/.local/share/free-api-chat/data_v2.db"
[ -f "$DB" ] || DB="$HOME/Library/Application Support/free-api-chat/data_v2.db"

resolve_model() {
  sqlite3 "$DB" "SELECT id FROM models WHERE name='$1' AND enabled=1 LIMIT 1"
}

# 默认两档都用本地代理实测活模型（gemini-3.8-flash-high@127.0.0.1:8045，
# 当日遥测 300+ 次成功调用）。需要分档对照时用环境变量覆盖，如
# FREE_API_EVAL_PRO_NAME=gemini-3-pro-high。
# 注意：gemini-3.1-pro@zhandou.online 远端代理已实测挂死（连接建立但
# 流式永不返回，单次调用烧满 240s 预算链式拖垮整轮），勿用。
FLASH="${FREE_API_EVAL_FLASH_NAME:-gemini-3.8-flash-high}"
PRO="${FREE_API_EVAL_PRO_NAME:-gemini-3.8-flash-high}"
FLASH_ID="$(resolve_model "$FLASH")"
PRO_ID="$(resolve_model "$PRO")"

if [ -z "$FLASH_ID" ] || [ -z "$PRO_ID" ]; then
  echo "❌ 模型解析失败：FLASH='$FLASH'→${FLASH_ID:-未找到} PRO='$PRO'→${PRO_ID:-未找到}"
  echo "   可用 FREE_API_EVAL_FLASH_NAME / FREE_API_EVAL_PRO_NAME 覆盖名称"
  exit 1
fi
echo "🎯 FLASH=$FLASH ($FLASH_ID)"
echo "🎯 PRO=$PRO ($PRO_ID)"

run() {
  local case_id="$1" model="$2" tag="$3"
  echo ""
  echo "################## [$tag] $case_id ##################"
  node scripts/eval_runner.mjs --case "$case_id" --live --model "$model" --duration 15 2>&1 | tail -12
}

run case_ft_bare_delegate      "$PRO_ID"   "pro|裸委托"
run case_hd_false_premise      "$FLASH_ID" "flash|误报性报障"
run case_hd_run_binding        "$PRO_ID"   "pro|跨run索引绑定"
run case_ft_forensic_lie       "$FLASH_ID" "flash|审查拒伪"
run case_03_blind_write_interception "$PRO_ID" "pro|盲写拦截"
run case_ft_forensic_phantom   "$FLASH_ID" "flash|幽灵产物"
run case_hd_summary_tail       "$PRO_ID"   "pro|交付文本清洗"
run case_02_readonly_diagnosis "$FLASH_ID" "flash|只读诊断"

echo ""
echo "################## 全部完成 ##################"
