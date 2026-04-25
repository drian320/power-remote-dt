//! Phase 4 G4 auto-update against GitHub Releases via `self_update`.
//!
//! Public surface:
//! - `compare_versions(current, latest)` — returns the relative ordering
//!   parsed via semver (Some(Less) means an update is available).
//! - `should_check_now(last, interval_days)` — policy gate.
//! - `check_async()` — async wrapper around blocking `self_update::Update`.
//! - `install_async(release)` — downloads the new MSI and starts msiexec.

use std::cmp::Ordering;
use std::time::{Duration, SystemTime};

#[derive(Debug, Clone, PartialEq, Default)]
pub enum CheckStatus {
    #[default]
    Idle,
    Checking,
    UpToDate,
    Available { version: String, download_url: String },
    Error(String),
}

#[derive(thiserror::Error, Debug)]
pub enum UpdateError {
    #[error("self_update: {0}")]
    SelfUpdate(String),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
}

/// Parse two version strings (with optional leading "v") and return their
/// ordering. Returns None if either fails to parse.
pub fn compare_versions(current: &str, latest: &str) -> Option<Ordering> {
    let c = semver::Version::parse(current.trim_start_matches('v')).ok()?;
    let l = semver::Version::parse(latest.trim_start_matches('v')).ok()?;
    Some(c.cmp(&l))
}

/// Decide whether to check for updates now based on the last-checked
/// timestamp and a 7-day-style interval.
pub fn should_check_now(last: Option<SystemTime>, interval_days: u32) -> bool {
    match last {
        None => true,
        Some(t) => SystemTime::now()
            .duration_since(t)
            .map(|d| d > Duration::from_secs(interval_days as u64 * 86_400))
            .unwrap_or(true),
    }
}

/// Run an async GitHub Releases check. Wraps the blocking self_update API
/// in `spawn_blocking` so the egui main thread never stalls.
pub async fn check_async() -> CheckStatus {
    let result: Result<CheckStatus, UpdateError> = tokio::task::spawn_blocking(|| {
        let updater = self_update::backends::github::Update::configure()
            .repo_owner("power-remote-dt")
            .repo_name("power-remote-dt")
            .bin_name("prdt-host")
            .current_version(env!("CARGO_PKG_VERSION"))
            .build()
            .map_err(|e| UpdateError::SelfUpdate(format!("configure: {e}")))?;
        let rel = updater
            .get_latest_release()
            .map_err(|e| UpdateError::SelfUpdate(format!("get_latest_release: {e}")))?;
        let status = match compare_versions(env!("CARGO_PKG_VERSION"), &rel.version) {
            Some(Ordering::Less) => {
                let url = rel
                    .assets
                    .iter()
                    .find(|a| a.name.ends_with(".msi"))
                    .map(|a| a.download_url.clone())
                    .unwrap_or_default();
                CheckStatus::Available {
                    version: rel.version,
                    download_url: url,
                }
            }
            _ => CheckStatus::UpToDate,
        };
        Ok(status)
    })
    .await
    .unwrap_or_else(|e| Err(UpdateError::SelfUpdate(format!("join: {e}"))));

    match result {
        Ok(s) => s,
        Err(e) => CheckStatus::Error(format!("{e}")),
    }
}

/// Download an MSI to a temp path and hand off to msiexec for in-place
/// upgrade. The current process should exit shortly after this returns Ok
/// — msiexec needs the old binary to be unloaded to overwrite it.
pub async fn install_async(download_url: String) -> Result<(), UpdateError> {
    let download_url_for_log = download_url.clone();
    let result: Result<std::path::PathBuf, UpdateError> = tokio::task::spawn_blocking(move || {
        let tmp_dir = tempfile::tempdir().map_err(UpdateError::Io)?;
        // Keep the directory alive past this scope — msiexec reads from it
        // after we exit. `keep()` releases the auto-delete guard.
        let tmp_root = tmp_dir.keep();
        let tmp_path = tmp_root.join("prdt-update.msi");
        let mut reader = ureq::get(&download_url)
            .call()
            .map_err(|e| UpdateError::SelfUpdate(format!("download: {e}")))?
            .into_reader();
        let mut file = std::fs::File::create(&tmp_path)?;
        std::io::copy(&mut reader, &mut file)?;
        Ok(tmp_path)
    })
    .await
    .unwrap_or_else(|e| Err(UpdateError::SelfUpdate(format!("join: {e}"))));

    let tmp_path = result?;
    tracing::info!(
        url = %download_url_for_log,
        path = %tmp_path.display(),
        "installer downloaded; spawning msiexec"
    );

    #[cfg(target_os = "windows")]
    {
        std::process::Command::new("msiexec")
            .arg("/i")
            .arg(&tmp_path)
            .arg("/qb")
            .arg("/norestart")
            .spawn()
            .map_err(UpdateError::Io)?;
    }
    #[cfg(not(target_os = "windows"))]
    {
        let _ = tmp_path;
        return Err(UpdateError::SelfUpdate(
            "MSI install only supported on Windows".into(),
        ));
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn newer_version_is_less() {
        assert_eq!(compare_versions("0.0.1", "0.0.2"), Some(Ordering::Less));
    }

    #[test]
    fn older_version_is_greater() {
        assert_eq!(compare_versions("0.0.2", "0.0.1"), Some(Ordering::Greater));
    }

    #[test]
    fn equal_versions_are_equal() {
        assert_eq!(compare_versions("0.0.1", "0.0.1"), Some(Ordering::Equal));
    }

    #[test]
    fn v_prefix_is_tolerated() {
        assert_eq!(compare_versions("v0.1.0", "v0.2.0"), Some(Ordering::Less));
        assert_eq!(compare_versions("v0.2.0", "0.1.0"), Some(Ordering::Greater));
    }

    #[test]
    fn invalid_version_returns_none() {
        assert_eq!(compare_versions("not.a.version", "0.0.1"), None);
        assert_eq!(compare_versions("0.0.1", "garbage"), None);
    }

    #[test]
    fn never_checked_should_check() {
        assert!(should_check_now(None, 7));
    }

    #[test]
    fn just_checked_should_not_check() {
        assert!(!should_check_now(Some(SystemTime::now()), 7));
    }

    #[test]
    fn old_check_should_check_again() {
        let old = SystemTime::now() - Duration::from_secs(8 * 86_400);
        assert!(should_check_now(Some(old), 7));
    }

    #[test]
    fn future_timestamp_treated_as_should_check() {
        let future = SystemTime::now() + Duration::from_secs(86_400);
        assert!(should_check_now(Some(future), 7));
    }
}
