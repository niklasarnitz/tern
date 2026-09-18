#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."
bash scripts/generate-bindings.sh
swift run --package-path apple Tern
