//! One worker per stream. A single replaceable frame slot bounds memory and latency.
use crate::{
    config::{Camera, Streams},
    media::{self, VideoFrame},
    onvif,
};
use std::{
    sync::{
        Arc, Condvar, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};
use url::Url;

#[derive(Clone)]
pub enum Source {
    Camera(Camera),
    Direct(Url),
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Status {
    Connecting,
    Live,
    Retrying,
    Ended,
}

pub struct Snapshot {
    pub frame: Option<Arc<VideoFrame>>,
    pub status: Status,
    pub detail: Option<String>,
    pub streams: Option<Streams>,
    pub decoded: u64,
    pub first_frame: Option<Duration>,
}

pub struct Player {
    state: Arc<Mutex<Snapshot>>,
    stop: Arc<AtomicBool>,
    wake: Arc<(Mutex<()>, Condvar)>,
    worker: Option<JoinHandle<()>>,
}

impl Player {
    pub fn new(source: Source, repaint: impl Fn() + Send + Sync + 'static) -> Self {
        Self::start(source, None, repaint)
    }

    pub fn saved(
        camera: Camera,
        path: &std::path::Path,
        repaint: impl Fn() + Send + Sync + 'static,
    ) -> Self {
        Self::start(Source::Camera(camera), Some(path.to_owned()), repaint)
    }

    fn start(
        source: Source,
        path: Option<std::path::PathBuf>,
        repaint: impl Fn() + Send + Sync + 'static,
    ) -> Self {
        let state = Arc::new(Mutex::new(Snapshot {
            frame: None,
            status: Status::Connecting,
            detail: None,
            streams: None,
            decoded: 0,
            first_frame: None,
        }));
        let stop = Arc::new(AtomicBool::new(false));
        let wake = Arc::new((Mutex::new(()), Condvar::new()));
        let worker = {
            let (state, stop, wake) = (state.clone(), stop.clone(), wake.clone());
            thread::spawn(move || run(source, path, state, stop, wake, repaint))
        };
        Self {
            state,
            stop,
            wake,
            worker: Some(worker),
        }
    }

    pub fn read<T>(&self, read: impl FnOnce(&Snapshot) -> T) -> T {
        read(&self.state.lock().expect("stream state"))
    }

    pub fn stop(&self) {
        let _guard = self.wake.0.lock().expect("stop lock");
        self.stop.store(true, Ordering::Relaxed);
        self.wake.1.notify_all();
    }

    pub fn join(mut self) {
        self.stop();
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

impl Drop for Player {
    fn drop(&mut self) {
        self.stop();
    }
}

fn run(
    mut source: Source,
    path: Option<std::path::PathBuf>,
    state: Arc<Mutex<Snapshot>>,
    stop: Arc<AtomicBool>,
    wake: Arc<(Mutex<()>, Condvar)>,
    repaint: impl Fn(),
) {
    let mut backoff = 1;
    let mut failed = false;
    let mut last_learning: Option<Instant> = None;
    let mut last_discovery: Option<Instant> = None;
    while !stop.load(Ordering::Relaxed) {
        if failed
            && last_discovery.is_none_or(|when| when.elapsed() >= Duration::from_secs(30))
            && let (Source::Camera(camera), Some(path)) = (&mut source, &path)
        {
            crate::reconnect::relocate(camera, path, &stop);
            last_discovery = Some(Instant::now());
        }
        let started = Instant::now();
        let streams = match &mut source {
            Source::Camera(camera) => camera.endpoint().and_then(|(_, credentials)| {
                if camera.session_password.is_none() {
                    camera.session_password = credentials.map(|credentials| credentials.password);
                }
                onvif::resolve(camera, &stop)
            }),
            Source::Direct(url) => Ok(Streams {
                wall: url.clone(),
                focus: None,
            }),
        };
        let mut received_frame = false;
        let result = streams.and_then(|streams| {
            let url = streams.wall.clone();
            state.lock().expect("stream state").streams = Some(streams);
            media::decode(&url, stop.clone(), |frame| {
                if stop.load(Ordering::Relaxed) {
                    return false;
                }
                let mut state = state.lock().expect("stream state");
                state.frame = Some(frame);
                state.status = Status::Live;
                state.detail = None;
                state.decoded += 1;
                if !received_frame {
                    received_frame = true;
                    state.first_frame = Some(started.elapsed());
                }
                drop(state);
                repaint();
                if last_learning.is_none_or(|when| when.elapsed() >= Duration::from_secs(30))
                    && let (Source::Camera(camera), Some(path)) = (&mut source, &path)
                    && camera.discovery_id.is_none()
                {
                    crate::reconnect::remember(camera, path, &stop);
                    last_learning = Some(Instant::now());
                }
                true
            })
        });
        if stop.load(Ordering::Relaxed) {
            break;
        }
        let direct = matches!(source, Source::Direct(_));
        {
            let mut state = state.lock().expect("stream state");
            state.frame = None;
            state.streams = None;
            state.status = if direct {
                Status::Ended
            } else {
                Status::Retrying
            };
            state.detail = result.err().map(|error| error.to_string());
        }
        repaint();
        if direct {
            break;
        }
        failed = true;
        if received_frame {
            backoff = 1;
        }
        let guard = wake.0.lock().expect("retry lock");
        let _ = wake
            .1
            .wait_timeout_while(guard, Duration::from_secs(backoff), |_| {
                !stop.load(Ordering::Relaxed)
            })
            .expect("retry wait");
        backoff = (backoff * 2).min(30);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Kind;
    use std::process::{Child, Command, Stdio};

    struct Publisher(Child);
    impl Publisher {
        fn start() -> Self {
            Self(
                Command::new("ffmpeg")
                    .args([
                        "-nostdin",
                        "-hide_banner",
                        "-loglevel",
                        "error",
                        "-re",
                        "-f",
                        "lavfi",
                        "-i",
                        "testsrc2=size=320x180:rate=12",
                        "-an",
                        "-c:v",
                        "libx264",
                        "-preset",
                        "ultrafast",
                        "-tune",
                        "zerolatency",
                        "-g",
                        "12",
                        "-rtsp_transport",
                        "tcp",
                        "-f",
                        "rtsp",
                        "rtsp://127.0.0.1:18554/recovery",
                    ])
                    .stdin(Stdio::null())
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .spawn()
                    .unwrap(),
            )
        }
    }
    impl Drop for Publisher {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }

    fn wait_until(player: &Player, ready: impl Fn(&Snapshot) -> bool) {
        let deadline = Instant::now() + Duration::from_secs(15);
        while !player.read(&ready) {
            assert!(
                Instant::now() < deadline,
                "Stream did not recover: {:?}",
                player.read(|s| s.detail.clone())
            );
            thread::sleep(Duration::from_millis(10));
        }
    }

    #[test]
    #[ignore = "requires scripts/test-live.sh fixtures and macOS video hardware"]
    fn reconnects_after_publisher_restart_and_stops_during_backoff() {
        let publisher = Publisher::start();
        let player = Player::new(
            Source::Camera(Camera {
                name: "Recovery fixture".into(),
                kind: Kind::Rtsp,
                url: "rtsp://127.0.0.1:18554/recovery".into(),
                username: None,
                password_env: None,
                discovery_id: None,
                password_keychain: None,
                session_password: None,
            }),
            || {},
        );
        wait_until(&player, |s| s.status == Status::Live);
        let before = player.read(|s| s.decoded);
        drop(publisher);
        wait_until(&player, |s| s.status == Status::Retrying);
        let publisher = Publisher::start();
        wait_until(&player, |s| s.status == Status::Live && s.decoded > before);
        drop(publisher);
        wait_until(&player, |s| s.status == Status::Retrying);
        let stop = Instant::now();
        player.join();
        assert!(stop.elapsed() < Duration::from_secs(2));
    }
}
