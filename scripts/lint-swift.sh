#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."
source scripts/apple-toolchain.sh
swiftlint lint --strict --config .swiftlint.yml
swiftformat --lint apple/Sources apple/Tests apple/Package.swift --config .swiftformat
