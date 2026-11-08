#!/bin/sh
set -eu
cd "$(dirname "$0")/.."

open_after=0
install_after=0
parse_options=1
remaining=$#
# Rotate Cargo arguments to the end, keeping their order and quoting intact.
while [ "$remaining" -gt 0 ]; do
  argument=$1
  shift
  remaining=$((remaining - 1))
  if [ "$parse_options" = 0 ]; then
    set -- "$@" "$argument"
    continue
  fi
  case "$argument" in
    --open) open_after=1 ;;
    --install) install_after=1 ;;
    --) parse_options=0 ;;
    -h|--help)
      printf '%s\n' 'Usage: build-app.sh [--open] [--install] [CARGO BUILD OPTIONS]' \
        '  --open     Launch the built app, or the installed copy with --install.' \
        '  --install  Replace /Applications/CamCam.app with the built app.' \
        '  --         Pass all following arguments to Cargo.'
      exit 0
      ;;
    *) set -- "$@" "$argument" ;;
  esac
done

cargo build --release "$@"
binary_dir="${CARGO_TARGET_DIR:-target}/release"
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

if [ "$install_after" = 1 ]; then
  destination="/Applications/CamCam.app"
  rm -rf "$destination"
  ditto "$bundle" "$destination"
  bundle=$destination
  printf 'Installed %s\n' "$bundle"
fi

if [ "$open_after" = 1 ]; then
  open "$bundle"
fi
