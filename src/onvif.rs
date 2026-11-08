use crate::config::{Camera, Credentials, Kind, Streams};
use anyhow::{Result, bail, ensure};
use base64::{Engine, engine::general_purpose::STANDARD};
use curl::easy::{Auth, Easy, List};
use roxmltree::{Document, Node};
use sha1::{Digest, Sha1};
use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};
use url::Url;

const MEDIA1: &str = "http://www.onvif.org/ver10/media/wsdl";
const MEDIA2: &str = "http://www.onvif.org/ver20/media/wsdl";
const DEVICE: &str = "http://www.onvif.org/ver10/device/wsdl";
const SECURITY: &str = "http://docs.oasis-open.org/wss/2004/01/oasis-200401-wss";

pub fn resolve(camera: &Camera, stop: &Arc<AtomicBool>) -> Result<Streams> {
    let (endpoint, credentials) = camera.endpoint()?;
    if camera.kind != Kind::Onvif {
        return Ok(Streams {
            wall: match credentials {
                Some(c) => c.apply(endpoint),
                None => endpoint,
            },
            focus: None,
        });
    }
    Client {
        device: endpoint,
        credentials,
        stop,
    }
    .streams()
}

struct Client<'a> {
    device: Url,
    credentials: Option<Credentials>,
    stop: &'a Arc<AtomicBool>,
}

impl Client<'_> {
    fn streams(&self) -> Result<Streams> {
        let services = match self.request(
            &self.device,
            DEVICE,
            "GetServices",
            "<m:IncludeCapability>false</m:IncludeCapability>",
        ) {
            Ok(response) => response,
            Err(_) => {
                self.check_cancelled()?;
                let response = self.request(
                    &self.device,
                    DEVICE,
                    "GetCapabilities",
                    "<m:Category>Media</m:Category>",
                )?;
                let document = Document::parse(&response)?;
                let address = document
                    .descendants()
                    .find(|n| named(*n, "Media"))
                    .and_then(|n| child_text(n, "XAddr"))
                    .ok_or_else(|| anyhow::anyhow!("ONVIF camera returned no media service"))?;
                return self.media(&self.service_url(address)?, MEDIA1);
            }
        };
        let document = Document::parse(&services)?;
        let mut error = anyhow::anyhow!("ONVIF camera returned no media service");
        for namespace in [MEDIA2, MEDIA1] {
            let address = document
                .descendants()
                .find(|n| named(*n, "Service") && child_text(*n, "Namespace") == Some(namespace))
                .and_then(|n| child_text(n, "XAddr"));
            let Some(address) = address else { continue };
            let result = self
                .service_url(address)
                .and_then(|url| self.media(&url, namespace));
            match result {
                Ok(streams) => return Ok(streams),
                Err(reason) => error = reason,
            }
            self.check_cancelled()?;
        }
        Err(error)
    }

    fn check_cancelled(&self) -> Result<()> {
        ensure!(!self.stop.load(Ordering::Relaxed), "Stopped");
        Ok(())
    }

    fn service_url(&self, address: &str) -> Result<Url> {
        let url = self
            .device
            .join(address)
            .map_err(|_| anyhow::anyhow!("Invalid ONVIF media address"))?;
        ensure!(
            url.host_str() == self.device.host_str()
                && matches!(url.scheme(), "http" | "https")
                && (self.device.scheme() != "https" || url.scheme() == "https"),
            "ONVIF camera returned an unusable media address"
        );
        Ok(url)
    }

    fn media(&self, endpoint: &Url, namespace: &str) -> Result<Streams> {
        let body = if namespace == MEDIA2 {
            "<m:Type>VideoEncoder</m:Type>"
        } else {
            ""
        };
        let response = self.request(endpoint, namespace, "GetProfiles", body)?;
        let profiles = profiles(&response)?;
        let first = profiles
            .first()
            .ok_or_else(|| anyhow::anyhow!("ONVIF camera returned no H.264/HEVC media profiles"))?;
        let sized: Vec<_> = profiles.iter().filter(|p| p.pixels > 0).collect();
        let small = sized
            .iter()
            .min_by_key(|p| p.pixels)
            .copied()
            .unwrap_or(first);
        let large = sized
            .iter()
            .max_by_key(|p| p.pixels)
            .copied()
            .unwrap_or(first);
        let wall = match self.stream_url(endpoint, namespace, &small.token) {
            Ok(url) => url,
            Err(error) => {
                self.check_cancelled()?;
                if large.token == small.token {
                    return Err(error);
                }
                return Ok(Streams {
                    wall: self.stream_url(endpoint, namespace, &large.token)?,
                    focus: None,
                });
            }
        };
        let focus = if large.token != small.token && large.pixels > small.pixels {
            self.stream_url(endpoint, namespace, &large.token)
                .ok()
                .filter(|url| *url != wall)
        } else {
            None
        };
        self.check_cancelled()?;
        Ok(Streams { wall, focus })
    }

    fn stream_url(&self, endpoint: &Url, namespace: &str, token: &str) -> Result<Url> {
        let setup = if namespace == MEDIA2 {
            "<m:Protocol>RTSP</m:Protocol>"
        } else {
            "<m:StreamSetup><t:Stream>RTP-Unicast</t:Stream><t:Transport><t:Protocol>RTSP</t:Protocol></t:Transport></m:StreamSetup>"
        };
        let body = format!("{setup}<m:ProfileToken>{}</m:ProfileToken>", escape(token));
        let response = self.request(endpoint, namespace, "GetStreamUri", &body)?;
        let document = Document::parse(&response)?;
        let uri = document
            .descendants()
            .find(|n| named(*n, "Uri"))
            .and_then(|n| n.text())
            .ok_or_else(|| anyhow::anyhow!("ONVIF camera returned no stream URL"))?;
        let url = Url::parse(uri).map_err(|_| anyhow::anyhow!("Invalid ONVIF stream URL"))?;
        ensure!(
            url.scheme() == "rtsp" && url.host_str().is_some(),
            "ONVIF camera returned no usable RTSP URL"
        );
        if url.host_str() == self.device.host_str()
            && let Some(credentials) = &self.credentials
        {
            return Ok(credentials.apply(url));
        }
        Ok(url)
    }

    fn request(&self, endpoint: &Url, namespace: &str, action: &str, body: &str) -> Result<String> {
        self.check_cancelled()?;
        let header = self
            .credentials
            .as_ref()
            .map(security_header)
            .unwrap_or_default();
        let request = format!(
            r#"<s:Envelope xmlns:s="http://www.w3.org/2003/05/soap-envelope" xmlns:m="{namespace}" xmlns:t="http://www.onvif.org/ver10/schema">
            <s:Header>{header}</s:Header><s:Body><m:{action}>{body}</m:{action}></s:Body></s:Envelope>"#
        );
        let mut client = Easy::new();
        client.url(endpoint.as_str())?;
        client.timeout(Duration::from_secs(6))?;
        client.connect_timeout(Duration::from_secs(5))?;
        client.follow_location(false)?;
        client.progress(true)?;
        let mut headers = List::new();
        headers.append(&format!(
            r#"Content-Type: application/soap+xml; charset=utf-8; action="{namespace}/{action}""#
        ))?;
        client.http_headers(headers)?;
        client.post_fields_copy(request.as_bytes())?;
        let mut response = Vec::new();
        let mut status = 0;
        // Send the SOAP/WS-Security body first. Selecting Digest preemptively makes
        // libcurl send an empty POST to solicit a challenge, which some cameras reject.
        for authenticate in [false, true] {
            if authenticate {
                let Some(credentials) = &self.credentials else {
                    break;
                };
                let mut auth = Auth::new();
                auth.digest(true).basic(endpoint.scheme() == "https");
                client.http_auth(&auth)?;
                client.username(&credentials.user)?;
                client.password(&credentials.password)?;
            }
            response.clear();
            let result = {
                let mut transfer = client.transfer();
                transfer.progress_function(|_, _, _, _| !self.stop.load(Ordering::Relaxed))?;
                transfer.write_function(|data| {
                    if response.len() + data.len() > 4 * 1024 * 1024 {
                        return Ok(0);
                    }
                    response.extend_from_slice(data);
                    Ok(data.len())
                })?;
                transfer.perform()
            };
            self.check_cancelled()?;
            if let Err(error) = result {
                bail!(
                    "ONVIF {action}: {}",
                    if error.is_operation_timedout() {
                        "timed out"
                    } else {
                        "connection failed"
                    }
                );
            }
            status = client.response_code()?;
            if status != 401 {
                break;
            }
        }
        ensure!(
            (200..300).contains(&status),
            "ONVIF {action}: HTTP {status}"
        );
        let response = String::from_utf8(response)
            .map_err(|_| anyhow::anyhow!("ONVIF {action}: invalid response"))?;
        let document = Document::parse(&response)
            .map_err(|_| anyhow::anyhow!("ONVIF {action}: invalid XML"))?;
        let body = document.descendants().find(|n| named(*n, "Body"));
        ensure!(
            body.is_some_and(|body| !body.children().any(|n| named(n, "Fault"))),
            "ONVIF {action}: invalid response or SOAP fault"
        );
        Ok(response)
    }
}

fn escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

fn security_header(credentials: &Credentials) -> String {
    let nonce: [u8; 20] = rand::random();
    let created = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    let digest = Sha1::new()
        .chain_update(nonce)
        .chain_update(created.as_bytes())
        .chain_update(credentials.password.as_bytes())
        .finalize();
    format!(
        r#"<wsse:Security xmlns:wsse="{SECURITY}-wssecurity-secext-1.0.xsd" xmlns:wsu="{SECURITY}-wssecurity-utility-1.0.xsd">
        <wsse:UsernameToken><wsse:Username>{}</wsse:Username>
        <wsse:Password Type="{SECURITY}-username-token-profile-1.0#PasswordDigest">{}</wsse:Password>
        <wsse:Nonce EncodingType="{SECURITY}-soap-message-security-1.0#Base64Binary">{}</wsse:Nonce>
        <wsu:Created>{created}</wsu:Created></wsse:UsernameToken></wsse:Security>"#,
        escape(&credentials.user),
        STANDARD.encode(digest),
        STANDARD.encode(nonce)
    )
}

fn named(node: Node<'_, '_>, name: &str) -> bool {
    node.is_element() && node.tag_name().name() == name
}

fn child_text<'a>(node: Node<'a, 'a>, name: &str) -> Option<&'a str> {
    node.children()
        .find(|n| named(*n, name))
        .and_then(|n| n.text())
        .map(str::trim)
}

struct Profile {
    token: String,
    pixels: u64,
}

fn profiles(response: &str) -> Result<Vec<Profile>> {
    let document = Document::parse(response)?;
    Ok(document
        .descendants()
        .filter(|n| named(*n, "Profiles"))
        .filter_map(|node| {
            let token = node.attribute("token")?;
            let encoding = node
                .descendants()
                .find(|n| named(*n, "Encoding"))
                .and_then(|n| n.text());
            if encoding
                .is_some_and(|e| !matches!(e.to_uppercase().as_str(), "H264" | "H265" | "HEVC"))
            {
                return None;
            }
            let resolution = node.descendants().find(|n| named(*n, "Resolution"));
            let dimension = |key| {
                resolution
                    .and_then(|n| child_text(n, key))
                    .and_then(|s| s.parse::<u32>().ok())
                    .unwrap_or(0) as u64
            };
            Some(Profile {
                token: token.to_owned(),
                pixels: dimension("Width") * dimension("Height"),
            })
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn incompatible_profiles_are_skipped() {
        let result =
            profiles(r#"<Profiles token="jpeg"><Encoding>JPEG</Encoding></Profiles>"#).unwrap();
        assert!(result.is_empty());
    }

    #[test]
    #[ignore = "requires scripts/test-live.sh fixtures"]
    fn media1_and_media2_keep_wall_and_focus_profiles() {
        let config =
            crate::config::Config::load(std::path::Path::new("tests/fixtures/wall.toml")).unwrap();
        let stop = Arc::new(AtomicBool::new(false));
        let mut cameras: Vec<_> = config
            .cameras
            .into_iter()
            .filter(|c| c.kind == Kind::Onvif)
            .collect();
        let mut digest = cameras[0].clone();
        digest.url = "http://127.0.0.1:18080/digest".into();
        cameras.push(digest);
        for camera in &cameras {
            let streams = resolve(camera, &stop).unwrap();
            assert_eq!(streams.wall.path(), "/sub");
            assert_eq!(streams.focus.unwrap().path(), "/main");
            assert_eq!(streams.wall.username(), "viewer");
        }
    }
}
