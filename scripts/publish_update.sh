#!/usr/bin/env bash
# publish_update.sh —— 自托管更新发布（经隧道域名分发）
#
# 流程：tauri build（带 createUpdaterArtifacts）→ minisign 签名包
#   → 拷贝产物+签名到 app 数据 updates/ → 生成 latest.json manifest
#   → 客户端通过 https://{域名}/update/latest.json 拉取升级
#
# 前置：~/.tauri/free-api-chat.key 私钥存在（tauri signer generate 产出）
# 用法：bash scripts/publish_update.sh [版本号]   # 默认读 tauri.conf.json 的 version

set -euo pipefail
cd "$(dirname "$0")/.."

CONF=src-tauri/tauri.conf.json
VERSION="${1:-$(python3 -c "import json;print(json.load(open('$CONF'))['version'])")}"
KEY_PATH="${TAURI_SIGNING_PRIVATE_KEY_PATH:-$HOME/.tauri/free-api-chat.key}"
UPDATES_DIR="$HOME/Library/Application Support/free-api-chat/updates"
ENDPOINT_HOST="${UPDATE_HOST:-agent.wxhjy.dpdns.org}"

[ -f "$KEY_PATH" ] || { echo "❌ 缺私钥 $KEY_PATH（先 npx tauri signer generate -w $KEY_PATH --ci -p ''）"; exit 1; }
mkdir -p "$UPDATES_DIR"

echo "==> 1/3 构建+签名 v$VERSION（TAURI build，几分钟）"
TAURI_SIGNING_PRIVATE_KEY_PATH="$KEY_PATH" \
TAURI_SIGNING_PRIVATE_KEY_PASSWORD="${TAURI_SIGNING_PRIVATE_KEY_PASSWORD:-}" \
  npx tauri build

# 2/3 收集 mac 更新产物（.app.tar.gz + .sig）
BUNDLE=src-tauri/target/release/bundle/macos
PKG=$(ls "$BUNDLE"/*.app.tar.gz 2>/dev/null | head -1)
SIG="$PKG.sig"
[ -f "$PKG" ] && [ -f "$SIG" ] || { echo "❌ 未找到更新产物 $BUNDLE/*.app.tar.gz(.sig)"; exit 1; }

NAME="$(basename "$PKG")"
cp -f "$PKG" "$UPDATES_DIR/$NAME"
SIGNATURE=$(cat "$SIG")

# 3/3 生成 latest.json（Tauri updater 协议格式）——只发实际构建的架构
ARCH=$(uname -m)
case "$ARCH" in
  arm64)  PLATFORM="darwin-aarch64" ;;
  x86_64) PLATFORM="darwin-x86_64" ;;
  *)      PLATFORM="linux-$ARCH" ;;
esac
python3 - "$UPDATES_DIR/latest.json" "$PLATFORM" <<PYEOF
import json, sys, datetime
manifest = {
  "version": "$VERSION",
  "notes": "Free API Chat $VERSION",
  "pub_date": datetime.datetime.now(datetime.timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ"),
  "platforms": {
    sys.argv[2]: {
      "signature": "$SIGNATURE",
      "url": "https://$ENDPOINT_HOST/update/files/$NAME"
    }
  }
}
json.dump(manifest, open(sys.argv[1], "w"), ensure_ascii=False, indent=2)
PYEOF

echo "==> ✅ 发布完成（$PLATFORM）：$UPDATES_DIR/latest.json → https://$ENDPOINT_HOST/update/latest.json"
echo "    客户端点「检查更新」即可拉取。其他架构需在其本机重跑本脚本合并 manifest。"
