#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."
swiftlint lint --strict --config .swiftlint.yml
swiftformat --lint apple/Sources apple/Package.swift --config .swiftformat
bash scripts/generate-bindings.sh
mkdir -p .devbox/checks
# Analysis needs actual compiler invocations, so do not reuse an incremental build log.
swift package --package-path apple clean
swift build --package-path apple -v -Xswiftc -warnings-as-errors 2>&1 | tee .devbox/checks/swift-build.log
swiftlint analyze --strict --config .swiftlint.yml --compiler-log-path .devbox/checks/swift-build.log
