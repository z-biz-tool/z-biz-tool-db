#!/usr/bin/env bash
#
# scripts/install.sh
# ------------------------------------------------------------------------------
# 本地打包并替换当前 macOS 的 .app：
#   1. 杀掉当前正在运行的同名应用（先 osascript 优雅退出，超时后再 kill）
#   2. 编译 Rust release 后端
#   3. 构建前端 + Tauri bundle（.app）
#   4. 把新的 .app 覆盖安装到 /Applications/（先备份旧版，可回滚）
#   5. 启动新版本
#
# 用法:
#   bash scripts/install.sh            # 默认安装到 /Applications/
#   bash scripts/install.sh --local    # 安装到 ~/Applications (无需 sudo)
#   bash scripts/install.sh --install-dir <path>   # 装到指定目录（演练/沙箱用）
#   bash scripts/install.sh --no-launch # 安装后不自动启动
#
# 前置条件:
#   - 在项目根目录执行
#   - 已安装 Rust + Node.js + npm
#
# 安全约束（T-058）：
#   - 默认不调用 kill -9；先 osascript 退出 + 等待，再 SIGTERM，最后才 SIGKILL
#   - 安装前保留 ~/.previous_${APP_NAME}_<时间戳>.app 备份，回滚用
#   - 不修改系统隔离属性，只在用户明确 --ignore-isolation 时清 quarantine
#   - 覆盖安装走「拷到同目录临时名 → 换名落位」，任何一步失败旧应用都分毫不动
#     （早先是先删旧再拷新，删失败时新版会被拷进旧目录里变成嵌套，而 5d 的校验
#      查的是旧二进制，会误判通过并打印「=== 安装完成 ===」）
#
# 产物定位：不写死 src-tauri/target。
#   `~/.cargo/config.toml` 的 build.target-dir 会把整棵 target 树重定向到别处，
#   写死会让脚本在"编译成功"之后报"未找到构建产物"并以 rc=1 退出 ——
#   症状看起来像构建失败，其实是产物在别处。
#   一律以 cargo metadata 报的 target_directory 为准，src-tauri/target 只作兜底。
#   ⚠️ 这个兜底必须真的走得到：脚本是 set -euo pipefail，而 pipefail 会把
#   `cargo metadata` 的失败状态带到整条管道，于是 `TARGET_DIR=$(...)` 这个赋值
#   本身就失败 → set -e 直接以 cargo 的退出码（常见 101）终止脚本。
#   实测：cargo 一坏，产物明明就在 src-tauri/target/release/bundle/macos/，
#   脚本却在 [2/3] 就退 101，永远走不到下面那段兜底。所以那里必须有 `|| true`。
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
INSTALL_DIR_OVERRIDE=""
GRACE_SECONDS=10    # SIGTERM 等待上限（默认 10s）
while [[ $# -gt 0 ]]; do
  case "$1" in
    --local) LOCAL_ONLY=true; shift ;;
    --no-launch) LAUNCH_AFTER=false; shift ;;
    --ignore-isolation) IGNORE_ISOLATION=true; shift ;;
    --install-dir) INSTALL_DIR_OVERRIDE="${2:-}"; shift 2 ;;
    --grace) GRACE_SECONDS="$2"; shift 2 ;;
    -h|--help)
      cat <<EOF
用法: bash scripts/install.sh [--local] [--install-dir <path>] [--no-launch] [--ignore-isolation] [--grace <秒>]
  --local            安装到 ~/Applications (无需 sudo)
  --install-dir <p>  安装到指定目录（演练 / 沙箱用；不给就是 /Applications）
  --no-launch        安装后不自动启动新版本
  --ignore-isolation 清除新包的 quarantine（Tauri 自签证书开发期需要）
  --grace <秒>       优雅退出等待上限（默认 10）
EOF
      exit 0
      ;;
    *) err "未知参数: $1"; exit 1 ;;
  esac
done

cd "$PROJECT_DIR"

# =============== 1. 停掉运行中的进程 ===============
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
  warn "优雅退出超时；发送 SIGTERM: $PIDS"
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
if [ -n "$INSTALL_DIR_OVERRIDE" ]; then
  INSTALL_DIR="$INSTALL_DIR_OVERRIDE"
elif [ "$LOCAL_ONLY" = true ]; then
  INSTALL_DIR="$HOME/Applications"
else
  INSTALL_DIR="/Applications"
fi
mkdir -p "$INSTALL_DIR"

# /Applications 不可写时整段安装走 sudo，其余直连。集中在一个函数里，
# 免得"删旧""拷新""换名""回滚"四条路径各自写一遍 sudo 而漏掉某一条。
USE_SUDO=false
if [ "$INSTALL_DIR" = "/Applications" ] && [ ! -w "$INSTALL_DIR" ]; then
  USE_SUDO=true
  warn "需要管理员权限写入 /Applications"
fi
elev() { if [ "$USE_SUDO" = true ]; then sudo "$@"; else "$@"; fi; }
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

# 查找 .app 产物（以 cargo metadata 为准，见文件头说明）
TARGET_DIR=""
if command -v cargo > /dev/null 2>&1 && [ -d src-tauri ]; then
  # `|| true` 不是可有可无的保险：脚本是 set -euo pipefail，pipefail 会把
  # cargo metadata 的失败状态带到整条管道，于是这个赋值本身就失败 → set -e
  # 直接以 cargo 的退出码终止脚本，下面那段 src-tauri/target 兜底根本走不到。
  # 2026-10-03 实测：cargo 一坏，产物明明就在 src-tauri/target/release/bundle/
  # macos/ 里，脚本却在 [2/3] 退 101。
  TARGET_DIR=$(cd src-tauri && cargo metadata --format-version 1 --no-deps 2>/dev/null \
    | sed -n 's/.*"target_directory":"\([^"]*\)".*/\1/p' | head -1) || true
  if [ -z "$TARGET_DIR" ]; then
    info "cargo metadata 没给出 target_directory，改用 src-tauri/target 兜底"
  fi
fi

APP_PATH=""
for root in "${TARGET_DIR:-}" "src-tauri/target"; do
  [ -n "$root" ] || continue
  for bundle_dir in "release/bundle/macos" "release/bundle/osx"; do
    for name in "${APP_NAME}.app" "${DISPLAY_NAME}.app"; do
      candidate="$root/$bundle_dir/$name"
      if [ -d "$candidate" ]; then
        APP_PATH="$candidate"
        break 3
      fi
    done
  done
done

if [ -z "$APP_PATH" ]; then
  err "未找到构建产物 .app"
  err "cargo target_directory=${TARGET_DIR:-<未知>}"
  err "请检查 $TARGET_DIR/release/bundle/ 与 src-tauri/target/release/bundle/"
  ls -la "${TARGET_DIR:-src-tauri/target}/release/bundle/" 2>/dev/null || true
  ls -la src-tauri/target/release/bundle/ 2>/dev/null || true
  exit 1
fi
ok "构建完成: $APP_PATH"

# =============== 5. 安装（备份旧版 → 复制 → 校验） ===============
info "[3/3] 安装到 $INSTALL_DIR..."
DEST="$INSTALL_DIR/${APP_NAME}.app"
BACKUP="${HOME}/.previous_${APP_NAME}_$(date +%Y%m%d_%H%M%S).app"

# 5a) 备份当前版本，便于回滚
if [ -d "$DEST" ]; then
  info "备份旧版本到 $BACKUP ..."
  elev rm -rf "$BACKUP" 2>/dev/null || true
  elev cp -R "$DEST" "$BACKUP"
  ok "已备份（回滚用：rm -rf ${DEST} && cp -R ${BACKUP} ${DEST}）"
fi

# 5b) 仅在用户显式传 --ignore-isolation 时才清 quarantine
if [ "$IGNORE_ISOLATION" = true ]; then
  warn "清除新包 quarantine（仅开发期）"
  xattr -dr com.apple.quarantine "$APP_PATH" 2>/dev/null || true
else
  info "保留默认 quarantine；如 Gatekeeper 阻断请用 codesign --deep --force --sign - 或走 codesign / 公证"
fi

# 5c) 覆盖安装：拷到同目录临时名 → 换名落位
#
# 早先这里是 `rm -rf "$DEST" 2>/dev/null || true` 然后 `cp -R "$APP_PATH" "$DEST"`。
# 那个 `|| true` 把删除失败整个吞掉，于是权限不足时：
#   1. 旧 app 删不掉，还在原处
#   2. cp -R 撞上已存在的目录 ⇒ 变成 /Applications/x.app/x.app/（新版被塞进旧 app 里）
#   3. 5d 校验查的是 $DEST/Contents/MacOS/<name> —— 那是**旧**二进制，存在且可执行
#   4. 校验通过，脚本打印「=== 安装完成 ===」并以 rc=0 退出
# 也就是说：装了个寂寞，还报喜。2026-10-03 在沙箱里实测复现过。
#
# 临时名故意放在目标同目录：同一文件系统内的 rename(2)，代价接近零，
# 且任何一步失败，旧 app 都分毫不动。
STAGE="$INSTALL_DIR/.${APP_NAME}.app.incoming"
OLD="$INSTALL_DIR/.${APP_NAME}.app.previous"

elev rm -rf "$STAGE" "$OLD" 2>/dev/null || true
# 清不掉就必须停：STAGE 残留时 cp -R 会拷进去变成嵌套，正是上面那个事故。
if [ -e "$STAGE" ] || [ -e "$OLD" ]; then
  err "临时名清不掉（$STAGE / $OLD 可能仍在），不继续 —— 继续就会把新版拷进旧目录里"
  exit 1
fi

if ! elev cp -R "$APP_PATH" "$STAGE"; then
  elev rm -rf "$STAGE" 2>/dev/null || true
  err "复制新应用失败，旧应用未动（仍在 ${DEST}）"
  exit 1
fi
if [ -d "$DEST" ] && ! elev mv "$DEST" "$OLD"; then
  elev rm -rf "$STAGE" 2>/dev/null || true
  err "无法移走旧应用（权限?），旧应用未动（仍在 ${DEST}）"
  exit 1
fi
if ! elev mv "$STAGE" "$DEST"; then
  [ -d "$OLD" ] && elev mv "$OLD" "$DEST"        # 换名失败就把旧 app 搬回来
  elev rm -rf "$STAGE" 2>/dev/null || true
  err "替换失败，已回滚旧应用（仍在 ${DEST}）"
  exit 1
fi
elev rm -rf "$OLD" 2>/dev/null || true
# 收尾的清理不能当成"删掉了"的证明（PATH 上的 rm 可能被包装成可恢复删除，
# 它会打印 moved to trash 而目录还在）。留了东西就说出来，别闷声报 OK。
if [ -e "$OLD" ]; then
  warn "旧应用 $OLD 没能删掉，可手动清理"
fi
ok "已安装: $DEST"

# =============== 5d. 校验 ===============
# 换名式安装已经让"嵌套"不可能发生，但这里仍独立量一次：
# 校验位如果只看 "$DEST/Contents/MacOS/<name> 存在且可执行"，那么**旧** app 原封
# 不动时它照样通过 —— 那正是早先那个假成功的最后一道掩护。多查一个嵌套目录，
# 多一份不依赖上游假设的证据。
if [ -d "$DEST/${APP_NAME}.app" ]; then
  err "校验失败：新版被拷进了 $DEST/${APP_NAME}.app （嵌套），$DEST 本身仍是旧版"
  if [ -d "$BACKUP" ]; then
    warn "回滚到备份..."
    elev rm -rf "$DEST" 2>/dev/null || true
    elev cp -R "$BACKUP" "$DEST"
  fi
  exit 1
fi
if [ ! -x "$DEST/Contents/MacOS/${APP_NAME}" ]; then
  err "校验失败：未找到 $DEST/Contents/MacOS/${APP_NAME}"
  if [ -d "$BACKUP" ]; then
    warn "回滚到备份..."
    elev rm -rf "$DEST" 2>/dev/null || true
    elev cp -R "$BACKUP" "$DEST"
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
  echo "回滚命令: rm -rf '$DEST' && cp -R '$BACKUP' '$DEST'"
fi
