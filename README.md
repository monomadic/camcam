# CamCam

A small Rust/macOS camera wall. Click a camera to focus it. Click again or press Esc to return.

- RTMP playback and direct RTSP streams.
- ONVIF Media1 and Media2 at configured device-service addresses.
- Smaller ONVIF profile for the wall, larger profile for focus.
- Independent connections, bounded frame delivery, and reconnects with exponential backoff.
- H.264 and HEVC through VideoToolbox, presented with wgpu on Metal.

## Architecture and Switchblade reference

CamCam follows Switchblade's winit/wgpu window and rendering architecture, dark video grid,
Quickview interaction, aspect-preserving video, and frame-driven redraws. The reference
implementation is in Switchblade's crates/sb-window/src/lib.rs, render.rs, and schedule.rs.
CamCam uses eframe/egui over winit 0.30 and wgpu 30 for its small UI.
It builds independently of the Switchblade checkout.

The app, config parser, ONVIF client, stream workers, decoder ownership, and GPU renderer
are Rust. FFmpeg provides live RTMP/RTSP demuxing through ffmpeg-sys-next.
VideoToolbox produces retained CVPixelBuffers. CoreVideo imports their NV12 or P010 planes
into Metal textures on wgpu's device. A GPU shader converts YUV to RGB, with video range
and BT.601/709/2020 matrix handling. Pixel buffers stay alive until in-flight GPU work
releases them. There is no CPU pixel readback or CPU color conversion in playback.

Each stream worker owns one replaceable frame slot and wakes the UI when it changes.
The window uses vsync with one queued surface frame and sleeps between updates.
Hidden wall feeds continue decoding to make returning from focus immediate.

[PROPOSAL.md](PROPOSAL.md) records the original product scope and Rust/wgpu direction.

## Build and run

Requires macOS, Rust, pkg-config, and FFmpeg development libraries. The installed
FFmpeg and its dependencies determine the minimum macOS version. This machine's
Homebrew FFmpeg requires macOS 26.

```sh
brew install ffmpeg pkg-config
cargo build --release
cargo run --release
```

Run `./scripts/build-app.sh` to create `.build/CamCam.app` for Finder.
Add `--open` to launch it, `--install` to replace `/Applications/CamCam.app`, or
`--install --open` to install and launch the installed copy. Other arguments pass to Cargo.
The app links to the installed Homebrew libraries. Keep those available when running it.

## Discover cameras

```sh
cargo run --release -- --discover
# Choose a different config destination.
cargo run --release -- --discover --config /path/to/config.toml
# Launch the bundled app in discovery mode.
open .build/CamCam.app --args --discover
```

Discovery opens a separate screen and scans the selected local IPv4 interface. Choose the
interface and scan range, click a camera to preview it, then tick the cameras to add and
choose **Save selected and open wall**. Names, addresses, usernames, and password variable
names can be edited before previewing or saving. Export password variables before launching
CamCam, using the same credentials format as the normal camera wall.

ONVIF cameras are found using WS-Discovery multicast probes. RTSP discovery checks ports
554, 8554, and 10554 and tries a short list of common stream paths. An RTSP service can be
listed even when its stream path needs to be entered manually, particularly when the
camera requires authentication. The preview reports connection errors for these entries.

The initial scan range covers at most a /24 around the interface address. The range can be
changed within that interface's subnet, up to 4096 addresses per scan. Progress and a stop
button keep scanning separate from playback.
The bundled app includes a Local Network usage description for macOS. Allow local network
access when prompted. If multicast is unavailable, the discovery screen reports it while
RTSP scanning continues; check Privacy & Security → Local Network and rescan.

Saving appends only selected cameras and opens the updated wall. Existing entries and TOML
comments are preserved, duplicate endpoints are skipped, and the file is replaced atomically.
The destination defaults to `~/.config/camcam/config.toml`, including when it is a symlink.

## Configuration

Copy [config.example.toml](config.example.toml) to `~/.config/camcam/config.toml`, edit the
addresses, then launch CamCam. The config format is preserved from the initial viewer.
Entries appear in file order. Config changes apply on relaunch.

```sh
mkdir -p ~/.config/camcam
cp config.example.toml ~/.config/camcam/config.toml
export CAMCAM_FRONT_DOOR_PASSWORD='your-camera-password'
cargo run --release
```

Use `camcam --config /path/to/config.toml` to choose another file. Password variables must
be available to the launched process. For a Finder app launched from a shell,
`open --env CAMCAM_FRONT_DOOR_PASSWORD=... .build/CamCam.app` passes the variable explicitly.
Each camera may use `username` and `password_env`. Existing stream URL credentials take precedence.

ONVIF entries take a device-service URL, typically `http://camera/onvif/device_service`.
CamCam reads services, profiles, and stream URIs, with a GetCapabilities fallback for
older devices. It selects H.264/HEVC profiles and skips incompatible advertised encodings.
WS-Security password digest and HTTP Digest authentication are supported. HTTP Basic is
accepted over HTTPS. Camera clock skew can cause WS-Security authentication to fail.

Only same-host HTTP(S) media-service addresses are accepted. Configured credentials are
added to discovered RTSP URLs only when the stream host matches the device host. A camera
advertising an unusable address can be configured directly using a working RTSP URL.

RTMP entries pull from a playback endpoint. A camera that publishes RTMP needs a receiver
such as MediaMTX, whose playback URL can then be added to CamCam.

## Playback behavior

Each tile connects independently. Hover a failing tile for its connection-stage error.
A disconnected or stalled feed retries after 1, 2, 4, 8, 16, and 30 seconds. The backoff
resets after a decoded frame. Initial stream connection and analysis have a 12-second
deadline. A live feed without decoded frames times out after 8 seconds.

Focus opens an additional main-stream connection when a larger ONVIF profile is available.
The wall feed stays visible until the main feed produces a frame. If the main stream
fails, the wall feed remains available. Returning to the wall stops the extra connection.
Workers stop through cancellation, including during network input and retry backoff.

The decoder waits for a keyframe when joining a live stream. Video preserves its aspect
ratio. Audio is silent.

The `LIVE · VT` badge confirms decoded VideoToolbox pixel buffers. CamCam declines FFmpeg
software pixel formats. FFmpeg requires hardware decoding for H.264. Its HEVC VideoToolbox
path enables hardware acceleration but can use Apple's software decoder on unsupported
hardware. Ten-bit HEVC plane import is supported. HDR tone mapping needs separate validation.

## Verification

`cargo test`, `cargo fmt --check`, and `cargo clippy --all-targets -- -D warnings` run the
ordinary checks. Tests requiring live fixtures and macOS GPU access are explicitly ignored
in the ordinary test run.

The fixture script starts loopback RTMP, RTSP, and ONVIF services, runs all tests, then stops
its processes. Ports 11935, 18554, and 18080 must be available.

```sh
brew install mediamtx
./scripts/test-live.sh
# Or use a separately downloaded official MediaMTX executable.
MEDIAMTX=/path/to/mediamtx ./scripts/test-live.sh
```

The live suite checks Media1/Media2 profile selection, WS-Security and HTTP Digest,
repeated H.264/HEVC joins through VideoToolbox, native Metal plane import, rendered color
bars, four concurrent feeds, focus promotion to the main stream, Esc returning to the
wall, automatic recovery after a publisher restart, and cancellation during backoff.
It also checks multicast ONVIF discovery, RTSP stream-path detection, previews of discovered
cameras, checkbox/save interaction, and reloading the saved config. Ordinary tests cover
comment preservation, symlinks, inline camera arrays, duplicate saves, and concurrent saves.
The UI tests send pointer and keyboard events through egui and render the normal
application paint callbacks into GPU textures. Wall and focus captures are saved under
`target/verification/`.

These are synthetic local fixtures. Physical-camera compatibility and sustained load
still need measurement on the intended cameras and Mac.
