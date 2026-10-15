//! Optional auto-update (`--features updater`), off unless the user enables it.
//!
//! The update endpoint and signing public key are baked in at build time from
//! `SIMPLE_REVIEW_UPDATE_ENDPOINT` and `SIMPLE_REVIEW_UPDATE_PUBKEY`; a build without them
//! (or without the feature) never contacts anything.

#[derive(Debug, Clone)]
pub struct UpdateInfo {
    pub version: String,
    pub notes: Option<String>,
}

const ENDPOINT: Option<&str> = option_env!("SIMPLE_REVIEW_UPDATE_ENDPOINT");
const PUBKEY: Option<&str> = option_env!("SIMPLE_REVIEW_UPDATE_PUBKEY");

/// Whether this build can update itself.
pub fn available() -> bool {
    cfg!(feature = "updater") && ENDPOINT.is_some_and(|e| !e.is_empty()) && PUBKEY.is_some_and(|k| !k.is_empty())
}

#[cfg(feature = "updater")]
mod imp {
    use std::sync::Mutex;

    use anyhow::{Context, Result, bail};
    use cargo_packager_updater::semver::Version;
    use cargo_packager_updater::url::Url;
    use cargo_packager_updater::{Config, Update, check_update};

    use super::{ENDPOINT, PUBKEY, UpdateInfo};

    /// The update found by the last check, kept for installing.
    static FOUND: Mutex<Option<Update>> = Mutex::new(None);

    pub fn check() -> Result<Option<UpdateInfo>> {
        let (Some(endpoint), Some(pubkey)) = (ENDPOINT, PUBKEY) else { bail!("updates are not configured in this build") };
        let config = Config { endpoints: vec![Url::parse(endpoint).context("invalid update endpoint")?], pubkey: pubkey.to_string(), windows: None };
        let current = Version::parse(env!("CARGO_PKG_VERSION"))?;
        let found = check_update(current, config).context("checking for updates")?;
        let info = found.as_ref().map(|u| UpdateInfo { version: u.version.clone(), notes: u.body.clone() });
        *FOUND.lock().unwrap() = found;
        Ok(info)
    }

    /// Download, verify and install the update found by [`check`], then restart.
    pub fn install_and_restart() -> Result<()> {
        let update = FOUND.lock().unwrap().take().context("no update to install")?;
        update.download_and_install().context("installing the update")?;
        let exe = std::env::current_exe()?;
        std::process::Command::new(exe).spawn()?;
        std::process::exit(0);
    }
}

#[cfg(not(feature = "updater"))]
mod imp {
    use anyhow::{Result, bail};

    use super::UpdateInfo;

    pub fn check() -> Result<Option<UpdateInfo>> {
        bail!("this build has no updater")
    }

    pub fn install_and_restart() -> Result<()> {
        bail!("this build has no updater")
    }
}

pub use imp::{check, install_and_restart};

#[cfg(all(test, feature = "updater"))]
mod tests {
    /// Needs a build with SIMPLE_REVIEW_UPDATE_ENDPOINT pointing at a served latest.json
    /// with a newer version: `cargo test -p simple-review --features updater -- --ignored updater`.
    #[test]
    #[ignore]
    fn finds_a_newer_release() {
        assert!(super::available());
        let info = super::check().unwrap().expect("an update");
        assert_ne!(info.version, env!("CARGO_PKG_VERSION"));
    }
}
