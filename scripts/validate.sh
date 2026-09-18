#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."
bash scripts/check-rust.sh
if [[ "$(uname -s)" == Darwin ]]; then
  bash scripts/check-swift.sh
fi
