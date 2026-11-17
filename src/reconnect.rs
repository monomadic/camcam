//! Camera identity learning and reconnection run on stream workers.
use crate::{
    bonjour,
    config::{Camera, Kind},
    config_save, discover, onvif,
};
use std::{
    collections::{HashSet, VecDeque},
    net::{Ipv4Addr, SocketAddr, TcpStream},
    path::Path,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};
use url::Url;

fn persist(path: &Path, camera: &mut Camera, updated: Camera) {
    if config_save::update_location(path, camera, &updated).is_err() {
        eprintln!("CamCam: could not save camera rediscovery information");
    }
    *camera = updated;
}

/// Learn only after decoding proves the existing connection is usable.
pub fn remember(camera: &mut Camera, path: &Path, stop: &Arc<AtomicBool>) {
    if camera.discovery_id.is_some() {
        return;
    }
    let Some(ip) = Url::parse(&camera.url)
        .ok()
        .and_then(|u| u.host_str()?.parse::<Ipv4Addr>().ok())
    else {
        return;
    };
    let identity = if camera.kind == Kind::Onvif {
        onvif::device_id(camera, stop).ok()
    } else {
        let matches: HashSet<_> = bonjour::cameras(stop, None)
            .into_iter()
            .filter(|(_, address)| *address == ip)
            .map(|(id, _)| id)
            .collect();
        if matches.len() == 1 {
            matches.into_iter().next()
        } else {
            None
        }
    };
    if let Some(id) = identity {
        let mut updated = camera.clone();
        updated.discovery_id = Some(id);
        persist(path, camera, updated);
    }
}

fn moved(camera: &Camera, address: Ipv4Addr) -> Option<Camera> {
    let mut url = Url::parse(&camera.url).ok()?;
    url.set_host(Some(&address.to_string())).ok()?;
    let mut updated = camera.clone();
    updated.url = url.to_string();
    Some(updated)
}

fn reachable(address: Ipv4Addr, port: u16) -> bool {
    TcpStream::connect_timeout(
        &SocketAddr::from((address, port)),
        Duration::from_millis(250),
    )
    .is_ok()
}

pub fn relocate(camera: &mut Camera, path: &Path, stop: &Arc<AtomicBool>) {
    let Some(id) = camera.discovery_id.as_deref() else {
        return;
    };
    let Ok(local) = discover::interfaces() else {
        return;
    };
    let is_local = |ip| local.iter().any(|interface| interface.subnet.contains(ip));
    let Some(url) = Url::parse(&camera.url).ok() else {
        return;
    };
    let Some(port) = url.port_or_known_default() else {
        return;
    };
    let mut matching = HashSet::new();
    if id.starts_with("bonjour:") {
        for (_, ip) in bonjour::cameras(stop, Some(id)) {
            if is_local(ip) && reachable(ip, port) {
                matching.insert(ip);
            }
        }
    } else if camera.kind == Kind::Onvif && id.starts_with("onvif:") {
        let mut hosts = local
            .iter()
            .flat_map(|i| i.subnet.hosts().unwrap_or_default())
            .collect::<Vec<_>>();
        hosts.sort_unstable();
        hosts.dedup();
        let targets = Mutex::new(VecDeque::from(hosts));
        let open = Mutex::new(Vec::new());
        std::thread::scope(|scope| {
            for _ in 0..16 {
                let (targets, open) = (&targets, &open);
                scope.spawn(move || {
                    while !stop.load(Ordering::Relaxed) {
                        let Some(ip) = targets.lock().expect("rediscovery targets").pop_front()
                        else {
                            break;
                        };
                        if reachable(ip, port) {
                            open.lock().expect("rediscovery results").push(ip);
                        }
                    }
                });
            }
        });
        for ip in open.into_inner().unwrap() {
            if stop.load(Ordering::Relaxed) {
                return;
            }
            if let Some(candidate) = moved(camera, ip)
                && onvif::device_id(&candidate, stop).ok().as_deref() == Some(id)
            {
                matching.insert(ip);
            }
        }
    }
    if !stop.load(Ordering::Relaxed)
        && matching.len() == 1
        && let Some(updated) = moved(camera, matching.into_iter().next().unwrap())
        && updated.url != camera.url
    {
        persist(path, camera, updated);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn moving_preserves_paths_ports_credentials_and_identity() {
        for (kind, url) in [
            (
                Kind::Onvif,
                "http://192.168.1.155:2020/onvif/device_service",
            ),
            (
                Kind::Rtsp,
                "rtsp://viewer:dummy@192.168.1.155:554/live0?channel=1",
            ),
        ] {
            let camera = Camera {
                name: "Test".into(),
                kind,
                url: url.into(),
                username: Some("viewer".into()),
                password_env: None,
                password_keychain: Some("opaque-reference".into()),
                session_password: None,
                discovery_id: Some("stable-device-identity".into()),
            };
            let updated = moved(&camera, "192.168.1.154".parse().unwrap()).unwrap();
            let old = Url::parse(&camera.url).unwrap();
            let new = Url::parse(&updated.url).unwrap();
            assert_eq!(new.host_str(), Some("192.168.1.154"));
            assert_eq!(
                (
                    new.path(),
                    new.query(),
                    new.username(),
                    new.password(),
                    new.port()
                ),
                (
                    old.path(),
                    old.query(),
                    old.username(),
                    old.password(),
                    old.port()
                )
            );
            assert_eq!(updated.password_keychain, camera.password_keychain);
            assert_eq!(updated.discovery_id, camera.discovery_id);
        }
    }
}
