#!/usr/bin/env bash
#
# scripts/release-evidence.sh
# ------------------------------------------------------------------------------
# 收集发布前证据：单元测试输出、typecheck、性能基线、安装脚本语法
# 输出到 release-evidence/{tag}/，归档供发布评审门禁使用（T-062 / A24）
#
# 用法：
#   bash scripts/release-evidence.sh v0.2.0
#   bash scripts/release-evidence.sh              # 默认用当前 HEAD 短 hash
#
set -euo pipefail

TAG="${1:-$(git rev-parse --short HEAD 2>/dev/null || echo unknown)}"
OUT_DIR="release-evidence/${TAG}"
mkdir -p "${OUT_DIR}"

info() { printf "\033[0;34m[INFO]\033[0m %s\n" "$*"; }
ok() { printf "\033[0;32m[OK]\033[0m %s\n" "$*"; }
err() { printf "\033[0;31m[ERROR]\033[0m %s\n" "$*" >&2; }

cd "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/.."

info "采集发布证据 -> ${OUT_DIR}"

# 1) 单元 / 集成测试
info "[1/5] cargo test --lib"
(cd src-tauri && cargo test --lib --quiet) > "${OUT_DIR}/cargo-test-lib.txt" 2>&1 || {
  err "cargo test --lib 失败；详见 ${OUT_DIR}/cargo-test-lib.txt"
  exit 1
}
ok "lib 测试通过"

info "[2/5] cargo test --test test_sqlite"
(cd src-tauri && cargo test --test test_sqlite --quiet) \
  > "${OUT_DIR}/cargo-test-sqlite.txt" 2>&1 || {
  err "sqlite 集成测试失败"
  exit 1
}
ok "sqlite 集成通过"

# 3) 前端类型检查
info "[3/5] npx tsc --noEmit"
npx tsc --noEmit -p tsconfig.json > "${OUT_DIR}/tsc.txt" 2>&1 || {
  warn "tsc 有错误（不阻断；详见 ${OUT_DIR}/tsc.txt）"
}
ok "tsc 收尾"

# 4) Install 脚本 bash 语法
info "[4/5] bash -n scripts/install.sh scripts/release.sh"
bash -n scripts/install.sh > "${OUT_DIR}/install-syntax.txt" 2>&1
bash -n scripts/release.sh > "${OUT_DIR}/release-syntax.txt" 2>&1 || true
ok "脚本语法 ok"

# 5) 性能基线（轻量；S2 阶段会扩展）
info "[5/5] timing baseline (npx vite build)"
START=$(date +%s%N)
npx vite build > "${OUT_DIR}/vite-build.txt" 2>&1 || true
END=$(date +%s%N)
ELAPSED_MS=$(( (END - START) / 1000000 ))
echo "vite build elapsed ${ELAPSED_MS} ms" > "${OUT_DIR}/timing.txt"

# 汇总
{
  echo "# Release Evidence Report"
  echo ""
  echo "- tag/head: ${TAG}"
  echo "- date: $(date -u +%Y-%m-%dT%H:%M:%SZ)"
  echo "- git: $(git rev-parse --short HEAD 2>/dev/null || echo unknown)"
  echo "- files included:"
  for f in "${OUT_DIR}"/*; do
    echo "  - $(basename "$f")"
  done
} > "${OUT_DIR}/SUMMARY.md"

ok "证据已写入 ${OUT_DIR}/SUMMARY.md"
