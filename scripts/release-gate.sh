#!/usr/bin/env bash
#
# scripts/release-gate.sh — z-biz-tool-db 特异发布门禁
# ------------------------------------------------------------------------------
# scripts/release.sh（lead 模板下发）已覆盖通用部分：
#   npm run typecheck + cargo test --lib
# 这里只放本仓独有的检查，被 release.sh 以 `bash scripts/release-gate.sh`
# 从仓库根目录调用；退出码非 0 直接阻断发布。
#
# 单独跑： bash scripts/release-gate.sh
# ------------------------------------------------------------------------------
set -euo pipefail

cd "$(dirname "$0")/.."

# SQLite 集成测试：db 的核心面是读写真实库，lib 单测覆盖不到
(
  cd src-tauri
  cargo test --test test_sqlite
)

echo "✓ db 特异门禁通过"
