//! Git plumbing: raw refs, CAS pushes, namespace fetches.
//!
//! Local ref layout (per repo):
//!   refs/jj-sync/r/<item>   mirror of the remote namespace as of last fetch
//!   refs/jj-sync/s/<item>   S — the sync-ref set after the last successful cycle
//! where <item> is `<workspace>`, `heads/<sha>`, or `bookmarks/<name>`,
//! exactly the wire shape under the repo's refPrefix.

use crate::exec::{run, run_with_stdin, Env, Output};
use anyhow::{bail, Result};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

pub const REMOTE_NS: &str = "refs/jj-sync/r/";
pub const S_NS: &str = "refs/jj-sync/s/";

pub struct Git<'a> {
    pub dir: PathBuf,
    pub env: &'a Env,
}

/// One CAS ref write on the sync remote. `new: None` deletes the ref.
/// `expected: None` means "must not exist yet".
#[derive(Debug, Clone)]
pub struct RefPush {
    /// Item name relative to the refPrefix, e.g. "default" or "heads/<sha>".
    pub item: String,
    pub new: Option<String>,
    pub expected: Option<String>,
}

#[derive(Debug)]
pub enum NetOutcome {
    Ok,
    /// CAS lease failed: the remote moved between fetch and push.
    LeaseFailed(String),
    Auth(String),
    Offline(String),
    Other(String),
}

fn classify_failure(stderr: &str) -> NetOutcome {
    let s = stderr;
    let auth = [
        "Authentication failed",
        "could not read Username",
        "Permission denied",
        "access denied",
        "HTTP 403",
        "HTTP 401",
        "The requested URL returned error: 403",
        "The requested URL returned error: 401",
    ];
    let offline = [
        "Could not resolve host",
        "unable to access",
        "Connection refused",
        "Connection timed out",
        "Network is unreachable",
        "Operation timed out",
        "Failed to connect",
    ];
    if s.contains("stale info") {
        return NetOutcome::LeaseFailed(s.trim().into());
    }
    if auth.iter().any(|p| s.contains(p)) {
        return NetOutcome::Auth(s.trim().into());
    }
    if offline.iter().any(|p| s.contains(p)) {
        return NetOutcome::Offline(s.trim().into());
    }
    NetOutcome::Other(s.trim().into())
}

impl<'a> Git<'a> {
    pub fn new(dir: &Path, env: &'a Env) -> Self {
        Git {
            dir: dir.to_path_buf(),
            env,
        }
    }

    fn git(&self, args: &[&str]) -> Result<Output> {
        run("git", args, &self.dir, self.env)
    }

    fn git_ok(&self, args: &[&str]) -> Result<Output> {
        let out = self.git(args)?;
        if !out.ok() {
            bail!("`git {}` failed: {}", args.join(" "), out.stderr.trim());
        }
        Ok(out)
    }

    pub fn ref_sha(&self, name: &str) -> Result<Option<String>> {
        let out = self.git(&["rev-parse", "--verify", "--quiet", name])?;
        if out.ok() {
            Ok(Some(out.stdout.trim().to_string()))
        } else {
            Ok(None)
        }
    }

    /// All refs under `prefix`, keyed by the name with the prefix stripped.
    pub fn refs_with_prefix(&self, prefix: &str) -> Result<BTreeMap<String, String>> {
        let out = self.git_ok(&[
            "for-each-ref",
            "--format=%(objectname) %(refname)",
            prefix.trim_end_matches('/'),
        ])?;
        let mut map = BTreeMap::new();
        for line in out.stdout.lines() {
            if let Some((sha, name)) = line.split_once(' ') {
                if let Some(rel) = name.strip_prefix(prefix) {
                    map.insert(rel.to_string(), sha.to_string());
                }
            }
        }
        Ok(map)
    }

    /// Batch-update local refs. `None` deletes.
    pub fn update_refs(&self, updates: &[(String, Option<String>)]) -> Result<()> {
        if updates.is_empty() {
            return Ok(());
        }
        let mut input = String::new();
        for (name, sha) in updates {
            match sha {
                Some(sha) => input.push_str(&format!("update {name} {sha}\n")),
                None => input.push_str(&format!("delete {name}\n")),
            }
        }
        let out = run_with_stdin(
            "git",
            &["update-ref", "--stdin"],
            &self.dir,
            self.env,
            Some(&input),
        )?;
        if !out.ok() {
            bail!("git update-ref --stdin failed: {}", out.stderr.trim());
        }
        Ok(())
    }

    /// Fetch the sync namespace into refs/jj-sync/r/*, pruning refs deleted remotely.
    pub fn fetch_namespace(&self, remote: &str, prefix: &str) -> Result<NetOutcome> {
        let refspec = format!("+{prefix}*:{REMOTE_NS}*");
        let out = self.git(&["fetch", "--prune", "--quiet", remote, &refspec])?;
        if out.ok() {
            Ok(NetOutcome::Ok)
        } else {
            Ok(classify_failure(&out.stderr))
        }
    }

    /// Atomic CAS push of a batch of ref updates/deletions. Never a plain force.
    pub fn push(&self, remote: &str, prefix: &str, pushes: &[RefPush]) -> Result<NetOutcome> {
        if pushes.is_empty() {
            return Ok(NetOutcome::Ok);
        }
        let mut args: Vec<String> = vec![
            "push".into(),
            "--quiet".into(),
            "--atomic".into(),
            remote.into(),
        ];
        for p in pushes {
            let full = format!("{prefix}{}", p.item);
            args.push(format!(
                "--force-with-lease={full}:{}",
                p.expected.as_deref().unwrap_or("")
            ));
            match &p.new {
                Some(sha) => args.push(format!("{sha}:{full}")),
                None => args.push(format!(":{full}")),
            }
        }
        let argrefs: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
        let out = self.git(&argrefs)?;
        if out.ok() {
            Ok(NetOutcome::Ok)
        } else {
            Ok(classify_failure(&out.stderr))
        }
    }

    pub fn is_ancestor(&self, ancestor: &str, descendant: &str) -> Result<bool> {
        let out = self.git(&["merge-base", "--is-ancestor", ancestor, descendant])?;
        Ok(out.status == 0)
    }

    /// Commits reachable from `tips` but not from `bases`.
    pub fn rev_list_not(&self, tips: &[String], bases: &[String]) -> Result<Vec<String>> {
        if tips.is_empty() {
            return Ok(vec![]);
        }
        let mut args: Vec<&str> = vec!["rev-list"];
        args.extend(tips.iter().map(|s| s.as_str()));
        if !bases.is_empty() {
            args.push("--not");
            args.extend(bases.iter().map(|s| s.as_str()));
        }
        let out = self.git_ok(&args)?;
        Ok(out
            .stdout
            .lines()
            .map(|l| l.trim().to_string())
            .filter(|l| !l.is_empty())
            .collect())
    }
}
