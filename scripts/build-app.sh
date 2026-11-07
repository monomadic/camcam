#!/bin/sh
set -eu
cd "$(dirname "$0")/.."

swift build -c release "$@"
binary_dir=$(swift build -c release --show-bin-path "$@")
bundle="$PWD/.build/CamCam.app"
mkdir -p "$bundle/Contents/MacOS"
cp "$binary_dir/camcam" "$bundle/Contents/MacOS/camcam"

cat > "$bundle/Contents/Info.plist" <<'PLIST'
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
<key>CFBundleExecutable</key><string>camcam</string>
<key>CFBundleIdentifier</key><string>local.camcam.viewer</string>
<key>CFBundleName</key><string>CamCam</string>
<key>CFBundlePackageType</key><string>APPL</string>
<key>CFBundleShortVersionString</key><string>0.1</string>
<key>LSMinimumSystemVersion</key><string>14.0</string>
<key>NSHighResolutionCapable</key><true/>
</dict></plist>
PLIST
codesign --force --sign - "$bundle"
printf '\nBuilt %s\n' "$bundle"
