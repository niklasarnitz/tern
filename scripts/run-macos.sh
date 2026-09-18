#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."
bash scripts/generate-bindings.sh
source scripts/apple-toolchain.sh
swift build --package-path apple --product Tern

binary_directory=$(swift build --package-path apple --show-bin-path)
app_bundle="$PWD/apple/.build/Tern.app"
contents="$app_bundle/Contents"
rm -rf "$app_bundle"
mkdir -p "$contents/MacOS"
cp "$binary_directory/Tern" "$contents/MacOS/Tern"
cp apple/Resources/Info.plist "$contents/Info.plist"

# Register the development bundle so macOS can offer Tern as a mail handler.
launch_services_register="/System/Library/Frameworks/CoreServices.framework/Frameworks/LaunchServices.framework/Support/lsregister"
"$launch_services_register" -f "$app_bundle"
exec "$contents/MacOS/Tern"
