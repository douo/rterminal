#!/usr/bin/env bash

set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
APP_NAME="${APP_NAME:-Agent Terminal}"
APP_BUNDLE_NAME="${APP_NAME}.app"
APP_ID="${APP_ID:-local.agent-terminal}"
PROFILE="${PROFILE:-release}"
TARGET_DIR="${TARGET_DIR:-$ROOT_DIR/target}"
DIST_DIR="${DIST_DIR:-$ROOT_DIR/dist}"
APP_DIR="$DIST_DIR/$APP_BUNDLE_NAME"
CONTENTS_DIR="$APP_DIR/Contents"
MACOS_DIR="$CONTENTS_DIR/MacOS"
RESOURCES_DIR="$CONTENTS_DIR/Resources"
BIN_NAME="agent_terminal"
BIN_PATH="$TARGET_DIR/$PROFILE/$BIN_NAME"
WRAPPER_NAME="$BIN_NAME"
REAL_BIN_NAME="${BIN_NAME}-bin"
VERSION="$(
    awk -F ' = ' '
        $1 == "version" {
            gsub(/"/, "", $2);
            print $2;
            exit
        }
    ' "$ROOT_DIR/Cargo.toml"
)"

if [[ -z "$VERSION" ]]; then
    echo "failed to read version from Cargo.toml" >&2
    exit 1
fi

echo "Building $BIN_NAME ($PROFILE)..."
if [[ "$PROFILE" == "release" ]]; then
    cargo build --manifest-path "$ROOT_DIR/Cargo.toml" --release
else
    cargo build --manifest-path "$ROOT_DIR/Cargo.toml" --profile "$PROFILE"
fi

rm -rf "$APP_DIR"
mkdir -p "$MACOS_DIR" "$RESOURCES_DIR"

cp "$BIN_PATH" "$MACOS_DIR/$BIN_NAME"
mv "$MACOS_DIR/$BIN_NAME" "$MACOS_DIR/$REAL_BIN_NAME"

cat > "$MACOS_DIR/$WRAPPER_NAME" <<'WRAPPER'
#!/bin/sh
set -eu

TERM=xterm-256color
COLORTERM=truecolor
# 与 src/pty.rs 保持一致（ENG-11）：pty.rs 对每个 shell 都会设 TERM_PROGRAM=rterminal，
# 这里若写别的名字，下游工具的探测结果就会随启动方式变化。
TERM_PROGRAM=rterminal
: "${LANG:=en_US.UTF-8}"
: "${LC_CTYPE:=en_US.UTF-8}"

export TERM
export COLORTERM
export TERM_PROGRAM
export LANG
export LC_CTYPE

SCRIPT_DIR="$(CDPATH= cd -- "$(dirname "$0")" && pwd)"
exec "$SCRIPT_DIR/agent_terminal-bin" "$@"
WRAPPER

chmod +x "$MACOS_DIR/$WRAPPER_NAME" "$MACOS_DIR/$REAL_BIN_NAME"

cat > "$CONTENTS_DIR/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleDevelopmentRegion</key>
  <string>en</string>
  <key>CFBundleExecutable</key>
  <string>${WRAPPER_NAME}</string>
  <key>CFBundleIdentifier</key>
  <string>${APP_ID}</string>
  <key>CFBundleInfoDictionaryVersion</key>
  <string>6.0</string>
  <key>CFBundleName</key>
  <string>${APP_NAME}</string>
  <key>CFBundlePackageType</key>
  <string>APPL</string>
  <key>CFBundleShortVersionString</key>
  <string>${VERSION}</string>
  <key>CFBundleVersion</key>
  <string>${VERSION}</string>
  <key>LSMinimumSystemVersion</key>
  <string>13.0</string>
  <key>NSHighResolutionCapable</key>
  <true/>
</dict>
</plist>
PLIST

# 签名（ENG-11）。本应用需要辅助功能（AX）权限，而 TCC 按代码签名记住授权：
# - 设置 CODESIGN_IDENTITY 为真实证书（如 "Apple Development: ..."）时，签名的
#   designated requirement 稳定，重建后 AX 授权保留；
# - 未设置时退化为 ad-hoc 签名（`-`）：签名合法但 CDHash 每次构建都变，
#   重建后仍需重新授权 AX——这是没有证书时的已知限制，不是 bug。
CODESIGN_IDENTITY="${CODESIGN_IDENTITY:--}"
echo "Codesigning with identity: $CODESIGN_IDENTITY"
codesign --force --deep --sign "$CODESIGN_IDENTITY" --identifier "$APP_ID" "$APP_DIR"

echo "Built app bundle:"
echo "  $APP_DIR"
