use std::io::Read;
use std::sync::mpsc::{self, Receiver};
use std::time::Duration;

#[derive(Debug, Clone)]
pub(super) enum UpdateCheck {
    UpToDate,
    Available { tag: String, url: String },
    Error(String),
}

const RELEASES_API: &str = "https://api.github.com/repos/martinezooo/keyjitsu/releases/latest";
const RELEASES_WEB: &str = "https://github.com/martinezooo/keyjitsu/releases";
const MAX_RELEASE_RESPONSE_BYTES: u64 = 1024 * 1024;

/// Read the newest GitHub release on a background thread. This never downloads
/// or installs release artifacts.
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
                .timeout(Duration::from_secs(8))
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
            let resp: serde_json::Value =
                serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;
            let tag = resp
                .get("tag_name")
                .and_then(|t| t.as_str())
                .ok_or("no tag_name in the response")?
                .to_string();
            let url = resp
                .get("html_url")
                .and_then(|u| u.as_str())
                .filter(|u| u.starts_with(RELEASES_WEB))
                .unwrap_or(RELEASES_WEB)
                .to_string();
            Ok(if version_newer(&tag, env!("CARGO_PKG_VERSION")) {
                UpdateCheck::Available { tag, url }
            } else {
                UpdateCheck::UpToDate
            })
        })();
        let _ = tx.send(result.unwrap_or_else(UpdateCheck::Error));
    });
    rx
}

fn version_newer(latest: &str, current: &str) -> bool {
    fn parts(v: &str) -> [u64; 3] {
        let mut out = [0u64; 3];
        for (i, p) in v
            .trim()
            .trim_start_matches('v')
            .split('.')
            .take(3)
            .enumerate()
        {
            out[i] = p
                .chars()
                .take_while(|c| c.is_ascii_digit())
                .collect::<String>()
                .parse()
                .unwrap_or(0);
        }
        out
    }
    parts(latest) > parts(current)
}

#[cfg(test)]
mod tests {
    use super::{spawn_update_check, version_newer, UpdateCheck};

    #[test]
    fn version_compare() {
        assert!(version_newer("v0.9.2", "0.9.1"));
        assert!(version_newer("1.0.0", "0.9.9"));
        assert!(!version_newer("v0.9.1", "0.9.1"));
        assert!(!version_newer("0.9.0", "0.9.1"));
        assert!(!version_newer("garbage", "0.9.1"));
        assert!(version_newer("v0.10.0-beta", "0.9.1"));
    }

    /// Exercises the real GitHub request and parsing. Kept ignored so normal
    /// tests stay deterministic and offline.
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

    use std::time::Duration;
}
