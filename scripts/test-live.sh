#!/bin/sh
# Start loopback camera fixtures, run hardware tests, and stop only these fixtures.
set -eu
cd "$(dirname "$0")/.."
MEDIAMTX=${MEDIAMTX:-mediamtx}
fixture_dir=$(mktemp -d)
fixture_pids=""

cleanup() {
    result=$?
    trap - EXIT
    if [ "$result" -ne 0 ]; then
        for log in "$fixture_dir"/*.log; do
            printf '\n%s\n' "$log"
            tail -20 "$log"
        done
    fi
    for pid in $fixture_pids; do kill "$pid" 2>/dev/null || true; done
    for pid in $fixture_pids; do wait "$pid" 2>/dev/null || true; done
    rm -rf "$fixture_dir"
    exit "$result"
}
trap cleanup EXIT
trap 'exit 130' INT TERM

cat > "$fixture_dir/server.yml" <<'YAML'
rtspAddress: 127.0.0.1:18554
rtspTransports: [tcp]
rtmpAddress: 127.0.0.1:11935
hls: no
webrtc: no
srt: no
moq: no
paths:
  all_others:
YAML

"$MEDIAMTX" "$fixture_dir/server.yml" > "$fixture_dir/server.log" 2>&1 &
fixture_pids="$fixture_pids $!"
python3 Tests/CamCamTests/Fixtures/onvif.py > "$fixture_dir/onvif.log" 2>&1 &
fixture_pids="$fixture_pids $!"
sleep 1
# A bind failure must fail the run instead of testing an existing service.
for pid in $fixture_pids; do kill -0 "$pid"; done

ffmpeg -nostdin -hide_banner -loglevel error -re -f lavfi -i testsrc2=size=640x360:rate=20 \
    -an -c:v libx264 -preset ultrafast -tune zerolatency -g 20 \
    -f flv rtmp://127.0.0.1:11935/workshop > "$fixture_dir/rtmp.log" 2>&1 &
fixture_pids="$fixture_pids $!"
ffmpeg -nostdin -hide_banner -loglevel error -re -f lavfi -i testsrc2=size=1280x720:rate=20 \
    -an -c:v libx264 -preset ultrafast -tune zerolatency -g 20 \
    -rtsp_transport tcp -f rtsp rtsp://127.0.0.1:18554/main > "$fixture_dir/main.log" 2>&1 &
fixture_pids="$fixture_pids $!"
ffmpeg -nostdin -hide_banner -loglevel error -re -f lavfi -i smptebars=size=480x270:rate=12 \
    -an -c:v libx264 -preset ultrafast -tune zerolatency -g 12 \
    -rtsp_transport tcp -f rtsp rtsp://127.0.0.1:18554/sub > "$fixture_dir/sub.log" 2>&1 &
fixture_pids="$fixture_pids $!"
sleep 3
for pid in $fixture_pids; do kill -0 "$pid"; done

CAMCAM_TEST_FEEDS=1 swift test --disable-xctest "$@"
