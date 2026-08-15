//! The status report `jjsync sync` writes and `jjsync status` reads, plus
//! desktop notifications for newly appeared problems.

use crate::cycle::RepoOutcome;
use crate::exec::Env;
use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::path::Path;

#[derive(Serialize, Deserialize, Default)]
pub struct Report {
    pub repos: Vec<RepoOutcome>,
}

impl Report {
    pub fn load(path: &Path) -> Option<Report> {
        let text = std::fs::read_to_string(path).ok()?;
        serde_json::from_str(&text).ok()
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        std::fs::write(path, serde_json::to_string_pretty(self)?)?;
        Ok(())
    }

    /// One line per workspace when healthy, one line per problem.
    pub fn render(&self) -> String {
        let mut out = String::new();
        for repo in &self.repos {
            let healthy = repo.problems.is_empty();
            for ws in &repo.synced_workspaces {
                if healthy && !repo.offline {
                    out.push_str(&format!(
                        "{:<24} ✓ synced\n",
                        format!("{}/{}", repo.repo, ws)
                    ));
                }
            }
            if repo.offline {
                out.push_str(&format!("{:<24} ○ offline (will retry)\n", repo.repo));
            }
            for p in &repo.problems {
                let label = match p.bookmark() {
                    Some(b) => format!("{:<10} {}", repo.repo, b),
                    None => repo.repo.clone(),
                };
                out.push_str(&format!("{:<24} ⚠ {}\n", label, p.message()));
            }
        }
        if out.is_empty() {
            out.push_str("no repos synced yet — run `jjsync init` inside a repo\n");
        }
        out
    }

    fn problem_keys(&self) -> BTreeSet<String> {
        self.repos
            .iter()
            .flat_map(|r| r.problems.iter().map(|p| format!("{}:{}", r.repo, p.key())))
            .collect()
    }
}

/// Notify about problems that were not present in the previous report, so a
/// 60 s timer doesn't nag every minute about the same frozen bookmark.
pub fn notify_new_problems(prev: Option<&Report>, cur: &Report, env: &Env) {
    if std::env::var_os("JJSYNC_NO_NOTIFY").is_some() {
        return;
    }
    let old = prev.map(|r| r.problem_keys()).unwrap_or_default();
    for repo in &cur.repos {
        for p in &repo.problems {
            if p.notify_worthy() && !old.contains(&format!("{}:{}", repo.repo, p.key())) {
                let _ = crate::exec::run(
                    "notify-send",
                    &["jjsync", &format!("{}: {}", repo.repo, p.message())],
                    Path::new("/"),
                    env,
                );
            }
        }
    }
}
