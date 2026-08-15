//! The status report `jjsync sync` writes and `jjsync status` reads, plus
//! desktop notifications for newly appeared problems.

use crate::cycle::RepoOutcome;
use crate::exec::Env;
use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
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

    /// One line per repo: problems win over ✓, unpublished local state shows
    /// as pending, plus the report's age. `configured` repos missing from the
    /// report show as never synced; `not_cloned` repos (path absent on this
    /// machine) point at `jjsync bootstrap` instead.
    pub fn render(
        &self,
        configured: &[String],
        not_cloned: &[String],
        pending: &BTreeMap<String, Vec<String>>,
        now: u64,
    ) -> String {
        let mut out = String::new();
        let age = age_suffix(self.when, now);
        for repo in &self.repos {
            if not_cloned.contains(&repo.repo) {
                continue; // a stale report entry for a since-deleted directory
            }
            // Frozen bookmarks differ from S by design and already show as ⚠.
            let frozen: BTreeSet<String> = repo
                .problems
                .iter()
                .filter_map(|p| p.bookmark().map(|b| format!("bookmark {b}")))
                .collect();
            let pend: Vec<String> = pending
                .get(&repo.repo)
                .into_iter()
                .flatten()
                .filter(|i| !frozen.contains(*i))
                .cloned()
                .collect();
            let mut parts: Vec<String> = vec![];
            if !repo.problems.is_empty() {
                parts.push(
                    repo.problems
                        .iter()
                        .map(|p| match p.bookmark() {
                            Some(b) => format!("⚠ {b} {}", p.message()),
                            None => format!("⚠ {}", p.message()),
                        })
                        .collect::<Vec<_>>()
                        .join("; "),
                );
            } else if repo.offline {
                parts.push("○ offline (will retry)".to_string());
            }
            if !pend.is_empty() {
                parts.push(format!("● pending: {}", pend.join(", ")));
            }
            if parts.is_empty() {
                parts.push(if repo.synced_workspaces.is_empty() {
                    "✓ synced".to_string()
                } else {
                    format!("✓ synced ({})", repo.synced_workspaces.join(", "))
                });
            }
            out.push_str(&format!("{:<24} {}{age}\n", repo.repo, parts.join("; ")));
        }
        for name in configured {
            if not_cloned.contains(name) {
                out.push_str(&format!(
                    "{name:<24} ○ not cloned — run `jjsync bootstrap`\n"
                ));
            } else if !self.repos.iter().any(|r| &r.repo == name) {
                let pend = pending.get(name).filter(|p| !p.is_empty());
                let extra = match pend {
                    Some(p) => format!("; ● pending: {}", p.join(", ")),
                    None => String::new(),
                };
                out.push_str(&format!("{name:<24} – never synced{extra}\n"));
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
