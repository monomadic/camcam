//! Local IPv4 discovery. WS-Discovery and bounded RTSP probes run off the UI thread.
use crate::{
    config::{Camera, Kind},
    config_save,
};
use anyhow::{Context, Result, ensure};
use roxmltree::Document;
use socket2::{Domain, Protocol, Socket, Type};
use std::{
    collections::{HashSet, VecDeque},
    ffi::CStr,
    io::{Read, Write},
    net::{Ipv4Addr, SocketAddr, SocketAddrV4, TcpStream, UdpSocket},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};
use url::Url;

pub const RTSP_PORTS: [u16; 3] = [554, 8554, 10554];
const STREAM_PATHS: [&str; 9] = [
    "/",
    "/stream1",
    "/stream2",
    "/live",
    "/live/ch00_0",
    "/h264Preview_01_main",
    "/Streaming/Channels/101",
    "/cam/realmonitor?channel=1&subtype=0",
    "/main",
];

#[derive(Clone, Copy, Debug)]
pub struct Subnet {
    pub address: Ipv4Addr,
    pub prefix: u8,
}

impl Subnet {
    pub fn parse(value: &str) -> Result<Self> {
        let (address, prefix) = value
            .trim()
            .split_once('/')
            .context("Enter a network such as 192.168.1.0/24")?;
        let prefix = prefix.parse::<u8>().context("Invalid network prefix")?;
        ensure!(prefix <= 32, "Invalid network prefix");
        Ok(Self {
            address: address.parse().context("Enter an IPv4 network")?,
            prefix,
        })
    }

    fn mask(self) -> u32 {
        u32::MAX.checked_shl(32 - self.prefix as u32).unwrap_or(0)
    }
    pub fn base(self) -> Ipv4Addr {
        (u32::from(self.address) & self.mask()).into()
    }
    pub fn contains(self, address: Ipv4Addr) -> bool {
        u32::from(address) & self.mask() == u32::from(self.base())
    }
    pub fn cidr(self) -> String {
        format!("{}/{}", self.base(), self.prefix)
    }

    fn hosts(self) -> Result<Vec<Ipv4Addr>> {
        let size = 1u64 << (32 - self.prefix);
        ensure!(
            size <= 4096,
            "Choose a network with at most 4096 addresses (/20 or smaller)"
        );
        let base = u32::from(self.base());
        let edges = if self.prefix < 31 { 1 } else { 0 };
        Ok((edges..size as u32 - edges)
            .map(|offset| (base + offset).into())
            .collect())
    }
}

#[derive(Clone)]
pub struct Interface {
    pub name: String,
    pub address: Ipv4Addr,
    pub subnet: Subnet,
}

pub fn interfaces() -> Result<Vec<Interface>> {
    let mut head = std::ptr::null_mut();
    // SAFETY: getifaddrs owns a linked list until the matching freeifaddrs below.
    ensure!(
        unsafe { libc::getifaddrs(&mut head) } == 0,
        "Could not enumerate local network interfaces"
    );
    let mut current = head;
    let mut found = Vec::new();
    unsafe {
        while !current.is_null() {
            let interface = &*current;
            if !interface.ifa_addr.is_null()
                && !interface.ifa_netmask.is_null()
                && (*interface.ifa_addr).sa_family as i32 == libc::AF_INET
                && interface.ifa_flags & libc::IFF_UP as u32 != 0
                && interface.ifa_flags & libc::IFF_LOOPBACK as u32 == 0
                && interface.ifa_flags & libc::IFF_POINTOPOINT as u32 == 0
            {
                let address = &*interface.ifa_addr.cast::<libc::sockaddr_in>();
                let mask = &*interface.ifa_netmask.cast::<libc::sockaddr_in>();
                let address = Ipv4Addr::from(address.sin_addr.s_addr.to_ne_bytes());
                let mask = u32::from_be(mask.sin_addr.s_addr);
                found.push(Interface {
                    name: CStr::from_ptr(interface.ifa_name)
                        .to_string_lossy()
                        .into_owned(),
                    address,
                    subnet: Subnet {
                        address,
                        prefix: mask.count_ones() as u8,
                    },
                });
            }
            current = interface.ifa_next;
        }
        libc::freeifaddrs(head);
    }
    // Prefer ordinary Ethernet/Wi-Fi interfaces over virtual bridges.
    found.sort_by_key(|interface| (!interface.name.starts_with("en"), interface.name.clone()));
    found.dedup_by_key(|interface| interface.address);
    Ok(found)
}

#[derive(Clone)]
pub struct Plan {
    pub interface: Interface,
    pub subnet: Subnet,
    pub ports: Vec<u16>,
    pub probe_address: SocketAddrV4,
    pub probe_duration: Duration,
}

impl Plan {
    pub fn local(interface: Interface, subnet: Subnet) -> Result<Self> {
        ensure!(
            subnet.prefix >= interface.subnet.prefix && interface.subnet.contains(subnet.base()),
            "Choose a subnet on the selected local interface"
        );
        subnet.hosts()?;
        Ok(Self {
            interface,
            subnet,
            ports: RTSP_PORTS.to_vec(),
            probe_address: SocketAddrV4::new(Ipv4Addr::new(239, 255, 255, 250), 3702),
            probe_duration: Duration::from_secs(3),
        })
    }
}

#[derive(Clone)]
pub struct Candidate {
    pub camera: Camera,
    pub note: String,
}

#[derive(Default, Clone)]
pub struct Snapshot {
    pub found: Vec<Candidate>,
    pub completed: usize,
    pub total: usize,
    pub active: bool,
    pub warnings: Vec<String>,
}

pub struct Scan {
    state: Arc<Mutex<Snapshot>>,
    stop: Arc<AtomicBool>,
    worker: Option<JoinHandle<()>>,
}

impl Scan {
    pub fn start(plan: Plan, repaint: impl Fn() + Send + Sync + 'static) -> Result<Self> {
        let targets = plan
            .subnet
            .hosts()?
            .into_iter()
            .flat_map(|ip| {
                plan.ports
                    .iter()
                    .map(move |port| SocketAddrV4::new(ip, *port))
            })
            .collect::<VecDeque<_>>();
        let state = Arc::new(Mutex::new(Snapshot {
            total: targets.len(),
            active: true,
            ..Default::default()
        }));
        let stop = Arc::new(AtomicBool::new(false));
        let (worker_state, worker_stop) = (state.clone(), stop.clone());
        let worker = thread::spawn(move || {
            let targets = Mutex::new(targets);
            thread::scope(|scope| {
                let (state, stop, repaint) = (&worker_state, &worker_stop, &repaint);
                let plan = &plan;
                scope.spawn(move || {
                    if let Err(error) =
                        probe_onvif(plan, stop, |candidate| publish(state, candidate, repaint))
                    {
                        state
                            .lock()
                            .expect("scan state")
                            .warnings
                            .push(error.to_string());
                        repaint();
                    }
                });
                for _ in 0..16 {
                    let targets = &targets;
                    scope.spawn(move || {
                        while !stop.load(Ordering::Relaxed) {
                            let Some(target) = targets.lock().expect("scan targets").pop_front()
                            else {
                                break;
                            };
                            if let Some(candidate) = probe_rtsp(target, stop) {
                                publish(state, candidate, repaint);
                            }
                            state.lock().expect("scan state").completed += 1;
                            repaint();
                        }
                    });
                }
            });
            worker_state.lock().expect("scan state").active = false;
            repaint();
        });
        Ok(Self {
            state,
            stop,
            worker: Some(worker),
        })
    }

    pub fn snapshot(&self) -> Snapshot {
        self.state.lock().expect("scan state").clone()
    }
    pub fn stop(&self) {
        self.stop.store(true, Ordering::Relaxed);
    }
    pub fn join(mut self) {
        self.stop();
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}
impl Drop for Scan {
    fn drop(&mut self) {
        self.stop();
    }
}

fn publish(state: &Mutex<Snapshot>, candidate: Candidate, repaint: &impl Fn()) {
    let mut state = state.lock().expect("scan state");
    let id = config_save::identity(&candidate.camera);
    if !state
        .found
        .iter()
        .any(|c| config_save::identity(&c.camera) == id)
    {
        state.found.push(candidate);
        drop(state);
        repaint();
    }
}

fn probe_onvif(plan: &Plan, stop: &AtomicBool, mut found: impl FnMut(Candidate)) -> Result<()> {
    let socket = Socket::new(Domain::IPV4, Type::DGRAM, Some(Protocol::UDP))?;
    socket.bind(&SocketAddrV4::new(plan.interface.address, 0).into())?;
    socket.set_multicast_if_v4(&plan.interface.address)?;
    socket.set_multicast_ttl_v4(1)?;
    let socket: UdpSocket = socket.into();
    socket.set_read_timeout(Some(Duration::from_millis(150)))?;
    let mut ids = HashSet::new();
    for types in ["<d:Types>dn:NetworkVideoTransmitter</d:Types>", ""] {
        let value = format!("{:032x}", rand::random::<u128>());
        let id = format!(
            "urn:uuid:{}-{}-4{}-a{}-{}",
            &value[..8],
            &value[8..12],
            &value[13..16],
            &value[17..20],
            &value[20..]
        );
        let message = format!(
            r#"<s:Envelope xmlns:s="http://www.w3.org/2003/05/soap-envelope"
            xmlns:a="http://schemas.xmlsoap.org/ws/2004/08/addressing"
            xmlns:d="http://schemas.xmlsoap.org/ws/2005/04/discovery"
            xmlns:dn="http://www.onvif.org/ver10/network/wsdl"><s:Header>
            <a:MessageID>{id}</a:MessageID><a:To>urn:schemas-xmlsoap-org:ws:2005:04:discovery</a:To>
            <a:Action>http://schemas.xmlsoap.org/ws/2005/04/discovery/Probe</a:Action>
            </s:Header><s:Body><d:Probe>{types}</d:Probe></s:Body></s:Envelope>"#
        );
        socket.send_to(message.as_bytes(), plan.probe_address).map_err(|error| {
            if matches!(error.raw_os_error(), Some(libc::EHOSTUNREACH | libc::EACCES | libc::EPERM)) {
                anyhow::anyhow!("ONVIF multicast is unavailable. Check CamCam's Local Network permission and network routing, then rescan.")
            } else {
                anyhow::anyhow!("Could not send ONVIF discovery probe: {error}")
            }
        })?;
        ids.insert(id);
    }
    let until = Instant::now() + plan.probe_duration;
    let mut data = [0; 65535];
    while Instant::now() < until && !stop.load(Ordering::Relaxed) {
        match socket.recv_from(&mut data) {
            Ok((length, SocketAddr::V4(source))) => {
                if let Ok(message) = std::str::from_utf8(&data[..length]) {
                    for candidate in probe_matches(message, *source.ip(), plan.subnet, &ids) {
                        found(candidate);
                    }
                }
            }
            Ok(_) => {}
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) => {}
            Err(error) => return Err(error.into()),
        }
    }
    Ok(())
}

fn probe_matches(
    message: &str,
    source: Ipv4Addr,
    subnet: Subnet,
    ids: &HashSet<String>,
) -> Vec<Candidate> {
    let Ok(document) = Document::parse(message) else {
        return Vec::new();
    };
    let text = |node: roxmltree::Node<'_, '_>, name| {
        node.children()
            .find(|n| n.is_element() && n.tag_name().name() == name)
            .and_then(|n| n.text())
            .unwrap_or("")
            .trim()
            .to_owned()
    };
    let relates = document
        .descendants()
        .find(|n| n.is_element() && n.tag_name().name() == "RelatesTo")
        .and_then(|n| n.text())
        .unwrap_or("")
        .trim();
    if !ids.contains(relates) || !subnet.contains(source) {
        return Vec::new();
    }
    document
        .descendants()
        .filter(|n| n.is_element() && n.tag_name().name() == "ProbeMatch")
        .filter_map(|node| {
            let types = text(node, "Types");
            let scopes = text(node, "Scopes");
            let addresses = text(node, "XAddrs");
            if !types
                .split_whitespace()
                .any(|t| t.ends_with("NetworkVideoTransmitter"))
                && !scopes.contains("onvif://")
            {
                return None;
            }
            let url = addresses
                .split_whitespace()
                .filter_map(|s| Url::parse(s).ok())
                .find(|url| {
                    matches!(url.scheme(), "http" | "https")
                        && url.host_str().and_then(|s| s.parse::<Ipv4Addr>().ok()) == Some(source)
                        && url.username().is_empty()
                        && url.password().is_none()
                })?;
            let name = scopes
                .split_whitespace()
                .find_map(|scope| {
                    let value = scope.strip_prefix("onvif://www.onvif.org/name/")?;
                    Some(
                        url::form_urlencoded::parse(format!("name={value}").as_bytes())
                            .next()?
                            .1
                            .into_owned(),
                    )
                })
                .unwrap_or_else(|| format!("ONVIF {source}"));
            Some(Candidate {
                camera: Camera {
                    name,
                    kind: Kind::Onvif,
                    url: url.to_string(),
                    username: None,
                    password_env: None,
                    password_keychain: None,
                    session_password: None,
                },
                note: "ONVIF device found".into(),
            })
        })
        .collect()
}

struct Response {
    status: u16,
    body: String,
}

fn rtsp_request(address: SocketAddrV4, method: &str, path: &str) -> Option<Response> {
    let mut stream =
        TcpStream::connect_timeout(&address.into(), Duration::from_millis(250)).ok()?;
    stream
        .set_read_timeout(Some(Duration::from_millis(500)))
        .ok()?;
    stream
        .set_write_timeout(Some(Duration::from_millis(500)))
        .ok()?;
    let request = format!(
        "{method} rtsp://{address}{path} RTSP/1.0\r\nCSeq: 1\r\nUser-Agent: CamCam\r\nAccept: application/sdp\r\n\r\n"
    );
    stream.write_all(request.as_bytes()).ok()?;
    let mut data = Vec::new();
    let mut buffer = [0; 4096];
    let mut header_end = None;
    let mut body_length = 0;
    let deadline = Instant::now() + Duration::from_secs(1);
    while data.len() < 65536 {
        if Instant::now() >= deadline {
            return None;
        }
        let count = stream.read(&mut buffer).ok()?;
        if count == 0 {
            break;
        }
        data.extend_from_slice(&buffer[..count]);
        if header_end.is_none()
            && let Some(index) = data.windows(4).position(|s| s == b"\r\n\r\n")
        {
            header_end = Some(index + 4);
            let header = std::str::from_utf8(&data[..index]).ok()?;
            let line = header.lines().next()?;
            if !line.starts_with("RTSP/1.") {
                return None;
            }
            body_length = header
                .lines()
                .filter_map(|line| line.split_once(':'))
                .find(|(name, _)| name.eq_ignore_ascii_case("content-length"))
                .and_then(|(_, value)| value.trim().parse::<usize>().ok())
                .unwrap_or(0);
        }
        if header_end.is_some_and(|end| data.len() >= end + body_length) {
            break;
        }
    }
    let end = header_end?;
    let header = std::str::from_utf8(&data[..end]).ok()?;
    let status = header
        .lines()
        .next()?
        .split_whitespace()
        .nth(1)?
        .parse()
        .ok()?;
    Some(Response {
        status,
        body: String::from_utf8_lossy(&data[end..]).into_owned(),
    })
}

fn probe_rtsp(address: SocketAddrV4, stop: &AtomicBool) -> Option<Candidate> {
    let response = rtsp_request(address, "OPTIONS", "/")?;
    let mut path = "/";
    let mut note = "RTSP service found; edit the stream path if needed";
    if response.status == 401 {
        note = "Authentication required; enter credentials and a stream path";
    } else {
        for candidate in STREAM_PATHS {
            if stop.load(Ordering::Relaxed) {
                return None;
            }
            if let Some(response) = rtsp_request(address, "DESCRIBE", candidate) {
                if response.status == 200
                    && response
                        .body
                        .lines()
                        .any(|line| line.starts_with("m=video"))
                {
                    path = candidate;
                    note = "RTSP video stream found";
                    break;
                }
                if response.status == 401 {
                    note = "Authentication required; enter credentials and a stream path";
                    break;
                }
            }
        }
    }
    Some(Candidate {
        camera: Camera {
            name: format!("RTSP {address}"),
            kind: Kind::Rtsp,
            url: format!("rtsp://{address}{path}"),
            username: None,
            password_env: None,
            password_keychain: None,
            session_password: None,
        },
        note: note.into(),
    })
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    #[test]
    fn subnet_bounds_and_probe_replies_are_checked() {
        assert_eq!(
            Subnet::parse("192.168.1.44/24")
                .unwrap()
                .hosts()
                .unwrap()
                .len(),
            254
        );
        assert!(Subnet::parse("192.168.0.0/16").unwrap().hosts().is_err());
        assert_eq!(
            Subnet::parse("127.0.0.1/32").unwrap().hosts().unwrap(),
            [Ipv4Addr::LOCALHOST]
        );
        assert!(Subnet::parse("192.168.1.0/33").is_err());
        let reply = r#"<Envelope><Header><RelatesTo>test</RelatesTo></Header><ProbeMatch>
            <Types>dn:NetworkVideoTransmitter</Types><Scopes>onvif://www.onvif.org/name/Front%20door</Scopes>
            <XAddrs>http://127.0.0.1:18080/onvif</XAddrs></ProbeMatch></Envelope>"#;
        let ids = HashSet::from(["test".to_owned()]);
        let subnet = Subnet::parse("127.0.0.1/32").unwrap();
        let cameras = probe_matches(reply, Ipv4Addr::LOCALHOST, subnet, &ids);
        assert_eq!(cameras[0].camera.name, "Front door");
        assert!(probe_matches(reply, "127.0.0.2".parse().unwrap(), subnet, &ids).is_empty());
        assert!(probe_matches(reply, Ipv4Addr::LOCALHOST, subnet, &HashSet::new()).is_empty());
    }
}

#[cfg(test)]
impl Plan {
    pub(crate) fn fixture() -> Self {
        let subnet = Subnet::parse("127.0.0.1/32").unwrap();
        Self {
            interface: Interface {
                name: "Fixture".into(),
                address: Ipv4Addr::LOCALHOST,
                subnet,
            },
            subnet,
            ports: vec![18554],
            probe_address: SocketAddrV4::new(Ipv4Addr::new(239, 255, 255, 250), 13702),
            probe_duration: Duration::from_millis(600),
        }
    }
}

#[cfg(test)]
mod live_tests {
    use super::*;

    #[test]
    #[ignore = "requires scripts/test-live.sh multicast and RTSP fixtures"]
    fn discovers_multicast_onvif_and_rtsp_and_cancels() {
        let scan = Scan::start(Plan::fixture(), || {}).unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        while scan.snapshot().active {
            assert!(Instant::now() < deadline);
            thread::sleep(Duration::from_millis(10));
        }
        let result = scan.snapshot();
        assert!(result.warnings.is_empty(), "{:?}", result.warnings);
        assert_eq!(result.found.len(), 2);
        assert!(
            result
                .found
                .iter()
                .any(|c| c.camera.kind == Kind::Onvif && c.camera.name == "Fixture camera")
        );
        assert!(
            result
                .found
                .iter()
                .any(|c| c.camera.kind == Kind::Rtsp && c.camera.url.ends_with("/main"))
        );
        scan.join();
        let scan = Scan::start(
            Plan {
                probe_duration: Duration::from_secs(30),
                ..Plan::fixture()
            },
            || {},
        )
        .unwrap();
        let before = Instant::now();
        scan.join();
        assert!(before.elapsed() < Duration::from_secs(2));
    }
}
