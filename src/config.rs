//! `~/.config/jjsync/config.json` plus an optional `config.local.json`.
//!
//! The shared file is meant to travel with the user's dotfiles (every machine
//! sees the same repo list); the local file never leaves the machine — it adds
//! machine-only repos and can opt out of shared ones via `ignoreRepos`.

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase", default)]
pub struct Config {
    pub interval_seconds: u64,
    /// A repo with no jj operation for this long is dormant.
    pub idle_after_seconds: u64,
    /// How often a dormant repo still runs a cycle.
    pub idle_interval_seconds: u64,
    pub repos: Vec<RepoConfig>,
    /// Repos to drop from the merged list, each entry a path or a directory
    /// name. Meant for config.local.json: "this machine opts out of X".
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub ignore_repos: Vec<String>,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            interval_seconds: 60,
            idle_after_seconds: 7 * 24 * 60 * 60,
            idle_interval_seconds: 15 * 60,
            repos: vec![],
            ignore_repos: vec![],
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase", default)]
pub struct RepoConfig {
    pub path: String,
    /// Clone source for `jjsync bootstrap`; recorded by `jjsync init`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    pub remote: String,
    pub ref_prefix: String,
    pub leak_scan: bool,
    pub exclude_bookmarks: Vec<String>,
}

impl Default for RepoConfig {
    fn default() -> Self {
        RepoConfig {
            path: String::new(),
            url: None,
            remote: "origin".into(),
            ref_prefix: "refs/jj-sync/".into(),
            leak_scan: true,
            exclude_bookmarks: vec![],
        }
    }
}

impl RepoConfig {
    /// refPrefix normalized to end with '/'.
    pub fn prefix(&self) -> String {
        let mut p = self.ref_prefix.clone();
        if !p.ends_with('/') {
            p.push('/');
        }
        p
    }

    pub fn expanded_path(&self) -> PathBuf {
        expand_tilde(&self.path)
    }

    /// Display name for status lines: the repo's own name, taken from the
    /// remote URL — a clone parked in `~/projects/gbandit/main` is "gbandit",
    /// not "main". Falls back to the last path component when no URL is
    /// recorded (pre-`url` configs, or a repo without a remote).
    pub fn name(&self) -> String {
        self.url
            .as_deref()
            .and_then(repo_name_from_url)
            .unwrap_or_else(|| {
                self.expanded_path()
                    .file_name()
                    .map(|s| s.to_string_lossy().into_owned())
                    .unwrap_or_else(|| self.path.clone())
            })
    }
}

/// The repo name inside a git URL: last segment, minus any `.git`. Handles
/// `https://host/org/repo.git` and the scp-like `git@host:org/repo` alike.
pub fn repo_name_from_url(url: &str) -> Option<String> {
    let last = url.trim_end_matches('/').rsplit(['/', ':']).next()?;
    let name = last.strip_suffix(".git").unwrap_or(last);
    (!name.is_empty()).then(|| name.to_string())
}

pub fn expand_tilde(path: &str) -> PathBuf {
    if let Some(rest) = path.strip_prefix("~/") {
        if let Some(home) = std::env::var_os("HOME") {
            return PathBuf::from(home).join(rest);
        }
    }
    PathBuf::from(path)
}

fn config_dir() -> PathBuf {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| expand_tilde("~/.config"));
    base.join("jjsync")
}

pub fn config_path() -> PathBuf {
    config_dir().join("config.json")
}

pub fn local_config_path() -> PathBuf {
    config_dir().join("config.local.json")
}

pub fn state_path() -> PathBuf {
    let base = std::env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| expand_tilde("~/.local/state"));
    base.join("jjsync/status.json")
}

impl Config {
    pub fn load(path: &Path) -> Result<Config> {
        if !path.exists() {
            return Ok(Config::default());
        }
        let text =
            std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
        serde_json::from_str(&text).with_context(|| format!("parsing {}", path.display()))
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
            // The shared config is meant to be tracked in dotfiles; a nested
            // .gitignore keeps the machine-local file from traveling with it.
            let gitignore = dir.join(".gitignore");
            if !gitignore.exists() {
                std::fs::write(&gitignore, "config.local.json\n")
                    .with_context(|| format!("writing {}", gitignore.display()))?;
            }
        }
        let mut text = serde_json::to_string_pretty(self)?;
        text.push('\n');
        std::fs::write(path, text).with_context(|| format!("writing {}", path.display()))
    }

    pub fn find_repo_mut(&mut self, repo_path: &Path) -> Option<&mut RepoConfig> {
        self.repos
            .iter_mut()
            .find(|r| r.expanded_path() == repo_path)
    }

    /// Shared config.json + this machine's config.local.json.
    pub fn load_both() -> Result<(Config, Config)> {
        Ok((
            Config::load(&config_path())?,
            Config::load(&local_config_path())?,
        ))
    }
}

/// The repo list this machine acts on: shared repos plus local ones, minus
/// anything named in either file's `ignoreRepos` (by path, repo name or
/// directory name — the name comes from the remote URL, which can differ).
pub fn effective_repos(shared: &Config, local: &Config) -> Vec<RepoConfig> {
    let ignored = |r: &RepoConfig| {
        shared
            .ignore_repos
            .iter()
            .chain(&local.ignore_repos)
            .any(|i| {
                let path = r.expanded_path();
                *i == r.name()
                    || path.file_name().is_some_and(|d| d == i.as_str())
                    || expand_tilde(i) == path
            })
    };
    shared
        .repos
        .iter()
        .chain(&local.repos)
        .filter(|r| !ignored(r))
        .cloned()
        .collect()
}

/// Simple glob match supporting only `*` (any run of characters).
pub fn glob_match(pattern: &str, name: &str) -> bool {
    fn inner(p: &[u8], n: &[u8]) -> bool {
        match p.first() {
            None => n.is_empty(),
            Some(b'*') => (0..=n.len()).any(|i| inner(&p[1..], &n[i..])),
            Some(c) => n.first() == Some(c) && inner(&p[1..], &n[1..]),
        }
    }
    inner(pattern.as_bytes(), name.as_bytes())
}

/// Walk up from `dir` to the enclosing jj repo root (directory containing .jj).
pub fn find_jj_root(dir: &Path) -> Option<PathBuf> {
    let mut cur = Some(dir);
    while let Some(d) = cur {
        if d.join(".jj").is_dir() {
            return Some(d.to_path_buf());
        }
        cur = d.parent();
    }
    None
}

/// For a non-colocated workspace directory, resolve the main repo path from
/// `.jj/repo` (a plain file containing the path of the main repo's store).
pub fn workspace_main_repo(ws_root: &Path) -> Result<PathBuf> {
    let repo_file = ws_root.join(".jj/repo");
    if !repo_file.is_file() {
        bail!("{} has no .jj/repo pointer", ws_root.display());
    }
    let store = std::fs::read_to_string(&repo_file)?;
    let store = PathBuf::from(store.trim());
    // store is <main>/.jj/repo
    let main = store
        .parent()
        .and_then(|p| p.parent())
        .context("unexpected .jj/repo layout")?;
    Ok(main.to_path_buf())
}
