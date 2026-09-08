#!/usr/bin/env bash
#
# scripts/release.sh
# ------------------------------------------------------------------------------
# 把当前仓库的 minor 版本号 +1 (patch 归零), 同步写入以下三处 version 字段,
# 然后提交、push main、打 v{version} tag 并 push, 触发 GitHub Actions 打包。
#
# 受影响的文件 (缺失会跳过, 不会报错):
#   - package.json
#   - src-tauri/Cargo.toml
#   - src-tauri/tauri.conf.json
#
# 用法:
#   bash scripts/release.sh           # 交互式, 每一步确认
#   bash scripts/release.sh --yes     # 全程不询问
#   bash scripts/release.sh --dry-run # 只打印计划, 不动任何文件 / git
#   bash scripts/release.sh --help
#
# 前置条件:
#   - 在项目根目录执行
#   - 当前在 git 仓库内, 默认分支是 main
#   - 工作区干净 (没有未提交的改动)
#   - 已配置 origin 指向 GitHub
# ------------------------------------------------------------------------------

set -euo pipefail

PROJECT_NAME="$(basename "$(pwd)")"

# ---------- 颜色 ----------
if [ -t 1 ]; then
  C_RESET=$'\033[0m'; C_BOLD=$'\033[1m'
  C_BLUE=$'\033[34m'; C_GREEN=$'\033[32m'
  C_YELLOW=$'\033[33m'; C_RED=$'\033[31m'
else
  C_RESET=""; C_BOLD=""; C_BLUE=""; C_GREEN=""; C_YELLOW=""; C_RED=""
fi

info()  { printf "${C_BLUE}==>${C_RESET} ${C_BOLD}%s${C_RESET}\n" "$*"; }
ok()    { printf "${C_GREEN}✓ %s${C_RESET}\n" "$*"; }
warn()  { printf "${C_YELLOW}! %s${C_RESET}\n" "$*"; }
err()   { printf "${C_RED}✗ %s${C_RESET}\n" "$*" >&2; }

# ---------- 参数 ----------
DRY_RUN=0
ASSUME_YES=0
for arg in "$@"; do
  case "$arg" in
    --dry-run) DRY_RUN=1 ;;
    --yes|-y)  ASSUME_YES=1 ;;
    -h|--help)
      sed -n '2,28p' "$0"
      exit 0
      ;;
    *) err "未知参数: $arg"; exit 2 ;;
  esac
done

run() {
  if [ $DRY_RUN -eq 1 ]; then
    printf "${C_YELLOW}[dry-run]${C_RESET} %s\n" "$*"
  else
    info "$*"
    "$@"
  fi
}

confirm() {
  local prompt="$1"
  if [ $ASSUME_YES -eq 1 ]; then return 0; fi
  printf "${C_BOLD}%s${C_RESET} [y/N] " "$prompt"
  read -r ans
  [ "$ans" = "y" ] || [ "$ans" = "Y" ]
}

# ---------- 预检 ----------
info "预检 [$PROJECT_NAME]"

if ! git rev-parse --is-inside-work-tree >/dev/null 2>&1; then
  err "当前目录不是 git 仓库"; exit 1
fi

if [ -n "$(git status --porcelain)" ]; then
  err "工作区不干净, 请先 commit 或 stash 所有改动"
  git status --short
  exit 1
fi

if ! command -v python3 >/dev/null 2>&1; then
  err "需要 python3 (用于就地改 version 字段)"
  exit 1
fi

CURRENT_BRANCH="$(git symbolic-ref --short HEAD 2>/dev/null || true)"
if [ -z "$CURRENT_BRANCH" ]; then
  err "当前处于 detached HEAD 状态, 请先切到分支"; exit 1
fi
if [ "$CURRENT_BRANCH" != "main" ]; then
  warn "当前分支是 $CURRENT_BRANCH, 不是 main"
  confirm "继续在 $CURRENT_BRANCH 上发布?" || exit 1
fi

if ! git remote get-url origin >/dev/null 2>&1; then
  err "没有配置 origin 远程仓库"; exit 1
fi

# ---------- 读当前版本 ----------
PKG_JSON="package.json"
if [ ! -f "$PKG_JSON" ]; then
  err "找不到 $PKG_JSON, 请在项目根目录运行此脚本"; exit 1
fi

CURRENT_VERSION="$(python3 -c "
import json, sys
with open('$PKG_JSON') as f:
    print(json.load(f)['version'])
")"
ok "当前版本: $CURRENT_VERSION"

# ---------- 计算新版本 (minor +1, patch 归零) ----------
IFS='.' read -r MAJOR MINOR PATCH <<<"$CURRENT_VERSION"
if [ -z "$MAJOR" ] || [ -z "$MINOR" ]; then
  err "无法解析版本号 '$CURRENT_VERSION', 期望 MAJOR.MINOR.PATCH"; exit 1
fi
NEW_VERSION="$MAJOR.$((MINOR + 1)).0"
NEW_TAG="v$NEW_VERSION"
ok "新版本: $NEW_VERSION  (tag: $NEW_TAG)"

if git rev-parse "$NEW_TAG" >/dev/null 2>&1; then
  err "tag $NEW_TAG 已经存在, 请先删除 (git tag -d $NEW_TAG && git push origin :refs/tags/$NEW_TAG)"
  exit 1
fi

# ---------- 更新文件 (缺失则跳过) ----------
bump_file() {
  local file="$1" new_ver="$2"
  if [ ! -f "$file" ]; then
    warn "跳过 (文件不存在): $file"
    return 0
  fi
  if [ $DRY_RUN -eq 1 ]; then
    printf "${C_YELLOW}[dry-run]${C_RESET} %s -> %s\n" "$file" "$new_ver"
    return 0
  fi
  python3 - "$file" "$new_ver" <<'PY'
import re, sys
path, new_ver = sys.argv[1], sys.argv[2]
with open(path, 'r', encoding='utf-8') as f:
    content = f.read()
# (regex, replacement, flags)
patterns = [
    (r'("version"\s*:\s*)"[^"]+"',  rf'\g<1>"{new_ver}"', 0),             # JSON
    (r'^(version\s*=\s*)"[^"]+"',    rf'\g<1>"{new_ver}"', re.MULTILINE),  # TOML
]
for pat, repl, flags in patterns:
    new, n = re.subn(pat, repl, content, count=1, flags=flags)
    if n:
        content = new
        break
else:
    sys.exit(f"未在 {path} 中找到 version 字段")
with open(path, 'w', encoding='utf-8') as f:
    f.write(content)
PY
  ok "更新 $file -> $new_ver"
}

bump_file "package.json"                 "$NEW_VERSION"
bump_file "src-tauri/Cargo.toml"         "$NEW_VERSION"
bump_file "src-tauri/tauri.conf.json"    "$NEW_VERSION"

# ---------- 总结计划 ----------
REMOTE_URL="$(git remote get-url origin)"
REPO_URL="$(echo "$REMOTE_URL" | sed -E 's#^(git@|https://)github.com[:/]+##; s#\.git$##')"
ACTIONS_URL="https://github.com/$REPO_URL/actions"

info "即将执行:"
cat <<EOF
  1. 同步 version 到 $NEW_VERSION (package.json / Cargo.toml / tauri.conf.json)
  2. git commit -m "chore(release): bump version to $NEW_VERSION"
  3. git push origin $CURRENT_BRANCH
  4. git tag $NEW_TAG
  5. git push origin $NEW_TAG   -> 触发 GitHub Actions: $ACTIONS_URL
EOF

confirm "确认执行?" || { err "已取消"; exit 1; }

# ---------- 执行 ----------
run git add package.json src-tauri/Cargo.toml src-tauri/tauri.conf.json
run git commit -m "chore(release): bump version to $NEW_VERSION"
run git push origin "$CURRENT_BRANCH"
run git tag "$NEW_TAG"
run git push origin "$NEW_TAG"

ok "发布完成! GitHub Actions 进度: $ACTIONS_URL"