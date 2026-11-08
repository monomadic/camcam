use anyhow::{Context, Result, bail};
use serde::Deserialize;
use std::path::{Path, PathBuf};
use url::Url;

#[derive(Deserialize)]
pub struct Config {
    pub cameras: Vec<Camera>,
}

#[derive(Clone, Deserialize)]
pub struct Camera {
    pub name: String,
    #[serde(rename = "type")]
    pub kind: Kind,
    pub url: String,
    pub username: Option<String>,
    pub password_env: Option<String>,
}

#[derive(Clone, Copy, Deserialize, PartialEq, Debug)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    Rtmp,
    Rtsp,
    Onvif,
}

#[derive(Clone)]
pub struct Credentials {
    pub user: String,
    pub password: String,
}

#[derive(Clone)]
pub struct Streams {
    pub wall: Url,
    pub focus: Option<Url>,
}

impl Config {
    pub fn load(path: &Path) -> Result<Self> {
        // Parser diagnostics may quote credential-bearing config lines.
        let source =
            std::fs::read_to_string(path).context("Could not read camera configuration")?;
        toml::from_str(&source).map_err(|_| anyhow::anyhow!("Invalid camera configuration"))
    }

    pub fn default_path() -> Result<PathBuf> {
        Ok(
            PathBuf::from(std::env::var_os("HOME").context("HOME is unavailable")?)
                .join(".config/camcam/config.toml"),
        )
    }
}

impl Camera {
    pub fn endpoint(&self) -> Result<(Url, Option<Credentials>)> {
        let url = Url::parse(&self.url).map_err(|_| anyhow::anyhow!("Invalid camera URL"))?;
        let valid_scheme = match self.kind {
            Kind::Rtmp => matches!(url.scheme(), "rtmp" | "rtmps"),
            Kind::Rtsp => url.scheme() == "rtsp",
            Kind::Onvif => matches!(url.scheme(), "http" | "https"),
        };
        if !valid_scheme || url.host_str().is_none() {
            bail!("Invalid {:?} camera URL", self.kind);
        }
        let password = self
            .password_env
            .as_ref()
            .map(|key| {
                std::env::var(key)
                    .map_err(|_| anyhow::anyhow!("Missing password environment variable"))
            })
            .transpose()?
            .unwrap_or_default();
        let credentials = self.username.as_ref().map(|user| Credentials {
            user: user.clone(),
            password,
        });
        Ok((url, credentials))
    }
}

impl Credentials {
    pub fn apply(&self, mut url: Url) -> Url {
        if url.username().is_empty() {
            // A validated network URL always accepts a user and password.
            url.set_username(&self.user)
                .expect("network URL accepts username");
            url.set_password(Some(&self.password))
                .expect("network URL accepts password");
        }
        url
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn invalid_scheme_and_missing_password_are_reported() {
        let mut camera = Camera {
            name: "Invalid camera".into(),
            kind: Kind::Onvif,
            url: "rtmp://127.0.0.1/live".into(),
            username: None,
            password_env: None,
        };
        assert!(camera.endpoint().is_err());
        camera.kind = Kind::Rtsp;
        camera.url = "rtsp://127.0.0.1/live".into();
        camera.password_env = Some("CAMCAM_TEST_UNSET_08DFD9".into());
        let error = camera.endpoint().err().expect("missing password");
        assert_eq!(error.to_string(), "Missing password environment variable");
    }

    #[test]
    fn existing_config_is_unchanged() {
        let config: Config = toml::from_str(include_str!("../config.example.toml")).unwrap();
        assert_eq!(
            config.cameras.iter().map(|c| c.kind).collect::<Vec<_>>(),
            [Kind::Onvif, Kind::Rtmp, Kind::Rtsp]
        );
        assert_eq!(
            config.cameras[0].password_env.as_deref(),
            Some("CAMCAM_FRONT_DOOR_PASSWORD")
        );
        let credentials = Credentials {
            user: "user@home".into(),
            password: "a/b?#@&".into(),
        };
        let url = credentials.apply(Url::parse("rtsp://localhost/main").unwrap());
        assert_eq!(url.path(), "/main");
        assert_eq!(url.username(), "user%40home");
        assert_eq!(url.password(), Some("a%2Fb%3F%23%40&"));
        let existing = Url::parse("rtsp://existing:secret@localhost/main").unwrap();
        assert_eq!(credentials.apply(existing.clone()), existing);
    }
}
