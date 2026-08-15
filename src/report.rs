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
    /// Unix seconds when the cycle ran; 0 in state files from older versions.
    #[serde(default)]
    pub when: u64,
    pub repos: Vec<RepoOutcome>,
}

pub fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
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

    /// One line per repo: problems win over ✓, plus the report's age.
    /// `configured` repos missing from the report show as never synced.
    pub fn render(&self, configured: &[String], now: u64) -> String {
        let mut out = String::new();
        let age = age_suffix(self.when, now);
        for repo in &self.repos {
            let state = if !repo.problems.is_empty() {
                repo.problems
                    .iter()
                    .map(|p| match p.bookmark() {
                        Some(b) => format!("⚠ {b} {}", p.message()),
                        None => format!("⚠ {}", p.message()),
                    })
                    .collect::<Vec<_>>()
                    .join("; ")
            } else if repo.offline {
                "○ offline (will retry)".to_string()
            } else if repo.synced_workspaces.is_empty() {
                "✓ synced".to_string()
            } else {
                format!("✓ synced ({})", repo.synced_workspaces.join(", "))
            };
            out.push_str(&format!("{:<24} {state}{age}\n", repo.repo));
        }
        for name in configured {
            if !self.repos.iter().any(|r| &r.repo == name) {
                out.push_str(&format!("{name:<24} – never synced\n"));
            }
        }
        if out.is_empty() {
            out.push_str("no repos configured — run `jjsync init` inside a repo\n");
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

fn age_suffix(when: u64, now: u64) -> String {
    if when == 0 {
        return String::new();
    }
    let d = now.saturating_sub(when);
    let text = match d {
        0..=59 => format!("{d}s ago"),
        60..=3599 => format!("{} min ago", d / 60),
        3600..=86399 => format!("{} h ago", d / 3600),
        _ => format!("{} d ago", d / 86400),
    };
    format!(" · {text}")
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
                // Critical: each problem notifies once (dedup above), so a
                // transient toast that expires unseen would be the only
                // warning the user ever gets. Critical ones stay up.
                let out = crate::exec::run(
                    "notify-send",
                    &[
                        "--urgency=critical",
                        "--app-name=jjsync",
                        "jjsync",
                        &format!("{}: {}", repo.repo, p.message()),
                    ],
                    Path::new("/"),
                    env,
                );
                match out {
                    Ok(o) if o.ok() => {}
                    Ok(o) => eprintln!("warning: notify-send failed: {}", o.stderr.trim()),
                    Err(e) => eprintln!("warning: could not run notify-send: {e}"),
                }
            }
        }
    }
}
