#!/usr/bin/env bash
#
# scripts/release.sh
# ------------------------------------------------------------------------------
# 把版本号 bump 一档（默认 minor、patch 归零），同步写入以下 version 字段，
# 跑发布前置门禁，然后提交、push main、打 vX.Y.Z tag 并 push，触发 GitHub Actions 打包。
#
# 受影响的文件:
#   - package.json            (必需: 版本号从这里读；缺失直接报错退出)
#   - src-tauri/Cargo.toml        (缺失会跳过, 不会报错)
#   - src-tauri/tauri.conf.json   (缺失会跳过, 不会报错)
#
# 发布前置门禁（缺哪步跳哪步，2026-09-24 从 db 的 T-059 上收）:
#   1) package.json 有 "typecheck" script -> npm run typecheck
#   2) 存在 src-tauri/                    -> cargo test --lib
#   3) 存在 scripts/release-gate.sh       -> 执行它（本仓特异门禁放这里，不进模板）
#   4) 门禁跑完复查工作区干净（测试会改写 lockfile）
#
# 用法:
#   bash scripts/release.sh                 # 交互式, 每一步确认, 默认 minor
#   bash scripts/release.sh --bump=patch    # 只 +patch   (0.2.0 -> 0.2.1)
#   bash scripts/release.sh --bump=major    # 升主版本    (0.2.0 -> 1.0.0)
#   bash scripts/release.sh --yes           # 全程不询问
#   bash scripts/release.sh --dry-run       # 只打印计划, 不动文件 / git, 门禁也只打印不执行
#   bash scripts/release.sh --help
#
# 版本号不接受位置参数（由本脚本按 --bump 档算），要指定档位用 --bump=。
# 版本基准取 max(package.json, 最高的 v* tag)，两者不一致时会警告并要确认——
#      note 就是文件停在 0.2.0、tag 已经发到 v1.0.0 的活例（2026-09-24 实测）。
# 注意：绕过本脚本手工改版本会造出"tag 与三处版本不一致"的发布，
#      cpu 的 v0.1.2 就是这么来的（2026-09-24 追查）。
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
BUMP=minor
for arg in "$@"; do
  case "$arg" in
    --dry-run)     DRY_RUN=1 ;;
    --yes|-y)      ASSUME_YES=1 ;;
    --bump=patch|--bump=minor|--bump=major) BUMP="${arg#--bump=}" ;;
    --bump=*)      err "无效的 --bump 值: ${arg#--bump=} (可选 patch|minor|major)"; exit 2 ;;
    -h|--help)
      awk 'NR>1 { if ($0 !~ /^#/) exit; sub(/^# ?/, ""); print }' "$0"
      exit 0
      ;;
    *) err "未知参数: $arg  (版本号不接受位置参数, 用 --bump=patch|minor|major)"; exit 2 ;;
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

gate() {
  # 门禁命令都带 cd, 用 bash -c 把目录切换隔离在子 shell 里; $1 始终是脚本内字面量
  if [ $DRY_RUN -eq 1 ]; then
    printf "${C_YELLOW}[dry-run]${C_RESET} %s\n" "$1"
  else
    info "$1"
    bash -c "$1"
  fi
}

confirm() {
  local prompt="$1"
  if [ $DRY_RUN -eq 1 ]; then
    printf "${C_YELLOW}[dry-run]${C_RESET} (不询问) %s\n" "$prompt"
    return 0
  fi
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

PKG_JSON="package.json"
if [ ! -f "$PKG_JSON" ]; then
  err "找不到 $PKG_JSON, 请在项目根目录运行此脚本"; exit 1
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

PKG_JSON="package.json"
if [ ! -f "$PKG_JSON" ]; then
  err "找不到 $PKG_JSON, 请在项目根目录运行此脚本"; exit 1
fi

# ---------- 读当前版本 ----------
FILE_VERSION="$(python3 -c "
import json, sys
with open('$PKG_JSON') as f:
    print(json.load(f)['version'])
")"
ok "文件版本: $FILE_VERSION"

# 基准 = max(文件版本, 最高的 v* tag)。两者可以不一致: note 的 v1.0.0 早在 2026-08-02
# 就发出去了, 三处文件却被改回 0.2.0 —— 只按文件算会造出比已发布版本更低的发布。
BASE_VERSION="$(python3 -c "
import re, subprocess, sys
def key(v): return [int(x) for x in re.findall(r'\d+', v)[:3]]
tags = subprocess.run(['git', 'tag', '-l', 'v*'], capture_output=True, text=True).stdout.split()
vers = [sys.argv[1]] + [t[1:] for t in tags if re.match(r'^v\d+\.\d+\.\d+$', t)]
print(max(vers, key=key))
" "$FILE_VERSION")"
if [ "$BASE_VERSION" != "$FILE_VERSION" ]; then
  warn "已发布的最高 tag 是 v$BASE_VERSION, 与文件版本 $FILE_VERSION 不一致, 以 v$BASE_VERSION 为基准"
  confirm "确认按 v$BASE_VERSION 继续?" || exit 1
fi

# ---------- 计算新版本 ----------
IFS='.' read -r MAJOR MINOR PATCH <<<"$BASE_VERSION"
if [ -z "$MAJOR" ] || [ -z "$MINOR" ] || [ -z "$PATCH" ]; then
  err "无法解析版本号 '$BASE_VERSION', 期望 MAJOR.MINOR.PATCH"; exit 1
fi
case "$BUMP" in
  patch) NEW_VERSION="$MAJOR.$MINOR.$((PATCH + 1))" ;;
  minor) NEW_VERSION="$MAJOR.$((MINOR + 1)).0" ;;
  major) NEW_VERSION="$((MAJOR + 1)).0.0" ;;
esac
NEW_TAG="v$NEW_VERSION"
ok "新版本: $NEW_VERSION  (bump: $BUMP, tag: $NEW_TAG)"

# tag 冲突放在门禁之前: 花两分钟编译测试之后才发现要发的版本已经发过，太亏
if git rev-parse "$NEW_TAG" >/dev/null 2>&1; then
  err "tag $NEW_TAG 已经存在, 请先删除 (git tag -d $NEW_TAG && git push origin :refs/tags/$NEW_TAG)"
  exit 1
fi

# ---------- 发布前置门禁 ----------
# 缺哪步跳哪步: 不是所有仓都有 tauri, 也不是所有仓都定义了 typecheck。
# 本仓特异的测试写进 scripts/release-gate.sh, 不要往模板里塞。
info "发布前置门禁 [$PROJECT_NAME]"
GATE_RAN=0

if grep -q '"typecheck"' "$PKG_JSON"; then
  GATE_RAN=1
  gate 'npm run typecheck'
fi

if [ -d src-tauri ]; then
  GATE_RAN=1
  gate 'cd src-tauri && cargo test --lib'
fi

if [ -f scripts/release-gate.sh ]; then
  GATE_RAN=1
  gate 'bash scripts/release-gate.sh'
fi

if [ $GATE_RAN -eq 0 ]; then
  warn "没有任何门禁可跑 (无 typecheck script / 无 src-tauri / 无 release-gate.sh)"
  confirm "跳过门禁继续发布?" || exit 1
fi

# 测试可能改写 lockfile: 门禁后复查, 否则下一步的 git add 会把脏改动一起提交
if [ $DRY_RUN -eq 0 ] && [ -n "$(git status --porcelain)" ]; then
  err "门禁执行后工作区变脏 (测试可能改写了 lockfile), 请检查后重新提交"
  git status --short
  exit 1
fi
ok "门禁通过"

# ---------- 更新文件 (缺失则跳过) ----------
BUMPED_FILES=""

bump_file() {
  local file="$1" new_ver="$2"
  if [ ! -f "$file" ]; then
    warn "跳过 (文件不存在): $file"
    return 0
  fi
  # 记录实际改过的文件: git add 只加这些, 写死路径会在缺文件的仓上 fatal: pathspec did not match
  BUMPED_FILES="$BUMPED_FILES $file"
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
  1. 同步 version 到 $NEW_VERSION ($BUMP bump):$BUMPED_FILES
  2. git commit -m "chore(release): bump version to $NEW_VERSION"
  3. git push origin $CURRENT_BRANCH
  4. git tag $NEW_TAG
  5. git push origin $NEW_TAG   -> 触发 GitHub Actions: $ACTIONS_URL
EOF

if [ $DRY_RUN -eq 1 ]; then
  ok "[dry-run] 计划已打印, 未改动任何文件 / git"
  exit 0
fi

confirm "确认执行?" || { err "已取消"; exit 1; }

# ---------- 执行 ----------
run git add $BUMPED_FILES
run git commit -m "chore(release): bump version to $NEW_VERSION"
run git push origin "$CURRENT_BRANCH"
run git tag "$NEW_TAG"
run git push origin "$NEW_TAG"

ok "发布完成! GitHub Actions 进度: $ACTIONS_URL"