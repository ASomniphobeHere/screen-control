//! Get this PC's config from the private config repo on GitHub: `pcs/<pc_name>.json`.

use std::collections::BTreeMap;
use std::sync::mpsc::{self, RecvTimeoutError, Sender};
use std::thread;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use base64::Engine as _;
use serde::Deserialize;
use serde_json::Value;

use crate::app::Event;

/// Time between checks for a new config.
pub const INTERVAL: Duration = Duration::from_secs(10 * 60);

pub enum SyncResult {
    Unchanged {
        sha: String,
    },
    /// `sha` is the file's SHA, `commit` the last commit that changed it, if known.
    Changed {
        sha: String,
        commit: Option<String>,
        text: String,
    },
    Failed(String),
}

pub struct Repo {
    client: reqwest::blocking::Client,
    /// `owner/repo`.
    repo: String,
    path: String,
    token: String,
}

#[derive(Deserialize)]
struct Contents {
    sha: String,
    content: String,
}

#[derive(Deserialize)]
struct Commit {
    sha: String,
}

impl Repo {
    pub fn new(token: &str, repo: &str, pc_name: &str) -> Result<Repo> {
        if repo.split('/').filter(|p| !p.is_empty()).count() != 2 {
            bail!("the config repo must be written as owner/repo, not \"{repo}\"");
        }
        let client = reqwest::blocking::Client::builder()
            .timeout(Duration::from_secs(30))
            .user_agent("screen-control")
            .build()
            .context("cannot make the HTTP client")?;
        Ok(Repo {
            client,
            repo: repo.to_string(),
            path: format!("pcs/{pc_name}.json"),
            token: token.to_string(),
        })
    }

    fn get<T: serde::de::DeserializeOwned>(&self, url: &str) -> Result<T> {
        let response = self
            .client
            .get(url)
            .bearer_auth(&self.token)
            .header("Accept", "application/vnd.github+json")
            .header("X-GitHub-Api-Version", "2022-11-28")
            .send()
            .context("cannot reach GitHub")?;
        let status = response.status();
        if !status.is_success() {
            let message = response
                .json::<Value>()
                .ok()
                .and_then(|v| v["message"].as_str().map(str::to_string))
                .unwrap_or_default();
            match status.as_u16() {
                404 => bail!(
                    "{} not found in {} (or the token cannot read the repo)",
                    self.path,
                    self.repo
                ),
                401 => bail!("GitHub did not accept the token: {message}"),
                _ => bail!("GitHub answered {status}: {message}"),
            }
        }
        response.json().context("bad answer from GitHub")
    }

    /// Returns the file's SHA and text.
    pub fn fetch(&self) -> Result<(String, String)> {
        let url = format!(
            "https://api.github.com/repos/{}/contents/{}",
            self.repo, self.path
        );
        let contents: Contents = self.get(&url)?;
        let base64: String = contents.content.split_whitespace().collect();
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(base64)
            .context("bad base64 from GitHub")?;
        let text = String::from_utf8(bytes).context("the config is not UTF-8")?;
        Ok((contents.sha, text))
    }

    /// The last commit that changed the file. Only used in messages, so errors are ignored.
    fn last_commit(&self) -> Option<String> {
        let url = format!(
            "https://api.github.com/repos/{}/commits?path={}&per_page=1",
            self.repo, self.path
        );
        let commits: Vec<Commit> = self.get(&url).ok()?;
        commits.into_iter().next().map(|c| c.sha)
    }

    fn check(&self, known_sha: Option<&str>) -> SyncResult {
        match self.fetch() {
            Ok((sha, _)) if Some(sha.as_str()) == known_sha => SyncResult::Unchanged { sha },
            Ok((sha, text)) => SyncResult::Changed {
                commit: self.last_commit(),
                sha,
                text,
            },
            Err(e) => SyncResult::Failed(format!("{e:#}")),
        }
    }
}

/// Starts the checking thread. Send a parent's Telegram ID to the returned sender to check at once;
/// that parent then always gets an answer.
pub fn start(repo: Repo, known_sha: Option<String>, events: Sender<Event>) -> Result<Sender<i64>> {
    let (requests, rx) = mpsc::channel::<i64>();
    thread::Builder::new()
        .name("config-sync".into())
        .spawn(move || {
            let mut known = known_sha;
            let mut requested_by = None;
            loop {
                let result = repo.check(known.as_deref());
                match &result {
                    SyncResult::Changed { sha, .. } => known = Some(sha.clone()),
                    SyncResult::Failed(e) => {
                        tracing::warn!(error = %e, "cannot check for a new config")
                    }
                    SyncResult::Unchanged { .. } => tracing::debug!("config unchanged"),
                }
                // Scheduled checks only report changes. A parent's /reload always gets an answer.
                let report = requested_by.is_some() || matches!(result, SyncResult::Changed { .. });
                if report
                    && events
                        .send(Event::ConfigSync {
                            result,
                            requested_by,
                        })
                        .is_err()
                {
                    return; // The main loop has ended.
                }
                requested_by = match rx.recv_timeout(INTERVAL) {
                    Ok(parent) => Some(parent),
                    Err(RecvTimeoutError::Timeout) => None,
                    Err(RecvTimeoutError::Disconnected) => return,
                };
            }
        })
        .context("cannot start the config thread")?;
    Ok(requests)
}

/// Lines like `children[0].limits.default: 120 -> 90` for each value that changed.
pub fn diff(old: &str, new: &str) -> Vec<String> {
    let (Ok(old), Ok(new)) = (
        serde_json::from_str::<Value>(old),
        serde_json::from_str::<Value>(new),
    ) else {
        return vec!["(cannot compare)".to_string()];
    };
    let (mut a, mut b) = (BTreeMap::new(), BTreeMap::new());
    flatten(&old, String::new(), &mut a);
    flatten(&new, String::new(), &mut b);
    let mut lines = Vec::new();
    for (path, old_value) in &a {
        match b.get(path) {
            Some(new_value) if new_value != old_value => {
                lines.push(format!("{path}: {old_value} -> {new_value}"))
            }
            None => lines.push(format!("{path}: {old_value} -> (removed)")),
            _ => {}
        }
    }
    for (path, new_value) in &b {
        if !a.contains_key(path) {
            lines.push(format!("{path}: (new) {new_value}"));
        }
    }
    lines
}

fn flatten(value: &Value, path: String, out: &mut BTreeMap<String, String>) {
    match value {
        Value::Object(map) => {
            for (key, v) in map {
                let p = if path.is_empty() {
                    key.clone()
                } else {
                    format!("{path}.{key}")
                };
                flatten(v, p, out);
            }
        }
        Value::Array(items) => {
            for (i, v) in items.iter().enumerate() {
                flatten(v, format!("{path}[{i}]"), out);
            }
        }
        other => {
            out.insert(path, other.to_string());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn diff_lists_changed_new_and_removed_values() {
        let old = r#"{ "a": 1, "children": [ { "limits": { "default": 120, "sat": 240 } } ] }"#;
        let new = r#"{ "a": 1, "children": [ { "limits": { "default": 90 } } ], "b": "x" }"#;
        assert_eq!(
            diff(old, new),
            [
                "children[0].limits.default: 120 -> 90",
                "children[0].limits.sat: 240 -> (removed)",
                "b: (new) \"x\"",
            ]
        );
    }

    #[test]
    fn same_values_in_other_layout_give_no_diff() {
        assert!(diff(r#"{"a":1,"b":[1,2]}"#, "{\n  \"b\": [1, 2],\n  \"a\": 1\n}").is_empty());
    }

    #[test]
    fn bad_repo_name() {
        assert!(Repo::new("t", "just-a-name", "PC-1").is_err());
        assert!(Repo::new("t", "owner/repo", "PC-1").is_ok());
    }
}
