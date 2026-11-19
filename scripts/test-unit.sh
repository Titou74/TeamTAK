#!/usr/bin/env bash
# Unit tests and static checks. Needs no running service.
#   - Rust: domain rules, reconciliation against in-memory backends, zips, e-mail texts,
#     and the check that every API error code is translated in every language;
#   - web: translation files (same keys in every language), no hard-coded text, types.
set -euo pipefail
cd "$(dirname "$0")/.."
command -v cargo > /dev/null || export PATH="/opt/homebrew/opt/rustup/bin:$PATH"

echo "== Rust unit tests"
cargo test --workspace

echo
echo "== Web checks"
cd web
[ -d node_modules ] || pnpm install --frozen-lockfile
pnpm test
