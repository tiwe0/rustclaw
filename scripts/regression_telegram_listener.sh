#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT_DIR"

echo "[regression] checking build"
cargo check

echo "[regression] running telegram listener config tests"
cargo test normalized_ -- --nocapture

echo "[regression] done"
