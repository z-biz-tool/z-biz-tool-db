#!/usr/bin/env bash
#
# scripts/install.sh
# ------------------------------------------------------------------------------
# 本地打包并替换当前 macOS 的 .app：
#   1. 杀掉当前正在运行的同名应用
#   2. 编译 Rust release 后端
#   3. 构建前端 + Tauri bundle（.app + .dmg）
#   4. 把新的 .app 覆盖安装到 /Applications/
#   5. 启动新版本
#
# 用法:
#   bash scripts/install.sh            # 默认安装到 /Applications/
#   bash scripts/install.sh --local    # 安装到 ~/Applications (无需 sudo)
#   bash scripts/install.sh --no-launch # 安装后不自动启动
#
# 前置条件:
#   - 在项目根目录执行
#   - 已安装 Rust + Node.js + npm
#
set -euo pipefail

# =============== 配置 ===============
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
APP_NAME="z-biz-tool-db"           # .app bundle 名称 (来自 tauri.conf.json productName)
BUNDLE_ID="com.zifang.z-biz-tool-db"  # bundle identifier
DISPLAY_NAME="z-biz-tool-db"

# 颜色
RED='\033[0;31m'; GREEN='\033[0;32m'; YELLOW='\033[1;33m'; BLUE='\033[0;34m'; NC='\033[0m'
info() { printf "${BLUE}[INFO]${NC} %s\n" "$*"; }
ok() { printf "${GREEN}[OK]${NC} %s\n" "$*"; }
warn() { printf "${YELLOW}[WARN]${NC} %s\n" "$*"; }
err() { printf "${RED}[ERROR]${NC} %s\n" "$*" >&2; }

# 参数解析
LOCAL_ONLY=false
LAUNCH_AFTER=true
while [[ $# -gt 0 ]]; do
  case "$1" in
    --local) LOCAL_ONLY=true; shift ;;
    --no-launch) LAUNCH_AFTER=false; shift ;;
    -h|--help)
      cat <<EOF
用法: bash scripts/install.sh [--local] [--no-launch]
  --local      安装到 ~/Applications (无需 sudo)
  --no-launch  安装后不自动启动新版本
EOF
      exit 0
      ;;
    *) err "未知参数: $1"; exit 1 ;;
  esac
done

cd "$PROJECT_DIR"

# =============== 1. 杀掉运行中的进程 ===============
info "检查运行中的 ${DISPLAY_NAME}..."

RUNNING=false
if pgrep -f "${APP_NAME}\.app/Contents/MacOS/${APP_NAME}" > /dev/null 2>&1; then
  RUNNING=true
  warn "正在运行，尝试安全退出..."
  # 优先用 AppleScript 优雅退出（保留用户数据）
  osascript -e "tell application \"${DISPLAY_NAME}\" to quit" 2>/dev/null || true
  sleep 2
fi

# 还有进程则强杀
PIDS=$(pgrep -f "${APP_NAME}\.app/Contents/MacOS/${APP_NAME}" 2>/dev/null || true)
if [ -n "$PIDS" ]; then
  warn "强杀残留进程: $PIDS"
  kill -9 $PIDS 2>/dev/null || true
  sleep 1
fi

if [ "$RUNNING" = true ]; then
  ok "已停止旧版本"
fi

# =============== 2. 决定安装目标 ===============
if [ "$LOCAL_ONLY" = true ]; then
  INSTALL_DIR="$HOME/Applications"
  mkdir -p "$INSTALL_DIR"
else
  INSTALL_DIR="/Applications"
fi
info "安装目标: $INSTALL_DIR/$APP_NAME.app"

# =============== 3. 构建前端 ===============
info "[1/3] 构建前端..."
if [ ! -d node_modules ]; then
  warn "node_modules 不存在，先安装依赖..."
  npm install
fi
npm run build

# =============== 4. 构建 Tauri bundle ===============
info "[2/3] 编译 Rust 后端 + 打 .app bundle（首次约 5-10 分钟）..."
# --no-bundle 可以只产出二进制不打包，但我们要 .app 所以保留 bundle
# 我们这里只产出 .app，不生成 .dmg（用户场景是替换现有 .app，无需 dmg）
TAURI_SKIP_DMG=1 npx tauri build --bundles app

# 查找 .app 产物
APP_PATH=""
for candidate in \
  "src-tauri/target/release/bundle/macos/${APP_NAME}.app" \
  "src-tauri/target/release/bundle/macos/${DISPLAY_NAME}.app" \
  "src-tauri/target/release/bundle/osx/${APP_NAME}.app"; do
  if [ -d "$candidate" ]; then
    APP_PATH="$candidate"
    break
  fi
done

if [ -z "$APP_PATH" ]; then
  err "未找到构建产物 .app"
  err "请检查 src-tauri/target/release/bundle/"
  ls -la src-tauri/target/release/bundle/ 2>/dev/null || true
  exit 1
fi
ok "构建完成: $APP_PATH"

# =============== 5. 复制到 /Applications ===============
info "[3/3] 安装到 $INSTALL_DIR..."
DEST="$INSTALL_DIR/${APP_NAME}.app"

# 先清掉旧的 quarantine（如果有）
xattr -dr com.apple.quarantine "$APP_PATH" 2>/dev/null || true

# 写入 /Applications 需要 sudo
if [ "$INSTALL_DIR" = "/Applications" ] && [ ! -w "$INSTALL_DIR" ]; then
  warn "需要管理员权限写入 /Applications"
  sudo rm -rf "$DEST" 2>/dev/null || true
  sudo cp -R "$APP_PATH" "$DEST"
  sudo xattr -dr com.apple.quarantine "$DEST" 2>/dev/null || true
else
  rm -rf "$DEST" 2>/dev/null || true
  cp -R "$APP_PATH" "$DEST"
fi
ok "已安装: $DEST"

# =============== 6. 启动新版本 ===============
if [ "$LAUNCH_AFTER" = true ]; then
  info "启动新版本..."
  open "$DEST" || true
  ok "已启动"
fi

echo ""
ok "=== 安装完成 ==="
echo "应用路径: $DEST"
echo "Bundle ID: ${BUNDLE_ID}"
echo ""
echo "如果未来想回滚:"
echo "  rm -rf $DEST"