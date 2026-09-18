#!/usr/bin/env bash
# Source this only for Apple tooling. Nix's SDK environment contains no Swift
# toolchain; SourceKit and SwiftPM must use the user's selected full Xcode.
unset DEVELOPER_DIR SDKROOT
export DEVELOPER_DIR
DEVELOPER_DIR=$(/usr/bin/xcode-select --print-path)
export SDKROOT
SDKROOT=$(/usr/bin/xcrun --sdk macosx --show-sdk-path)
export PATH="$DEVELOPER_DIR/Toolchains/XcodeDefault.xctoolchain/usr/bin:/usr/bin:$PATH"
