//! Bounded native Bonjour lookups. DNS-SD callbacks run on the calling stream worker.
use std::{
    ffi::{CStr, CString, c_char, c_void},
    net::Ipv4Addr,
    sync::atomic::{AtomicBool, Ordering},
    time::{Duration, Instant},
};

type ServiceRef = *mut c_void;
type BrowseReply = unsafe extern "C" fn(
    ServiceRef,
    u32,
    u32,
    i32,
    *const c_char,
    *const c_char,
    *const c_char,
    *mut c_void,
);
type ResolveReply = unsafe extern "C" fn(
    ServiceRef,
    u32,
    u32,
    i32,
    *const c_char,
    *const c_char,
    u16,
    u16,
    *const u8,
    *mut c_void,
);
type AddressReply = unsafe extern "C" fn(
    ServiceRef,
    u32,
    u32,
    i32,
    *const c_char,
    *const libc::sockaddr,
    u32,
    *mut c_void,
);
#[link(name = "System")]
unsafe extern "C" {
    fn DNSServiceBrowse(
        service: *mut ServiceRef,
        flags: u32,
        interface: u32,
        kind: *const c_char,
        domain: *const c_char,
        callback: BrowseReply,
        context: *mut c_void,
    ) -> i32;
    fn DNSServiceResolve(
        service: *mut ServiceRef,
        flags: u32,
        interface: u32,
        name: *const c_char,
        kind: *const c_char,
        domain: *const c_char,
        callback: ResolveReply,
        context: *mut c_void,
    ) -> i32;
    fn DNSServiceGetAddrInfo(
        service: *mut ServiceRef,
        flags: u32,
        interface: u32,
        protocol: u32,
        host: *const c_char,
        callback: AddressReply,
        context: *mut c_void,
    ) -> i32;
    fn DNSServiceRefSockFD(service: ServiceRef) -> i32;
    fn DNSServiceProcessResult(service: ServiceRef) -> i32;
    fn DNSServiceRefDeallocate(service: ServiceRef);
}

struct Query(ServiceRef);
impl Query {
    fn collect(&self, stop: &AtomicBool, duration: Duration) {
        let end = Instant::now() + duration;
        while Instant::now() < end && !stop.load(Ordering::Relaxed) {
            // SAFETY: self owns a live DNS-SD reference and callback storage outlives this call.
            unsafe {
                let mut fd = libc::pollfd {
                    fd: DNSServiceRefSockFD(self.0),
                    events: libc::POLLIN,
                    revents: 0,
                };
                if fd.fd < 0 {
                    break;
                }
                let ready = libc::poll(&mut fd, 1, 100);
                if ready < 0 {
                    break;
                }
                if ready > 0
                    && (fd.revents & libc::POLLIN == 0 || DNSServiceProcessResult(self.0) != 0)
                {
                    break;
                }
            }
        }
    }
}
impl Drop for Query {
    fn drop(&mut self) {
        // SAFETY: this reference is exclusively owned, with no callbacks outstanding.
        unsafe { DNSServiceRefDeallocate(self.0) };
    }
}

#[derive(Clone)]
struct Service {
    name: String,
    kind: String,
    interface: u32,
}
impl Service {
    fn id(&self) -> String {
        format!("bonjour:{}:{}", self.kind, self.name)
    }
}

unsafe extern "C" fn browsed(
    _: ServiceRef,
    flags: u32,
    interface: u32,
    error: i32,
    name: *const c_char,
    kind: *const c_char,
    _: *const c_char,
    context: *mut c_void,
) {
    if error != 0 || flags & 2 == 0 || name.is_null() || kind.is_null() {
        return;
    }
    // SAFETY: DNS-SD supplies NUL-terminated names and context points to the active collector.
    unsafe {
        let services = &mut *context.cast::<Vec<Service>>();
        services.push(Service {
            name: CStr::from_ptr(name).to_string_lossy().into_owned(),
            kind: CStr::from_ptr(kind)
                .to_string_lossy()
                .trim_end_matches('.')
                .to_owned(),
            interface,
        });
    }
}

unsafe extern "C" fn resolved(
    _: ServiceRef,
    _: u32,
    interface: u32,
    error: i32,
    _: *const c_char,
    host: *const c_char,
    _: u16,
    _: u16,
    _: *const u8,
    context: *mut c_void,
) {
    if error != 0 || host.is_null() {
        return;
    }
    // SAFETY: host is a DNS-SD string. The collector remains live through ProcessResult.
    unsafe {
        *context.cast::<Option<(String, u32)>>() = Some((
            CStr::from_ptr(host).to_string_lossy().into_owned(),
            interface,
        ));
    }
}

unsafe extern "C" fn addressed(
    _: ServiceRef,
    flags: u32,
    _: u32,
    error: i32,
    _: *const c_char,
    address: *const libc::sockaddr,
    _: u32,
    context: *mut c_void,
) {
    if error != 0 || flags & 2 == 0 || address.is_null() {
        return;
    }
    // SAFETY: DNS-SD provides a sockaddr matching its family for the duration of the callback.
    unsafe {
        if (*address).sa_family as i32 == libc::AF_INET {
            let address = &*address.cast::<libc::sockaddr_in>();
            let ip = Ipv4Addr::from(address.sin_addr.s_addr.to_ne_bytes());
            let addresses = &mut *context.cast::<Vec<Ipv4Addr>>();
            if !addresses.contains(&ip) {
                addresses.push(ip);
            }
        }
    }
}

fn addresses(service: &Service, stop: &AtomicBool) -> Vec<Ipv4Addr> {
    let mut reference = std::ptr::null_mut();
    let mut host: Option<(String, u32)> = None;
    let (Ok(name), Ok(kind)) = (
        CString::new(service.name.as_str()),
        CString::new(service.kind.as_str()),
    ) else {
        return Vec::new();
    };
    // SAFETY: string buffers and host collector remain live until the query is dropped.
    let status = unsafe {
        DNSServiceResolve(
            &mut reference,
            0,
            service.interface,
            name.as_ptr(),
            kind.as_ptr(),
            c"local.".as_ptr(),
            resolved,
            (&mut host as *mut Option<(String, u32)>).cast(),
        )
    };
    if status != 0 {
        return Vec::new();
    }
    let query = Query(reference);
    query.collect(stop, Duration::from_millis(1500));
    drop(query);
    let Some((host, interface)) = host else {
        return Vec::new();
    };
    let Ok(host) = CString::new(host) else {
        return Vec::new();
    };
    let mut addresses = Vec::<Ipv4Addr>::new();
    // SAFETY: host and address collector outlive the query.
    let status = unsafe {
        DNSServiceGetAddrInfo(
            &mut reference,
            0,
            interface,
            1,
            host.as_ptr(),
            addressed,
            (&mut addresses as *mut Vec<Ipv4Addr>).cast(),
        )
    };
    if status != 0 {
        return addresses;
    }
    let query = Query(reference);
    query.collect(stop, Duration::from_millis(1500));
    drop(query);
    addresses
}

pub fn cameras(stop: &AtomicBool, saved: Option<&str>) -> Vec<(String, Ipv4Addr)> {
    let mut services = Vec::<Service>::new();
    if let Some(id) = saved {
        if let Some(value) = id.strip_prefix("bonjour:")
            && let Some((kind, name)) = value.split_once(':')
        {
            services.push(Service {
                name: name.into(),
                kind: kind.into(),
                interface: 0,
            });
        }
    } else {
        for kind in [c"_hap._tcp", c"_rtsp._tcp"] {
            let mut reference = std::ptr::null_mut();
            // SAFETY: services remains live while each callback is processed.
            let status = unsafe {
                DNSServiceBrowse(
                    &mut reference,
                    0,
                    0,
                    kind.as_ptr(),
                    c"local.".as_ptr(),
                    browsed,
                    (&mut services as *mut Vec<Service>).cast(),
                )
            };
            if status == 0 {
                let query = Query(reference);
                query.collect(stop, Duration::from_millis(1500));
            }
        }
    }
    let mut found = Vec::new();
    for service in services {
        if stop.load(Ordering::Relaxed) {
            break;
        }
        for address in addresses(&service, stop) {
            found.push((service.id(), address));
        }
    }
    found.sort_unstable();
    found.dedup();
    found
}
