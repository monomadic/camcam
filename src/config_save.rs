//! Preserve existing TOML while appending discovered cameras. Lock cooperating writers and
//! replace the file atomically, including when the user's config is a symlink into dotfiles.
use crate::config::{Camera, Config, Kind};
use anyhow::{Context, Result, ensure};
use std::{
    collections::HashMap,
    fs::{self, OpenOptions},
    io::Write,
    os::{
        fd::AsRawFd,
        unix::fs::{OpenOptionsExt, PermissionsExt},
    },
    path::Path,
};
use toml_edit::{ArrayOfTables, DocumentMut, Item, Table, TableLike, Value};
use url::Url;

pub fn identity(camera: &Camera) -> String {
    let mut url = Url::parse(&camera.url).ok();
    if let Some(url) = &mut url {
        let _ = url.set_username("");
        let _ = url.set_password(None);
        if url.scheme() == "rtsp" && url.port() == Some(554) {
            let _ = url.set_port(None);
        }
        if url.path().is_empty() {
            url.set_path("/");
        }
    }
    format!(
        "{:?}:{}",
        camera.kind,
        url.as_ref().map(Url::as_str).unwrap_or(&camera.url)
    )
}

pub fn append(path: &Path, cameras: &[Camera]) -> Result<usize> {
    if cameras.is_empty() {
        return Ok(0);
    }
    for camera in cameras {
        camera.endpoint()?;
    }
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    fs::create_dir_all(parent).context("Could not create config directory")?;
    let target = if path.symlink_metadata().is_ok() {
        fs::canonicalize(path).context("Could not resolve config path")?
    } else {
        fs::canonicalize(parent)?.join(path.file_name().context("Invalid config path")?)
    };
    let parent = target.parent().context("Invalid config directory")?;
    let lock_name = format!(".{}.lock", target.file_name().unwrap().to_string_lossy());
    let lock = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .open(parent.join(lock_name))?;
    // SAFETY: the file descriptor is owned for the duration of the lock. Closing releases it.
    ensure!(
        unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX) } == 0,
        "Could not lock camera configuration"
    );
    let original = match fs::read_to_string(&target) {
        Ok(source) => source,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(error) => return Err(error).context("Could not read camera configuration"),
    };
    let mut document = original
        .parse::<DocumentMut>()
        .map_err(|_| anyhow::anyhow!("Existing config is invalid; fix it before saving cameras"))?;
    let mut known = HashMap::new();
    let mut count = 0;
    if document.contains_key("cameras") {
        let config: Config = toml::from_str(&original)
            .map_err(|_| anyhow::anyhow!("Existing camera configuration is invalid"))?;
        count = config.cameras.len();
        for (index, camera) in config.cameras.into_iter().enumerate() {
            known.entry(identity(&camera)).or_insert((index, camera));
        }
    } else {
        document["cameras"] = Item::ArrayOfTables(ArrayOfTables::new());
    }
    let mut pending = crate::passwords::Pending::default();
    let mut changed = 0;
    for camera in cameras {
        let id = identity(camera);
        let existing = known.get(&id);
        if existing.is_some_and(|(_, previous)| same_settings(previous, camera)) {
            continue;
        }
        let mut stored = camera.clone();
        if let Some(password) = stored.session_password.take() {
            stored.password_keychain = Some(pending.store(&password)?);
            stored.password_env = None;
        }
        let table = camera_table(&stored);
        let index = if let Some((index, _)) = existing {
            let previous: &mut dyn TableLike = match &mut document["cameras"] {
                Item::ArrayOfTables(entries) => {
                    entries.get_mut(*index).context("Missing camera entry")?
                }
                Item::Value(Value::Array(entries)) => entries
                    .get_mut(*index)
                    .and_then(Value::as_inline_table_mut)
                    .context("Invalid inline camera entry")?,
                _ => anyhow::bail!("Existing cameras must be an array"),
            };
            for key in ["name", "username", "password_env", "password_keychain"] {
                match table.get(key) {
                    Some(value) if previous.get(key).and_then(Item::as_str) != value.as_str() => {
                        let mut value = value.clone();
                        if let Some(old) = previous.get(key).and_then(Item::as_value)
                            && let Some(new) = value.as_value_mut()
                        {
                            *new.decor_mut() = old.decor().clone();
                        }
                        previous.insert(key, value);
                    }
                    None => {
                        previous.remove(key);
                    }
                    _ => {}
                }
            }
            *index
        } else {
            match &mut document["cameras"] {
                Item::ArrayOfTables(entries) => entries.push(table),
                Item::Value(Value::Array(entries)) => entries.push(table.into_inline_table()),
                _ => anyhow::bail!("Existing cameras must be an array"),
            }
            let index = count;
            count += 1;
            index
        };
        known.insert(id, (index, stored));
        changed += 1;
    }
    if changed == 0 {
        return Ok(0);
    }
    let output = document.to_string();
    let _: Config = toml::from_str(&output).context("Could not serialize camera configuration")?;
    let mut temporary = tempfile::NamedTempFile::new_in(parent)?;
    let permissions = fs::metadata(&target)
        .map(|m| m.permissions())
        .unwrap_or_else(|_| fs::Permissions::from_mode(0o600));
    temporary.as_file().set_permissions(permissions)?;
    temporary.write_all(output.as_bytes())?;
    temporary.as_file().sync_all()?;
    // Editors need not honor our lock. Detect an intervening edit before replacing the file.
    let current = fs::read_to_string(&target).or_else(|error| {
        if error.kind() == std::io::ErrorKind::NotFound {
            Ok(String::new())
        } else {
            Err(error)
        }
    })?;
    ensure!(
        current == original,
        "Config changed while saving; try again"
    );
    temporary
        .persist(&target)
        .map_err(|e| e.error)
        .context("Could not replace camera configuration")?;
    pending.commit();
    Ok(changed)
}

fn same_settings(previous: &Camera, camera: &Camera) -> bool {
    if previous.name != camera.name || previous.username != camera.username {
        return false;
    }
    if let Some(password) = &camera.session_password {
        return previous.password_env.is_none()
            && previous.password_keychain.is_some()
            && previous
                .endpoint()
                .ok()
                .and_then(|(_, credentials)| credentials)
                .is_some_and(|credentials| credentials.password == *password);
    }
    previous.password_env == camera.password_env
        && previous.password_keychain == camera.password_keychain
}

fn camera_table(camera: &Camera) -> Table {
    let mut table = Table::new();
    table["name"] = toml_edit::value(&camera.name);
    table["type"] = toml_edit::value(match camera.kind {
        Kind::Onvif => "onvif",
        Kind::Rtsp => "rtsp",
        Kind::Rtmp => "rtmp",
    });
    table["url"] = toml_edit::value(&camera.url);
    if let Some(username) = &camera.username {
        table["username"] = toml_edit::value(username);
    }
    if let Some(variable) = &camera.password_env {
        table["password_env"] = toml_edit::value(variable);
    }
    if let Some(account) = &camera.password_keychain {
        table["password_keychain"] = toml_edit::value(account);
    }
    table
}

#[cfg(test)]
mod tests {
    use super::*;

    fn camera() -> Camera {
        Camera {
            name: "Front door".into(),
            kind: Kind::Rtsp,
            url: "rtsp://192.168.1.40/stream1".into(),
            username: None,
            password_env: None,
            password_keychain: None,
            session_password: None,
        }
    }

    #[test]
    fn updating_a_selected_endpoint_preserves_other_duplicate_entries() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("config.toml");
        std::fs::write(&path, "[[cameras]]\nname='Front door'\ntype='rtsp'\nurl='rtsp://192.168.1.40/stream1'\n[[cameras]]\nname='Other view'\ntype='rtsp'\nurl='rtsp://192.168.1.40/stream1'\n").unwrap();
        let mut selected = camera();
        selected.username = Some("viewer".into());
        assert_eq!(append(&path, &[selected]).unwrap(), 1);
        let saved = Config::load(&path).unwrap();
        assert_eq!(saved.cameras.len(), 2);
        assert_eq!(saved.cameras[0].username.as_deref(), Some("viewer"));
        assert_eq!(saved.cameras[1].name, "Other view");
        assert!(saved.cameras[1].username.is_none());
    }

    #[test]
    #[ignore = "requires access to the macOS login Keychain"]
    fn replaces_mistaken_environment_credentials_and_reloads_keychain_passwords() {
        let directory = tempfile::tempdir().unwrap();
        for (name, source) in [
            (
                "table",
                "# Keep comment\n[[cameras]]\nname='Front door' # keep name comment\ntype='rtsp'\nurl='rtsp://192.168.1.40/stream1'\nusername='viewer'\npassword_env='mistaken-literal-password'\n",
            ),
            (
                "inline",
                "# Keep comment\ncameras=[{name='Front door',type='rtsp',url='rtsp://192.168.1.40/stream1',username='viewer',password_env='mistaken-literal-password'}]\n",
            ),
        ] {
            let path = directory.path().join(name);
            std::fs::write(&path, source).unwrap();
            let mut camera = camera();
            camera.username = Some("viewer".into());
            camera.session_password = Some("dummy typed password".into());
            assert_eq!(append(&path, &[camera.clone()]).unwrap(), 1);
            let config = Config::load(&path).unwrap();
            assert_eq!(config.cameras.len(), 1);
            let saved = &config.cameras[0];
            assert!(saved.password_env.is_none());
            let account = saved.password_keychain.as_deref().unwrap();
            assert_eq!(
                saved.endpoint().unwrap().1.unwrap().password,
                "dummy typed password"
            );
            assert_eq!(append(&path, &[camera.clone()]).unwrap(), 0);
            let content = std::fs::read_to_string(&path).unwrap();
            assert!(content.contains("# Keep comment"));
            assert!(!content.contains("dummy typed password"));
            assert!(!content.contains("mistaken-literal-password"));
            if name == "table" {
                assert!(content.contains("# keep name comment"));
            }

            camera.session_password = Some("replacement password".into());
            assert_eq!(append(&path, &[camera]).unwrap(), 1);
            let updated = Config::load(&path).unwrap();
            assert_eq!(updated.cameras.len(), 1);
            assert_eq!(
                updated.cameras[0].endpoint().unwrap().1.unwrap().password,
                "replacement password"
            );
            crate::passwords::remove_fixture(
                updated.cameras[0].password_keychain.as_deref().unwrap(),
            );
            crate::passwords::remove_fixture(account);
        }
    }

    #[test]
    fn appends_preserving_comments_and_skips_duplicates() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("config.toml");
        let original = "# Keep my cameras\n[[cameras]]\nname = 'Old camera' # custom name\ntype = 'rtsp'\nurl = 'rtsp://old/live'\n";
        fs::write(&path, original).unwrap();
        assert_eq!(append(&path, &[camera(), camera()]).unwrap(), 1);
        let result = fs::read_to_string(&path).unwrap();
        assert!(result.contains("# Keep my cameras"));
        assert!(result.contains("# custom name"));
        assert_eq!(Config::load(&path).unwrap().cameras.len(), 2);
        let mut default_port = camera();
        default_port.url = "rtsp://192.168.1.40:554/stream1".into();
        assert_eq!(append(&path, &[default_port]).unwrap(), 0);
        assert_eq!(fs::read_to_string(&path).unwrap(), result);
    }

    #[test]
    fn supports_new_empty_and_inline_configs_and_preserves_symlinks() {
        let directory = tempfile::tempdir().unwrap();
        for (name, content) in [
            ("empty", "cameras = []\n"),
            (
                "inline",
                "cameras = [{name='Old', type='rtsp', url='rtsp://old/live'}]\n",
            ),
        ] {
            let path = directory.path().join(name);
            fs::write(&path, content).unwrap();
            assert_eq!(append(&path, &[camera()]).unwrap(), 1);
            Config::load(&path).unwrap();
        }
        let target = directory.path().join("nested/config.toml");
        assert_eq!(append(&target, &[camera()]).unwrap(), 1);
        let link = directory.path().join("linked.toml");
        std::os::unix::fs::symlink(&target, &link).unwrap();
        let mut second = camera();
        second.url = "rtsp://other/stream".into();
        append(&link, &[second]).unwrap();
        assert!(link.symlink_metadata().unwrap().file_type().is_symlink());
        assert_eq!(Config::load(&target).unwrap().cameras.len(), 2);
    }

    #[test]
    fn invalid_config_is_not_overwritten_and_concurrent_saves_merge() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("config.toml");
        fs::write(&path, "cameras = [broken").unwrap();
        assert!(append(&path, &[camera()]).is_err());
        assert_eq!(fs::read_to_string(&path).unwrap(), "cameras = [broken");
        fs::remove_file(&path).unwrap();
        std::thread::scope(|scope| {
            for index in 0..4 {
                let path = &path;
                scope.spawn(move || {
                    let mut camera = camera();
                    camera.url = format!("rtsp://host{index}/live");
                    append(path, &[camera]).unwrap();
                });
            }
        });
        assert_eq!(Config::load(&path).unwrap().cameras.len(), 4);
    }
}
