//! "Is there a newer version?" — asks GitHub for the latest release of VTube Collab.
//! Only the version number and its download page are read; nothing about the user is sent.

use std::time::Duration;

const LATEST_RELEASE: &str = "https://api.github.com/repos/WindowsBen/VTube-Collab/releases/latest";
/// Where to download VTube Collab and report problems.
pub const WEBSITE: &str = "https://github.com/WindowsBen/VTube-Collab/releases";
/// Tips for the developer (VTube Collab is free; tipping is entirely optional).
pub const DONATE: &str = "https://streamelements.com/windowsben/tip";

/// A newer version than this one.
#[derive(Debug, Clone)]
pub struct NewVersion {
    /// e.g. "0.4.0-beta"
    pub version: String,
    /// The release's download page.
    pub page: String,
}

/// Ask GitHub for the latest release. `None` if this version is up to date, or if
/// GitHub can't be asked (offline, no releases yet...) — never an error for the user.
pub fn check() -> Option<NewVersion> {
    #[derive(serde::Deserialize)]
    struct Release {
        tag_name: String,
        html_url: String,
    }
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(10)))
        .build()
        .into();
    let mut response = agent
        .get(LATEST_RELEASE)
        .header("Accept", "application/vnd.github+json")
        .header("User-Agent", "VTubeCollab-update-check")
        .call()
        .map_err(|e| crate::log::info(&format!("Update check: couldn't ask GitHub ({e})")))
        .ok()?;
    let text = response.body_mut().read_to_string().ok()?;
    let release: Release = serde_json::from_str(&text).ok()?;
    let latest = release.tag_name.trim_start_matches(['v', 'V']).to_string();
    let newer = is_newer(&latest, env!("CARGO_PKG_VERSION"));
    crate::log::info(&format!("Update check: latest release is {latest}{}", if newer { " (newer)" } else { "" }));
    newer.then_some(NewVersion { version: latest, page: release.html_url })
}

/// Is version `a` newer than `b`? "0.4.0" > "0.4.0-beta" > "0.3.1-beta".
/// Unreadable versions never count as newer.
fn is_newer(a: &str, b: &str) -> bool {
    fn parse(v: &str) -> Option<(Vec<u32>, bool)> {
        let (numbers, pre) = match v.split_once('-') {
            Some((n, _)) => (n, true),
            None => (v, false),
        };
        let numbers = numbers.split('.').map(|n| n.parse().ok()).collect::<Option<Vec<u32>>>()?;
        // A full release ranks above its pre-releases (beta) with the same numbers.
        Some((numbers, !pre))
    }
    match (parse(a), parse(b)) {
        (Some(a), Some(b)) => a > b,
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::is_newer;

    #[test]
    fn version_order() {
        assert!(is_newer("0.4.0-beta", "0.3.0-beta"));
        assert!(is_newer("0.3.1-beta", "0.3.0-beta"));
        assert!(is_newer("0.3.0", "0.3.0-beta"));
        assert!(is_newer("1.0.0", "0.9.9"));
        assert!(is_newer("0.10.0", "0.9.0"));
        assert!(!is_newer("0.3.0-beta", "0.3.0-beta"));
        assert!(!is_newer("0.3.0-beta", "0.3.0"));
        assert!(!is_newer("0.2.1-beta", "0.3.0-beta"));
        assert!(!is_newer("nonsense", "0.3.0-beta"));
    }
}
