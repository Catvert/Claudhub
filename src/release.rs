//! Is a newer Claudhub published? Asked once per launch, on the network
//! queue — an HTTP round trip is exactly its profile — and answered to the
//! status bar, which shows a download link when the running version is
//! behind.
//!
//! GitHub's `releases/latest` only names **published** releases: the drafts
//! the CI attaches its files to stay invisible until the tag's release is
//! published, which is the moment an update becomes true. Offline, rate
//! limited, no release yet: all normal days, logged at `debug` and shown
//! nowhere — an update notice is the one message that must never nag.
//!
//! The comparison stays here, pure and tested, but is **applied by the
//! interface** against its own `CARGO_PKG_VERSION`: in remote mode it is the
//! server that fetches, and the version that matters is the window's.

use std::time::Duration;

use anyhow::{Context, Result};

/// The repository the binaries come from.
const REPO: &str = "Catvert/Claudhub";

/// Beyond this, the check is abandoned: it runs on the network queue, and a
/// hung request would keep a fetch waiting.
const TIMEOUT: Duration = Duration::from_secs(10);

/// The latest published release: its version and the page holding the files.
pub struct Latest {
    /// The tag, `v` stripped: `0.7.4`.
    pub version: String,
    /// The release's own page — where the AppImage and the installer are.
    pub url: String,
}

/// Asks GitHub for the latest published release. A subprocess-free network
/// call: never on the interface thread.
pub fn check() -> Result<Latest> {
    let url = format!("https://api.github.com/repos/{REPO}/releases/latest");
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_global(Some(TIMEOUT))
        .build()
        .into();
    let mut response = agent
        .get(&url)
        // Asked for explicitly: GitHub serves other shapes to other Accepts.
        .header("Accept", "application/vnd.github+json")
        .call()
        .with_context(|| url.clone())?;
    let text = response
        .body_mut()
        .read_to_string()
        .with_context(|| format!("unreadable answer from {url}"))?;
    parse(&text).with_context(|| format!("unexpected answer from {url}"))
}

/// Reads the two fields the bar needs out of the release JSON.
fn parse(json: &str) -> Result<Latest> {
    let release: serde_json::Value = serde_json::from_str(json)?;
    let tag = release
        .get("tag_name")
        .and_then(|tag| tag.as_str())
        .context("no tag_name")?;
    let url = release
        .get("html_url")
        .and_then(|url| url.as_str())
        // The list page shows the same files one click further.
        .map(str::to_string)
        .unwrap_or_else(|| format!("https://github.com/{REPO}/releases"));
    Ok(Latest {
        version: tag.strip_prefix('v').unwrap_or(tag).to_string(),
        url,
    })
}

/// Is `latest` strictly ahead of `running`? Anything that does not read as
/// `x.y.z` compares as nothing: an unreadable tag must not paint a download
/// link on every startup.
pub fn is_newer(latest: &str, running: &str) -> bool {
    match (triple(latest), triple(running)) {
        (Some(latest), Some(running)) => latest > running,
        _ => false,
    }
}

fn triple(version: &str) -> Option<(u64, u64, u64)> {
    let mut parts = version.trim().trim_start_matches('v').splitn(3, '.');
    let mut next = || parts.next()?.parse::<u64>().ok();
    Some((next()?, next()?, next()?))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_release_answer_gives_version_and_page() {
        let latest = parse(
            r#"{"tag_name": "v0.7.4", "html_url": "https://github.com/Catvert/Claudhub/releases/tag/v0.7.4", "draft": false}"#,
        )
        .unwrap();
        assert_eq!(latest.version, "0.7.4");
        assert!(latest.url.ends_with("/tag/v0.7.4"));
        assert!(parse(r#"{"message": "Not Found"}"#).is_err());
    }

    /// Strictly ahead, component by component — and an unreadable version
    /// never claims an update.
    #[test]
    fn only_a_strictly_newer_version_is_newer() {
        assert!(is_newer("0.7.4", "0.7.3"));
        assert!(is_newer("0.8.0", "0.7.9"));
        assert!(is_newer("1.0.0", "0.99.99"));
        assert!(!is_newer("0.7.3", "0.7.3"));
        assert!(!is_newer("0.7.2", "0.7.3"));
        // Tags keep their `v`, Cargo versions do not: both read.
        assert!(is_newer("v0.7.4", "0.7.3"));
        assert!(!is_newer("nightly", "0.7.3"));
        assert!(!is_newer("0.7.4", "unknown"));
    }
}
