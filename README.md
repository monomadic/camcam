# CamCam

A small macOS camera wall. Click a camera to focus it. Click again or press Esc to return.

- RTMP playback and direct RTSP streams.
- ONVIF Media1 and Media2 at configured device-service addresses.
- Lower-resolution ONVIF profile for the wall, highest-resolution profile for focus.
- Independent connections, bounded frame delivery, and reconnects with exponential backoff.
- H.264 and HEVC through VideoToolbox, with native pixel-buffer presentation.

The first implementation uses Swift/AppKit, FFmpeg network input, and Apple's video display
layers. Switchblade's dark grid and Quickview are the interaction reference. This keeps the
macOS shell small and lets decoded pixel buffers go directly to the system compositor.
The saved [proposal](PROPOSAL.md) records the initial Rust/wgpu direction.

## Build and run

Requires macOS 14+, Swift 6, pkg-config, and FFmpeg development libraries. The installed
FFmpeg and its dependencies may require a newer macOS version. This machine's Homebrew
FFmpeg requires macOS 26.

```sh
brew install ffmpeg pkg-config
swift build -c release
swift run -c release camcam
```

For a local Finder-launchable app, run `./scripts/build-app.sh` and open
`.build/CamCam.app`. The app links to the installed Homebrew libraries. Keep those available
when running it.

## Configuration

Copy [config.example.toml](config.example.toml) to `~/.config/camcam/config.toml`, edit the
addresses, then launch CamCam. Entries appear in file order. Config changes apply on relaunch.

```sh
mkdir -p ~/.config/camcam
cp config.example.toml ~/.config/camcam/config.toml
export CAMCAM_FRONT_DOOR_PASSWORD='your-camera-password'
swift run -c release camcam
```

Use `camcam --config /path/to/config.toml` to choose another file. Password environment
variables must be available to the launched process. For a Finder app launched from a shell,
`open --env CAMCAM_FRONT_DOOR_PASSWORD=... .build/CamCam.app` passes the variable explicitly.
Each camera may use `username` and `password_env`. Existing URL credentials take precedence.

ONVIF entries take a device-service URL, typically `http://camera/onvif/device_service`.
CamCam reads services, profiles, and stream URIs. It supports WS-Security password digest
and HTTP Digest authentication. HTTP Basic authentication is accepted over HTTPS.
Camera clock skew can cause WS-Security authentication to fail.

Only same-host HTTP(S) media-service addresses are accepted. Configured credentials are
added to discovered RTSP URLs only when the stream host matches the device host. A camera
advertising an unusable address can be configured directly using a working RTSP URL.

RTMP entries pull from a playback endpoint. A camera that publishes RTMP needs a receiver
such as MediaMTX, whose playback URL can then be added to CamCam.

## Playback behavior

Every tile connects independently. Hover a failing tile for its connection-stage error.
A stalled connection times out, then retries after 1, 2, 4, 8, 16, and 30 seconds. The
backoff resets after a decoded frame. Initial connection and stream analysis have a
12-second deadline. A live feed without decoded frames times out after 8 seconds.

Focus starts an additional main-stream connection when a larger ONVIF profile is
available. The wall feed stays visible until the main feed produces a frame. If the main
stream fails, the wall feed remains available. Returning to the wall closes the additional
connection. Hidden wall feeds keep decoding so returning is immediate.

The decoder waits for a keyframe when joining a live stream. The UI mailbox retains at most
one pending pixel buffer per stream. Frames are presented immediately, with aspect ratio
preserved. Audio is currently silent.

The `LIVE · VT` badge confirms decoded VideoToolbox pixel buffers. CamCam accepts H.264 and
HEVC and declines FFmpeg software pixel formats. FFmpeg requires hardware decoding for
H.264. Its HEVC VideoToolbox path enables hardware acceleration but can use Apple's software
decoder on unsupported hardware. HEVC hardware use needs validation on the target machine.

## Checks

`swift test --disable-xctest` checks config loading, credential escaping, and invalid camera settings.
The opt-in live test checks Media1/Media2 profile selection and repeatedly joins RTMP and
RTSP streams through VideoToolbox, including joins between keyframes.

To reproduce the live test, install MediaMTX and run the fixture script. It starts
loopback-only RTMP, RTSP, and ONVIF services, publishes three synthetic feeds, runs the
tests, and stops its fixture processes. Ports 11935, 18554, and 18080 must be available.
Run it with access to the macOS video hardware.

```sh
brew install mediamtx
./scripts/test-live.sh
# Or use a separately downloaded official MediaMTX executable.
MEDIAMTX=/path/to/mediamtx ./scripts/test-live.sh
```

These are synthetic local fixtures. Physical-camera compatibility, HEVC, and sustained
load need measurement on the intended cameras and Mac.
