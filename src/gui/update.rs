use std::io::Read;
use std::sync::mpsc::{self, Receiver};
use std::time::Duration;

// Only the macOS frontend consumes the downloaded installer fields.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
#[derive(Debug, Clone)]
pub(super) struct UpdateAsset {
    pub download_url: String,
    pub sha256: String,
}

#[derive(Debug, Clone)]
pub(super) enum UpdateCheck {
    UpToDate,
    Available {
        tag: String,
        url: String,
        asset: Option<UpdateAsset>,
    },
    Error(String),
}

const RELEASES_API: &str = "https://api.github.com/repos/martinezooo/keyjitsu/releases?per_page=30";
const RELEASES_WEB: &str = "https://github.com/martinezooo/keyjitsu/releases";
const MAX_RELEASE_RESPONSE_BYTES: u64 = 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct Version {
    major: u64,
    minor: u64,
    patch: u64,
    // alpha < beta < rc < final
    channel: u8,
    number: u64,
}

fn parse_version(value: &str) -> Option<Version> {
    let trimmed = value.trim();
    let signed = trimmed.starts_with("signed-macos-v");
    let tag = if signed {
        let version_and_hash = trimmed.strip_prefix("signed-macos-v")?;
        let (version, hash) = version_and_hash.rsplit_once('-')?;
        if hash.len() < 7 || !hash.bytes().all(|b| b.is_ascii_hexdigit()) {
            return None;
        }
        version
    } else {
        trimmed.strip_prefix('v').unwrap_or(trimmed)
    };
    let mut parts = tag.split('-');
    let core = parts.next()?;
    let mut nums = core.split('.');
    let major = nums.next()?.parse().ok()?;
    let minor = nums.next()?.parse().ok()?;
    let patch = nums.next()?.parse().ok()?;
    if nums.next().is_some() {
        return None;
    }
    let (channel, number) = match parts.next() {
        None => (3, 0),
        Some(label) => {
            let mut labels = label.split('.');
            let channel = match labels.next()? {
                "alpha" => 0,
                "beta" => 1,
                "rc" => 2,
                _ => return None,
            };
            let number = labels.next()?.parse().ok()?;
            if labels.next().is_some() {
                return None;
            }
            (channel, number)
        }
    };
    if parts.next().is_some() {
        return None;
    }
    Some(Version {
        major,
        minor,
        patch,
        channel,
        number,
    })
}

fn newer_release(json: &serde_json::Value, current: &str) -> Result<UpdateCheck, String> {
    let installed = parse_version(current).ok_or("invalid current version")?;
    let releases = json
        .as_array()
        .ok_or("GitHub releases response is not an array")?;
    let mut best: Option<(Version, String, String, Option<UpdateAsset>)> = None;
    for entry in releases {
        if entry.get("draft").and_then(|v| v.as_bool()) == Some(true) {
            continue;
        }
        let Some(tag) = entry.get("tag_name").and_then(|v| v.as_str()) else {
            continue;
        };
        let Some(version) = parse_version(tag) else {
            continue;
        };
        if version <= installed || best.as_ref().is_some_and(|(v, ..)| version <= *v) {
            continue;
        }
        let page = entry
            .get("html_url")
            .and_then(|v| v.as_str())
            .filter(|url| url.starts_with(&format!("{RELEASES_WEB}/tag/")))
            .unwrap_or(RELEASES_WEB);
        let mut asset = None;
        // The legacy v* workflow produces ad-hoc-signed macOS archives.
        // Only the explicitly notarized signed-macos tag is installable.
        if cfg!(target_os = "macos") && tag.starts_with("signed-macos-v") {
            if let Some(assets) = entry.get("assets").and_then(|v| v.as_array()) {
                for item in assets {
                    let Some(name) = item.get("name").and_then(|v| v.as_str()) else {
                        continue;
                    };
                    let Some(url) = item.get("browser_download_url").and_then(|v| v.as_str())
                    else {
                        continue;
                    };
                    let Some(digest) = item.get("digest").and_then(|v| v.as_str()) else {
                        continue;
                    };
                    let expected_prefix = format!("{RELEASES_WEB}/download/{tag}/");
                    if name.starts_with("Keyjitsu-")
                        && name.ends_with("-macos-arm64.zip")
                        && url == format!("{expected_prefix}{name}")
                        && digest.len() == 71
                        && digest.starts_with("sha256:")
                        && digest[7..].bytes().all(|b| b.is_ascii_hexdigit())
                    {
                        asset = Some(UpdateAsset {
                            download_url: url.to_string(),
                            sha256: digest[7..].to_ascii_lowercase(),
                        });
                        break;
                    }
                }
            }
        }
        best = Some((version, tag.to_string(), page.to_string(), asset));
    }
    Ok(match best {
        Some((_, tag, url, asset)) => UpdateCheck::Available { tag, url, asset },
        None => UpdateCheck::UpToDate,
    })
}

pub(super) fn spawn_update_check() -> Receiver<UpdateCheck> {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let result = (|| -> Result<UpdateCheck, String> {
            let response = ureq::get(RELEASES_API)
                .set(
                    "User-Agent",
                    concat!("keyjitsu/", env!("CARGO_PKG_VERSION")),
                )
                .set("Accept", "application/vnd.github+json")
                .timeout(Duration::from_secs(10))
                .call()
                .map_err(|e| e.to_string())?;
            let mut bytes = Vec::new();
            response
                .into_reader()
                .take(MAX_RELEASE_RESPONSE_BYTES + 1)
                .read_to_end(&mut bytes)
                .map_err(|e| e.to_string())?;
            if bytes.len() as u64 > MAX_RELEASE_RESPONSE_BYTES {
                return Err("GitHub Releases response is unexpectedly large".into());
            }
            let releases = serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;
            newer_release(&releases, env!("CARGO_PKG_VERSION"))
        })();
        let _ = tx.send(result.unwrap_or_else(UpdateCheck::Error));
    });
    rx
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn semver_prerelease_order() {
        let cases = [
            ("v0.10.0-beta.3", "0.10.0-beta.4", false),
            ("signed-macos-v0.10.0-beta.5-abcdef1", "0.10.0-beta.4", true),
            ("v0.10.0", "0.10.0-rc.3", true),
            ("signed-macos-v0.10.1-abcdef1", "0.10.0", true),
            ("v0.10.0-beta.4", "0.10.0", false),
            ("v1.0.0", "0.10.0", true),
            ("v0.10.0-alpha.5", "0.10.0-beta.1", false),
        ];
        for (latest, current, expected) in cases {
            assert_eq!(
                parse_version(latest) > parse_version(current),
                expected,
                "{latest} {current}"
            );
        }
        assert!(parse_version("signed-macos-v0.10.0-beta.4-xyz").is_none());
        assert!(parse_version("v0.10.0-garbage").is_none());
    }

    #[test]
    fn signed_macos_release_is_selected() {
        let releases = serde_json::json!([
            {"tag_name":"v0.10.0-beta.3","html_url":"https://github.com/martinezooo/keyjitsu/releases/tag/v0.10.0-beta.3"},
            {"tag_name":"signed-macos-v0.10.0-beta.5-abc1234",
             "html_url":"https://github.com/martinezooo/keyjitsu/releases/tag/signed-macos-v0.10.0-beta.5-abc1234",
             "assets":[{"name":"Keyjitsu-v0.10.0-beta.5-abc1234-macos-arm64.zip",
                        "browser_download_url":"https://github.com/martinezooo/keyjitsu/releases/download/signed-macos-v0.10.0-beta.5-abc1234/Keyjitsu-v0.10.0-beta.5-abc1234-macos-arm64.zip",
                        "digest":format!("sha256:{}", "a".repeat(64))}]}
        ]);
        match newer_release(&releases, "0.10.0-beta.4").unwrap() {
            UpdateCheck::Available { tag, asset, .. } => {
                assert!(tag.contains("beta.5"));
                if cfg!(target_os = "macos") {
                    assert!(asset.is_some());
                }
            }
            other => panic!("wrong result: {other:?}"),
        }
        assert!(matches!(
            newer_release(&releases, "0.10.0-beta.5").unwrap(),
            UpdateCheck::UpToDate
        ));
    }

    #[test]
    fn reject_unverified_asset() {
        let releases = serde_json::json!([{
            "tag_name":"signed-macos-v0.10.0-beta.5-abc1234",
            "assets":[{"name":"Keyjitsu-v0.10.0-beta.5-abc1234-macos-arm64.zip",
                       "browser_download_url":"https://evil.invalid/fake.zip",
                       "digest":format!("sha256:{}", "a".repeat(64))}]
        }]);
        match newer_release(&releases, "0.10.0-beta.4").unwrap() {
            UpdateCheck::Available { asset, .. } => assert!(asset.is_none()),
            other => panic!("wrong result: {other:?}"),
        }
    }

    /// Proves that the real GitHub API exposes a verified signed beta release
    /// to a client that has not yet installed it. Only run when online.
    #[test]
    #[ignore]
    fn signed_release_discovery_live() {
        let response = ureq::get(RELEASES_API)
            .set("User-Agent", "keyjitsu-updater-live-test")
            .timeout(Duration::from_secs(15))
            .call()
            .expect("GitHub release query");
        let mut body = Vec::new();
        response
            .into_reader()
            .take(MAX_RELEASE_RESPONSE_BYTES + 1)
            .read_to_end(&mut body)
            .expect("read release listing");
        assert!(body.len() as u64 <= MAX_RELEASE_RESPONSE_BYTES);
        let listing = serde_json::from_slice(&body).expect("parse release listing");
        match newer_release(&listing, "0.10.0-beta.4").expect("select newest release") {
            UpdateCheck::Available { tag, asset, .. } => {
                assert!(tag.starts_with("signed-macos-v0.10.0-beta.5-"), "{tag}");
                if cfg!(target_os = "macos") {
                    assert!(asset.is_some(), "signed macOS asset not selectable");
                }
            }
            other => panic!("older beta failed to detect signed release: {other:?}"),
        }
    }

    #[test]
    #[ignore]
    fn update_check_live() {
        let rx = spawn_update_check();
        let result = rx
            .recv_timeout(Duration::from_secs(20))
            .expect("checker replied");
        match result {
            UpdateCheck::Error(e) => panic!("update check failed: {e}"),
            other => eprintln!("LIVE RESULT: {other:?}"),
        }
    }
}
