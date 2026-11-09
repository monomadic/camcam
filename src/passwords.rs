//! Camera passwords live in macOS Keychain. Configs contain only opaque references.
use anyhow::{Context, Result};
use security_framework::passwords::{
    delete_generic_password, get_generic_password, set_generic_password,
};

const SERVICE: &str = "local.camcam.camera-password";

pub fn load(account: &str) -> Result<String> {
    let bytes = get_generic_password(SERVICE, account)
        .map_err(|_| anyhow::anyhow!("Could not read camera password from Keychain. Unlock your login Keychain or enter the password again."))?;
    String::from_utf8(bytes).context("Camera password in Keychain is invalid")
}

/// New entries are removed if writing the corresponding config fails. Existing entries are
/// never changed, so a failed save leaves the old config and its password usable together.
#[derive(Default)]
pub struct Pending {
    accounts: Vec<String>,
}

impl Pending {
    pub fn store(&mut self, password: &str) -> Result<String> {
        let account = format!("{:032x}", rand::random::<u128>());
        set_generic_password(SERVICE, &account, password.as_bytes()).map_err(|_| {
            anyhow::anyhow!(
                "Could not save camera password in Keychain. Unlock your login Keychain and retry."
            )
        })?;
        self.accounts.push(account.clone());
        Ok(account)
    }

    pub fn commit(mut self) {
        self.accounts.clear();
    }
}

impl Drop for Pending {
    fn drop(&mut self) {
        for account in &self.accounts {
            let _ = delete_generic_password(SERVICE, account);
        }
    }
}

#[cfg(test)]
pub(crate) fn remove_fixture(account: &str) {
    delete_generic_password(SERVICE, account).expect("remove test password");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[ignore = "requires access to the macOS login Keychain"]
    fn reads_passwords_and_rolls_back_uncommitted_entries() {
        let account;
        {
            let mut pending = Pending::default();
            account = pending.store("camcam fixture password").unwrap();
            assert_eq!(load(&account).unwrap(), "camcam fixture password");
        }
        assert!(get_generic_password(SERVICE, &account).is_err());
    }
}
