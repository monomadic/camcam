# CamCam

- A small macOS app that opens straight to a live camera wall. Click a tile to focus that camera. Press Esc to return to the wall.
- Load cameras from `~/.config/camcam/config.toml`. Start with RTMP playback, ONVIF cameras at known addresses, and direct RTSP URLs.
- Use Switchblade’s grid and Quickview as the UI reference. Keep the app fast with hardware decoding, GPU rendering, and lower-resolution streams for the wall where available.
- Once the viewer is solid, use the same playback pipeline to figure out what undocumented Lazada and Shopee cameras actually expose.

## The viewer

Show a dark grid with a name and connection status on each tile. Draw the wall immediately and connect to each camera independently. If a feed drops, keep its tile in place, show that it’s reconnecting, and retry with backoff. Click any live tile to enlarge it in place. For an ONVIF camera with two streams, keep the low-resolution feed visible while the main stream starts.

Read the config at launch and use its order for the wall. For ONVIF entries, query the device’s media profiles and use the returned RTSP stream URL. A direct RTSP entry also gives us a way to try cameras with a known stream URL and incomplete ONVIF support. Start with RTMP pull URLs. For cameras that only publish RTMP, point them at a separate receiver and add its playback URL to CamCam. [ONVIF streaming specification](https://www.onvif.org/wp-content/uploads/2018/08/ONVIF_Profile_-S_Specification_v1-2.pdf) · [FFmpeg protocol documentation](https://ffmpeg.org/ffmpeg-protocols.html)

```toml
[[cameras]]
name = "Front door"
type = "onvif"
url = "http://192.168.1.40/onvif/device_service"
username = "viewer"
password_env = "CAMCAM_FRONT_DOOR_PASSWORD"

[[cameras]]
name = "Workshop"
type = "rtmp"
url = "rtmp://192.168.1.20/live/workshop"

[[cameras]]
name = "Bench camera"
type = "rtsp"
url = "rtsp://192.168.1.42/stream1"
```

## Keep it lean

Start with Rust, wgpu/Metal, and VideoToolbox for H.264 and HEVC. Borrow Switchblade’s interaction and rendering patterns, then test the live ingest separately. Keep connection and decode work off the UI thread. Bound the frame queues so a slow tile drops stale frames instead of drifting behind live. For cameras with only a full-resolution feed, measure the decode cost before deciding how many the wall can sustain. Drawing a smaller tile still requires decoding the source. [Switchblade README](/Users/nom/src/switchblade/README.md)

## Camera testbed

Add a `camcam probe` command once the viewer works. Give it a camera address or stream URL and have it report which stage worked or failed: connection, authentication, ONVIF profile lookup, stream setup, or first decoded frame. Report the actual codec, resolution, and frame rate when available. Keep a small compatibility log by model and firmware so the next camera can build on what we already tried. Add opt-in LAN discovery later if entering addresses by hand becomes the bottleneck.

**First milestone** — Get one RTMP feed and one ONVIF camera into the same wall. Verify hardware decode, focus switching, and recovery when one feed drops. Then run a four-feed wall on the target Mac and measure CPU, memory, dropped frames, and time to first frame. Use that baseline before testing the first undocumented camera.
