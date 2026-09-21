#!/usr/bin/env bash
#
# scripts/install.sh
# ------------------------------------------------------------------------------
# 本地打包并替换当前 macOS 的 .app：
#   1. 杀掉当前正在运行的同名应用（先 osascript 优雅退出，超时后再 kill）
#   2. 编译 Rust release 后端
#   3. 构建前端 + Tauri bundle（.app + .dmg）
#   4. 把新的 .app 覆盖安装到 /Applications/（先备份旧版，可回滚）
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
# 安全约束（T-058）：
#   - 默认不调用 kill -9；先 osascript 退出 + 等待，再 SIGTERM，最后才 SIGKILL
#   - 安装前保留 ~/.previous_${APP_NAME}.app 备份，回滚用
#   - 不修改系统隔离属性，只在用户明确 --ignore-isolation 时清 quarantine
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
IGNORE_ISOLATION=false
GRACE_SECONDS=10    # SIGTERM 等待上限（默认 10s）
while [[ $# -gt 0 ]]; do
  case "$1" in
    --local) LOCAL_ONLY=true; shift ;;
    --no-launch) LAUNCH_AFTER=false; shift ;;
    --ignore-isolation) IGNORE_ISOLATION=true; shift ;;
    --grace) GRACE_SECONDS="$2"; shift 2 ;;
    -h|--help)
      cat <<EOF
用法: bash scripts/install.sh [--local] [--no-launch] [--ignore-isolation] [--grace <秒>]
  --local            安装到 ~/Applications (无需 sudo)
  --no-launch        安装后不自动启动新版本
  --ignore-isolation 清除新包的 quarantine（Tauri 自签证书开发期需要）
  --grace <秒>       SIGTERM 后等待秒数（默认 10）
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
  warn "正在运行，先用 AppleScript 优雅退出（保留用户数据）..."
  # osascript 优先；失败也不致命
  osascript -e "tell application \"${DISPLAY_NAME}\" to quit" 2>/dev/null || true

  info "等待优雅退出（最长 ${GRACE_SECONDS}s）..."
  for ((i = 0; i < GRACE_SECONDS; i++)); do
    if ! pgrep -f "${APP_NAME}\.app/Contents/MacOS/${APP_NAME}" > /dev/null 2>&1; then
      break
    fi
    sleep 1
  done
fi

# 仍未退出才 SIGTERM，再 SIGKILL（默认不直接 -9）
PIDS=$(pgrep -f "${APP_NAME}\.app/Contents/MacOS/${APP_NAME}" 2>/dev/null || true)
if [ -n "$PIDS" ]; then
  warn "Graceful 退出超时；发送 SIGTERM: $PIDS"
  kill -TERM $PIDS 2>/dev/null || true
  for ((i = 0; i < 5; i++)); do
    sleep 1
    pgrep -f "${APP_NAME}\.app/Contents/MacOS/${APP_NAME}" > /dev/null 2>&1 || break
  done
  REMAIN=$(pgrep -f "${APP_NAME}\.app/Contents/MacOS/${APP_NAME}" 2>/dev/null || true)
  if [ -n "$REMAIN" ]; then
    err "5s SIGTERM 后仍未退出；最后才用 SIGKILL 兜底"
    kill -KILL $REMAIN 2>/dev/null || true
  fi
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

# =============== 5. 安装（备份旧版 → 复制 → 校验） ===============
info "[3/3] 安装到 $INSTALL_DIR..."
DEST="$INSTALL_DIR/${APP_NAME}.app"
BACKUP="${HOME}/.previous_${APP_NAME}_$(date +%Y%m%d_%H%M%S).app"

# 5a) 备份当前版本（仅当存在且不是我们自己刚构建的）
if [ -d "$DEST" ]; then
  info "备份旧版本到 $BACKUP ..."
  rm -rf "$BACKUP" 2>/dev/null || true
  cp -R "$DEST" "$BACKUP"
  ok "已备份（回滚用：rm -rf $DEST && cp -R $BACKUP $DEST）"
fi

# 5b) 仅在用户显式传 --ignore-isolation 时才清 quarantine
if [ "$IGNORE_ISOLATION" = true ]; then
  warn "清除新包 quarantine（仅开发期）"
  xattr -dr com.apple.quarantine "$APP_PATH" 2>/dev/null || true
else
  info "保留默认 quarantine；如 Gatekeeper 阻断请用 codesign --deep --force --sign - 或走 codesign / 公证"
fi

# 5c) 复制
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

# 5d) 校验：必须能看到可执行二进制
if [ ! -x "$DEST/Contents/MacOS/${APP_NAME}" ]; then
  err "校验失败：未找到 $DEST/Contents/MacOS/${APP_NAME}"
  if [ -d "$BACKUP" ]; then
    warn "回滚到备份..."
    rm -rf "$DEST" 2>/dev/null || true
    cp -R "$BACKUP" "$DEST"
  fi
  exit 1
fi

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
if [ -d "$BACKUP" ]; then
  echo "回滚备份: $BACKUP"
fi
