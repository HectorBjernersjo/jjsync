//! Jujutsu wrapper. Queries pass --ignore-working-copy so the cycle acts on
//! the snapshot it took at the start; mutations snapshot first (jj default),
//! so edits the user made mid-cycle are amended in before @ moves.

use crate::exec::{run, Env, Output};
use anyhow::{bail, Result};
use std::path::{Path, PathBuf};

pub struct Jj<'a> {
    pub dir: PathBuf,
    pub env: &'a Env,
}

impl<'a> Jj<'a> {
    pub fn new(dir: &Path, env: &'a Env) -> Self {
        Jj {
            dir: dir.to_path_buf(),
            env,
        }
    }

    fn run_raw(&self, args: &[&str]) -> Result<Output> {
        let mut full = vec!["--color", "never"];
        full.extend_from_slice(args);
        run("jj", &full, &self.dir, self.env)
    }

    fn jj(&self, args: &[&str]) -> Result<Output> {
        let out = self.run_raw(args)?;
        if !out.ok() && out.stderr.contains("stale") && out.stderr.contains("working copy") {
            // Another workspace's operation moved this one forward; heal and retry.
            self.run_raw(&["workspace", "update-stale"])?;
            return self.run_raw(args);
        }
        Ok(out)
    }

    fn jj_ok(&self, args: &[&str]) -> Result<Output> {
        let out = self.jj(args)?;
        if !out.ok() {
            bail!("`jj {}` failed: {}", args.join(" "), out.stderr.trim());
        }
        Ok(out)
    }

    pub fn snapshot(&self) -> Result<()> {
        self.jj_ok(&["util", "snapshot"]).map(|_| ())
    }

    /// All workspaces in the repo: (name, working-copy commit id).
    pub fn workspaces(&self) -> Result<Vec<(String, String)>> {
        let out = self.jj_ok(&[
            "workspace",
            "list",
            "--ignore-working-copy",
            "-T",
            r#"name ++ " " ++ target.commit_id() ++ "\n""#,
        ])?;
        Ok(out
            .stdout
            .lines()
            .filter_map(|l| {
                l.split_once(' ')
                    .map(|(n, s)| (n.to_string(), s.trim().to_string()))
            })
            .collect())
    }

    /// Root directory of a workspace by name (jj records each workspace's
    /// path in the repo's workspace store). None when the workspace's
    /// directory no longer resolves on this machine.
    pub fn workspace_root(&self, name: &str) -> Option<PathBuf> {
        self.jj_ok(&["workspace", "root", "--ignore-working-copy", "--name", name])
            .ok()
            .map(|o| PathBuf::from(o.stdout.trim()))
    }

    /// Commit id of this directory's workspace working copy.
    pub fn wc_commit(&self) -> Result<String> {
        let out = self.jj_ok(&[
            "log",
            "--ignore-working-copy",
            "--no-graph",
            "-r",
            "@",
            "-T",
            "commit_id",
        ])?;
        Ok(out.stdout.trim().to_string())
    }

    pub fn log_shas(&self, revset: &str) -> Result<Vec<String>> {
        let out = self.jj_ok(&[
            "log",
            "--ignore-working-copy",
            "--no-graph",
            "-r",
            revset,
            "-T",
            r#"commit_id ++ "\n""#,
        ])?;
        Ok(out
            .stdout
            .lines()
            .map(|l| l.trim().to_string())
            .filter(|l| !l.is_empty())
            .collect())
    }

    /// (commit id, change id) of every visible commit.
    pub fn visible_commits(&self) -> Result<Vec<(String, String)>> {
        let out = self.jj_ok(&[
            "log",
            "--ignore-working-copy",
            "--no-graph",
            "-r",
            "all()",
            "-T",
            r#"commit_id ++ " " ++ change_id ++ "\n""#,
        ])?;
        Ok(out
            .stdout
            .lines()
            .filter_map(|l| {
                l.split_once(' ')
                    .map(|(c, ch)| (c.to_string(), ch.trim().to_string()))
            })
            .collect())
    }

    /// Is this commit id known to jj and visible? Naming a hidden commit by
    /// id makes it resolve in revsets (even through `& all()`), so hiddenness
    /// must be read from the commit itself.
    pub fn is_visible(&self, sha: &str) -> bool {
        self.jj_ok(&[
            "log",
            "--ignore-working-copy",
            "--no-graph",
            "-r",
            &format!("present({sha})"),
            "-T",
            r#"if(hidden, "0", "1")"#,
        ])
        .map(|o| o.stdout.trim() == "1")
        .unwrap_or(false)
    }

    /// Parent commit ids. Works on hidden commits too — revsets resolve a
    /// hidden commit named by id.
    pub fn parents(&self, sha: &str) -> Result<Vec<String>> {
        self.log_shas(&format!("parents({sha})"))
    }

    pub fn change_of(&self, sha: &str) -> Result<String> {
        let out = self.jj_ok(&[
            "log",
            "--ignore-working-copy",
            "--no-graph",
            "-r",
            sha,
            "-T",
            "change_id",
        ])?;
        Ok(out.stdout.trim().to_string())
    }

    /// `jj rebase -s src -d dest` — moves src and all its descendants.
    pub fn rebase(&self, src: &str, dest: &str) -> Result<()> {
        self.jj_ok(&["rebase", "-s", src, "-d", dest]).map(|_| ())
    }

    pub fn edit(&self, sha: &str) -> Result<()> {
        self.jj_ok(&["edit", sha]).map(|_| ())
    }

    /// `jj new a b` — the merge working copy. Returns the new @ commit id.
    pub fn new_merge(&self, a: &str, b: &str) -> Result<String> {
        self.jj_ok(&["new", a, b])?;
        self.wc_commit()
    }

    pub fn abandon(&self, sha: &str) -> Result<()> {
        self.jj_ok(&["abandon", sha]).map(|_| ())
    }

    pub fn git_import(&self) -> Result<()> {
        self.jj_ok(&["git", "import"]).map(|_| ())
    }

    pub fn bookmark_set(&self, name: &str, sha: &str) -> Result<()> {
        self.jj_ok(&["bookmark", "set", name, "--allow-backwards", "-r", sha])
            .map(|_| ())
    }

    pub fn bookmark_delete(&self, names: &[String]) -> Result<()> {
        if names.is_empty() {
            return Ok(());
        }
        let mut args = vec!["bookmark", "delete"];
        args.extend(names.iter().map(|s| s.as_str()));
        self.jj_ok(&args).map(|_| ())
    }

    /// Empty tree diff and empty description — jj would auto-abandon it on leave.
    pub fn is_discardable(&self, sha: &str) -> Result<bool> {
        let out = self.jj_ok(&[
            "log",
            "--ignore-working-copy",
            "--no-graph",
            "-r",
            sha,
            "-T",
            r#"if(empty, "1", "0") ++ if(description, "1", "0")"#,
        ])?;
        Ok(out.stdout.trim() == "10")
    }

    pub fn has_children(&self, sha: &str) -> Result<bool> {
        Ok(!self.log_shas(&format!("children({sha})"))?.is_empty())
    }
}
