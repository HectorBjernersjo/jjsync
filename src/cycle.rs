//! The sync cycle: snapshot → fetch → three-way reconcile → publish (CAS).
//! One idempotent pass per repo; every step is safe to repeat or interrupt.
//!
//! S (the last-synced mirror, ADR 0003) lives in plain local git refs under
//! refs/jj-sync/s/. Note: ADR 0001 wanted the `__jj_sync/<ws>` jj bookmark as
//! the marker, but jj moves local bookmarks along when the commit they point
//! at is rewritten (verified on jj 0.41), which would make "moved here"
//! undetectable — plain refs outside refs/heads/ stay put.

use crate::config::{glob_match, RepoConfig};
use crate::exec::Env;
use crate::git::{Git, NetOutcome, RefPush, REMOTE_NS, S_NS};
use crate::jj::Jj;
use crate::leak::{self, ScanResult};
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

macro_rules! debug {
    ($($t:tt)*) => {
        if std::env::var_os("JJSYNC_DEBUG").is_some() {
            eprintln!($($t)*);
        }
    };
}

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Problem {
    FrozenBookmark {
        bookmark: String,
        local: String,
        remote: String,
    },
    LeakBlocked {
        detail: String,
    },
    Auth {
        detail: String,
    },
    Error {
        detail: String,
    },
}

impl Problem {
    /// Stable identity for notification dedup across cycles.
    pub fn key(&self) -> String {
        match self {
            Problem::FrozenBookmark { bookmark, .. } => format!("frozen:{bookmark}"),
            Problem::LeakBlocked { .. } => "leak".into(),
            Problem::Auth { .. } => "auth".into(),
            Problem::Error { detail } => format!("error:{detail}"),
        }
    }

    pub fn message(&self) -> String {
        match self {
            Problem::FrozenBookmark {
                bookmark: _,
                local,
                remote,
            } => format!(
                "frozen: moved on both machines ({} / {})",
                &local[..local.len().min(8)],
                &remote[..remote.len().min(8)]
            ),
            Problem::LeakBlocked { detail } => format!("publish blocked: {detail}"),
            Problem::Auth { detail } => format!("auth error: {detail}"),
            Problem::Error { detail } => detail.clone(),
        }
    }

    /// Frozen bookmarks and auth failures warrant a desktop notification;
    /// everything else only shows in `jjsync status`.
    pub fn notify_worthy(&self) -> bool {
        !matches!(self, Problem::Error { .. })
    }

    pub fn bookmark(&self) -> Option<&str> {
        match self {
            Problem::FrozenBookmark { bookmark, .. } => Some(bookmark),
            _ => None,
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct RepoOutcome {
    pub repo: String,
    pub synced_workspaces: Vec<String>,
    pub problems: Vec<Problem>,
    pub offline: bool,
}

impl RepoOutcome {
    fn error(repo: String, detail: String) -> Self {
        RepoOutcome {
            repo,
            synced_workspaces: vec![],
            problems: vec![Problem::Error { detail }],
            offline: false,
        }
    }
}

#[derive(Default)]
pub struct Hooks<'a> {
    /// Runs after the fetch, before reconciliation — lets tests move remote
    /// refs inside the race window. Receives the attempt number (0-based).
    pub after_fetch: Option<&'a dyn Fn(u32)>,
}

#[derive(Default)]
pub struct CycleOpts<'a> {
    pub env: Env,
    /// Bookmarks whose divergence resolves as "local position wins".
    pub resolve: Vec<String>,
    pub hooks: Hooks<'a>,
}

enum Attempt {
    Done(RepoOutcome),
    /// A CAS lease failed — remote moved mid-cycle. Refetch and reconcile again.
    Retry,
}

pub fn sync_repo(cfg: &RepoConfig, opts: &CycleOpts) -> RepoOutcome {
    let name = cfg.name();
    for attempt in 0..3 {
        match cycle_attempt(cfg, opts, attempt) {
            Ok(Attempt::Done(outcome)) => return outcome,
            Ok(Attempt::Retry) => continue,
            Err(e) => return RepoOutcome::error(name, format!("{e:#}")),
        }
    }
    RepoOutcome::error(
        name,
        "publish kept losing the CAS race after 3 attempts".into(),
    )
}

/// Imports fetched commits into jj's view via short-lived local branches.
/// Cleanup deletes the temp bookmarks through jj, which keeps the commits
/// visible (deleting the git ref directly would abandon them on re-import).
struct Importer<'a, 'b> {
    git: &'b Git<'a>,
    jj: &'b Jj<'a>,
    tmp: Vec<String>,
    n: usize,
}

impl<'a, 'b> Importer<'a, 'b> {
    fn new(git: &'b Git<'a>, jj: &'b Jj<'a>) -> Self {
        Importer {
            git,
            jj,
            tmp: vec![],
            n: 0,
        }
    }

    fn ensure_visible(&mut self, sha: &str) -> Result<()> {
        if self.jj.is_visible(sha) {
            return Ok(());
        }
        let name = format!("__jjsync-tmp-{}", self.n);
        self.n += 1;
        self.git
            .update_refs(&[(format!("refs/heads/{name}"), Some(sha.to_string()))])?;
        self.jj.git_import()?;
        self.tmp.push(name);
        Ok(())
    }

    fn cleanup(&mut self) -> Result<()> {
        let names = std::mem::take(&mut self.tmp);
        self.jj.bookmark_delete(&names)?;
        debug!(
            "importer cleanup {names:?}; refs/heads now: {:?}",
            self.git.refs_with_prefix("refs/heads/")
        );
        Ok(())
    }
}

fn is_sync_bookmark(name: &str) -> bool {
    name.starts_with("__jjsync") || name.starts_with("__jj_sync")
}

fn primary_workspace(jj: &Jj, workspaces: &[(String, String)]) -> Result<String> {
    let wc = jj.wc_commit()?;
    let matches: Vec<&String> = workspaces
        .iter()
        .filter(|(_, t)| *t == wc)
        .map(|(n, _)| n)
        .collect();
    if matches.iter().any(|n| *n == "default") {
        return Ok("default".into());
    }
    matches
        .first()
        .map(|n| n.to_string())
        .context("could not determine the repo path's workspace")
}

fn cycle_attempt(cfg: &RepoConfig, opts: &CycleOpts, attempt: u32) -> Result<Attempt> {
    let repo_name = cfg.name();
    let path = cfg.expanded_path();
    if !path.join(".jj").is_dir() || !(path.join(".git").exists()) {
        return Ok(Attempt::Done(RepoOutcome::error(
            repo_name,
            format!("{} is not a colocated jj repo", path.display()),
        )));
    }
    let env = &opts.env;
    let git = Git::new(&path, env);
    let jj = Jj::new(&path, env);
    let prefix = cfg.prefix();
    let mut problems: Vec<Problem> = vec![];

    // ---- discover workspaces & snapshot ------------------------------------
    let ws_list = jj.workspaces()?;
    let primary = primary_workspace(&jj, &ws_list)?;
    let mut synced_ws: Vec<(String, PathBuf)> = vec![(primary.clone(), path.clone())];
    for (name, dir) in &cfg.workspaces {
        let dir = crate::config::expand_tilde(dir);
        if *name != primary && ws_list.iter().any(|(n, _)| n == name) && dir.is_dir() {
            synced_ws.push((name.clone(), dir));
        }
    }
    for (_, dir) in &synced_ws {
        if let Err(e) = Jj::new(dir, env).snapshot() {
            problems.push(Problem::Error {
                detail: format!("snapshot failed: {e:#}"),
            });
        }
    }
    let ws_targets: BTreeMap<String, String> = jj.workspaces()?.into_iter().collect();

    // ---- fetch ---------------------------------------------------------------
    match git.fetch_namespace(&cfg.remote, &prefix)? {
        NetOutcome::Ok => {}
        NetOutcome::Auth(d) => {
            problems.push(Problem::Auth {
                detail: first_line(&d),
            });
            return Ok(Attempt::Done(RepoOutcome {
                repo: repo_name,
                synced_workspaces: vec![],
                problems,
                offline: false,
            }));
        }
        NetOutcome::Offline(_) => {
            // Offline is a non-event: silent, retried by the next timer tick.
            return Ok(Attempt::Done(RepoOutcome {
                repo: repo_name,
                synced_workspaces: vec![],
                problems,
                offline: true,
            }));
        }
        NetOutcome::LeaseFailed(d) | NetOutcome::Other(d) => {
            problems.push(Problem::Error {
                detail: format!("fetch failed: {}", first_line(&d)),
            });
            return Ok(Attempt::Done(RepoOutcome {
                repo: repo_name,
                synced_workspaces: vec![],
                problems,
                offline: false,
            }));
        }
    }
    if let Some(hook) = opts.hooks.after_fetch {
        hook(attempt);
    }

    let remote_map = git.refs_with_prefix(REMOTE_NS)?;
    let s_map = git.refs_with_prefix(S_NS)?;
    let mut pushes: Vec<RefPush> = vec![];
    // Items whose S entry must survive this cycle untouched (frozen bookmarks):
    // letting S catch up with the remote would dissolve the recorded divergence.
    let mut preserve_s: BTreeSet<String> = BTreeSet::new();
    debug!("[{repo_name}] remote={remote_map:?} s={s_map:?} ws={ws_targets:?}");

    // ---- workspaces (M1) -----------------------------------------------------
    {
        let mut importer = Importer::new(&git, &jj);
        for (ws, dir) in &synced_ws {
            if ws == "heads" || ws == "bookmarks" || ws.contains('/') {
                problems.push(Problem::Error {
                    detail: format!("workspace name '{ws}' collides with the sync namespace"),
                });
                continue;
            }
            let l = match ws_targets.get(ws) {
                Some(l) => l.clone(),
                None => continue,
            };
            let s = s_map.get(ws.as_str());
            let r = remote_map.get(ws.as_str());
            debug!("[{repo_name}] ws {ws}: l={l} s={s:?} r={r:?}");
            sync_workspace(
                cfg,
                &git,
                &jj,
                &mut importer,
                &mut pushes,
                ws,
                dir,
                &l,
                s,
                r,
                env,
            )?;
        }
        importer.cleanup()?;
        // S records what this machine and the remote agree on. Workspace items
        // we did not reconcile (missing or unregistered here) must not get an
        // S entry — that would fake agreement and turn a later first sync of
        // that workspace into a bogus "moved here" publish.
        let synced_names: BTreeSet<&String> = synced_ws.iter().map(|(n, _)| n).collect();
        for item in remote_map.keys().chain(s_map.keys()) {
            if !item.starts_with("heads/")
                && !item.starts_with("bookmarks/")
                && !synced_names.contains(item)
            {
                preserve_s.insert(item.clone());
            }
        }
    }

    // ---- bookmarks (M2) --------------------------------------------------------
    // Before heads, so freshly adopted bookmarks exclude their targets from
    // the anonymous-head set.
    {
        let excluded =
            |n: &str| is_sync_bookmark(n) || cfg.exclude_bookmarks.iter().any(|p| glob_match(p, n));
        let local_bm: BTreeMap<String, String> = git
            .refs_with_prefix("refs/heads/")?
            .into_iter()
            .filter(|(n, _)| !excluded(n))
            .collect();
        let sub = |map: &BTreeMap<String, String>| -> BTreeMap<String, String> {
            map.iter()
                .filter_map(|(k, v)| {
                    k.strip_prefix("bookmarks/")
                        .map(|n| (n.to_string(), v.clone()))
                })
                .filter(|(n, _)| !excluded(n))
                .collect()
        };
        let remote_bm = sub(&remote_map);
        let s_bm = sub(&s_map);
        let mut names: BTreeSet<&String> = BTreeSet::new();
        names.extend(local_bm.keys());
        names.extend(remote_bm.keys());
        names.extend(s_bm.keys());

        let mut importer = Importer::new(&git, &jj);
        for name in names {
            let l = local_bm.get(name);
            let s = s_bm.get(name);
            let r = remote_bm.get(name);
            sync_bookmark(
                &jj,
                &mut importer,
                &mut pushes,
                &mut problems,
                &mut preserve_s,
                &opts.resolve,
                name,
                l,
                s,
                r,
            )?;
        }
        importer.cleanup()?;
    }

    // ---- anonymous heads (M2) ---------------------------------------------------
    {
        let ws_now: BTreeMap<String, String> = jj.workspaces()?.into_iter().collect();
        let ws_shas: BTreeSet<String> = ws_now.values().cloned().collect();
        let heads_of = |jj: &Jj| -> Result<BTreeSet<String>> {
            Ok(jj
                .log_shas("heads(all()) ~ bookmarks() ~ remote_bookmarks() ~ root()")?
                .into_iter()
                .filter(|s| !ws_shas.contains(s))
                .collect())
        };
        let r_heads: BTreeMap<String, String> = remote_map
            .iter()
            .filter_map(|(k, v)| k.strip_prefix("heads/").map(|s| (s.to_string(), v.clone())))
            .collect();
        let s_heads: BTreeMap<String, String> = s_map
            .iter()
            .filter_map(|(k, v)| k.strip_prefix("heads/").map(|s| (s.to_string(), v.clone())))
            .collect();

        let h_before = heads_of(&jj)?;
        debug!(
            "[{repo_name}] heads: H={h_before:?} r={:?} s={:?}",
            r_heads.keys(),
            s_heads.keys()
        );
        debug!(
            "[{repo_name}] raw heads(all())={:?} bookmarks={:?} ws_shas={ws_shas:?}",
            jj.log_shas("heads(all())"),
            jj.log_shas("bookmarks() | remote_bookmarks()"),
        );
        // Adopt heads created on other machines.
        let mut importer = Importer::new(&git, &jj);
        let mut adopted: Vec<String> = vec![];
        for sha in r_heads.keys() {
            // In S means "we already synced this" — its absence locally is a
            // local abandon/supersede, not something to re-adopt.
            if !h_before.contains(sha) && !s_heads.contains_key(sha) && !jj.is_visible(sha) {
                importer.ensure_visible(sha)?;
                adopted.push(sha.clone());
            }
        }
        importer.cleanup()?;
        // A stale local twin of an adopted head (same change, byte-identical
        // to last-synced, superseded remotely) is replaced, not kept — the
        // same rule as the working-copy adopt. Genuine concurrent edits keep
        // both versions (divergent change).
        if !adopted.is_empty() {
            let chg: BTreeMap<String, String> = jj.visible_commits()?.into_iter().collect();
            for sha in &adopted {
                let Some(c) = chg.get(sha) else { continue };
                for y in &h_before {
                    if y != sha
                        && chg.get(y) == Some(c)
                        && s_heads.contains_key(y)
                        && !r_heads.contains_key(y)
                        && !jj.has_children(y)?
                    {
                        jj.abandon(y)?;
                    }
                }
            }
        }

        let mut h = heads_of(&jj)?;
        // Deleted there: abandon locally only if untouched (still a visible
        // head, no divergent sibling keeping the change alive).
        let deleted_there: Vec<String> = h
            .iter()
            .filter(|sha| s_heads.contains_key(*sha) && !r_heads.contains_key(*sha))
            .cloned()
            .collect();
        if !deleted_there.is_empty() {
            let chg: BTreeMap<String, String> = jj.visible_commits()?.into_iter().collect();
            for sha in deleted_there {
                let Some(c) = chg.get(&sha) else { continue };
                let has_sibling = chg.iter().any(|(s2, c2)| c2 == c && *s2 != sha);
                if !has_sibling && !jj.has_children(&sha)? {
                    jj.abandon(&sha)?;
                    h.remove(&sha);
                }
                // else: modified here in some form — edit wins, it stays and
                // is republished below.
            }
        }
        // Publish heads new here (or resurrected).
        for sha in &h {
            if !r_heads.contains_key(sha) {
                pushes.push(RefPush {
                    item: format!("heads/{sha}"),
                    new: Some(sha.clone()),
                    expected: None,
                });
            }
        }
        // Superseded or abandoned here: delete the remote ref (CAS).
        for (sha, val) in &r_heads {
            if s_heads.contains_key(sha) && !h.contains(sha) {
                pushes.push(RefPush {
                    item: format!("heads/{sha}"),
                    new: None,
                    expected: Some(val.clone()),
                });
            }
        }
    }

    // ---- leak gate -----------------------------------------------------------
    if !pushes.is_empty() && cfg.leak_scan {
        let new_shas: Vec<String> = pushes.iter().filter_map(|p| p.new.clone()).collect();
        let bases: Vec<String> = remote_map.values().cloned().collect();
        let outgoing = git.rev_list_not(&new_shas, &bases)?;
        match leak::scan(&path, &outgoing, env) {
            Ok(ScanResult::Clean) => {}
            Ok(ScanResult::Hit(files)) => {
                problems.push(Problem::LeakBlocked {
                    detail: format!("gitleaks hit in {}", files.join(", ")),
                });
                pushes.clear();
            }
            Err(e) => {
                problems.push(Problem::LeakBlocked {
                    detail: format!("{e:#}"),
                });
                pushes.clear();
            }
        }
    }

    // ---- publish (CAS, atomic) -------------------------------------------------
    let mut final_remote = remote_map.clone();
    if !pushes.is_empty() {
        match git.push(&cfg.remote, &prefix, &pushes)? {
            NetOutcome::Ok => {
                for p in &pushes {
                    match &p.new {
                        Some(sha) => final_remote.insert(p.item.clone(), sha.clone()),
                        None => final_remote.remove(&p.item),
                    };
                }
            }
            NetOutcome::LeaseFailed(_) => return Ok(Attempt::Retry),
            NetOutcome::Auth(d) => problems.push(Problem::Auth {
                detail: first_line(&d),
            }),
            NetOutcome::Offline(_) => {
                // Publish lost the network: keep S at the fetched state so the
                // next cycle simply retries the publish.
                return Ok(Attempt::Done(RepoOutcome {
                    repo: repo_name,
                    synced_workspaces: vec![],
                    problems,
                    offline: true,
                }));
            }
            NetOutcome::Other(d) => problems.push(Problem::Error {
                detail: format!("push failed: {}", first_line(&d)),
            }),
        }
    }

    // ---- S := the remote state we now agree on ---------------------------------
    // On any partial failure S stays at the fetched state, so unpublished
    // changes look "moved here" again next cycle — self-healing by design.
    let mut updates: Vec<(String, Option<String>)> = vec![];
    for (item, sha) in &final_remote {
        if !preserve_s.contains(item) && s_map.get(item) != Some(sha) {
            updates.push((format!("{S_NS}{item}"), Some(sha.clone())));
        }
    }
    for item in s_map.keys() {
        if !preserve_s.contains(item) && !final_remote.contains_key(item) {
            updates.push((format!("{S_NS}{item}"), None));
        }
    }
    git.update_refs(&updates)?;

    Ok(Attempt::Done(RepoOutcome {
        repo: repo_name,
        synced_workspaces: synced_ws.iter().map(|(n, _)| n.clone()).collect(),
        problems,
        offline: false,
    }))
}

/// Reconcile one workspace's working copy (the M1 core).
#[allow(clippy::too_many_arguments)]
fn sync_workspace(
    _cfg: &RepoConfig,
    git: &Git,
    jj: &Jj,
    importer: &mut Importer,
    pushes: &mut Vec<RefPush>,
    ws: &str,
    dir: &Path,
    l: &str,
    s: Option<&String>,
    r: Option<&String>,
    env: &Env,
) -> Result<()> {
    let ws_jj = Jj::new(dir, env);
    let publish = |pushes: &mut Vec<RefPush>, new: &str, lease: Option<&String>| {
        pushes.push(RefPush {
            item: ws.to_string(),
            new: Some(new.to_string()),
            expected: lease.cloned(),
        });
    };

    let Some(r) = r else {
        // Nothing on the remote (first machine, or the ref was removed):
        // publish. A workspace's @ always exists, so deletion propagation
        // does not apply to workspace refs.
        publish(pushes, l, None);
        return Ok(());
    };
    if l == r {
        return Ok(()); // in sync (S catches up via the final rewrite)
    }
    if s == Some(r) {
        // Remote unchanged since last cycle, local moved: publish.
        publish(pushes, l, Some(r));
        return Ok(());
    }
    let local_untouched = s == Some(&l.to_string());
    let first_sync_discardable = s.is_none() && ws_jj.is_discardable(l).unwrap_or(false);
    if local_untouched || first_sync_discardable {
        // Adopt: the working copy becomes the same jj change as on the other
        // machine (ADR 0002).
        importer.ensure_visible(r)?;
        ws_jj.edit(r)?;
        abandon_stale_wc(git, jj, l, local_untouched)?;
        return Ok(());
    }
    // Both sides moved: divergence. Ancestor cases degenerate to plain
    // publish/adopt; a real fork merges immediately (clean or as an ordinary
    // jj conflict in the files) and the merge is published.
    if git.is_ancestor(r, l)? {
        publish(pushes, l, Some(r));
        return Ok(());
    }
    if git.is_ancestor(l, r)? {
        importer.ensure_visible(r)?;
        ws_jj.edit(r)?;
        return Ok(());
    }
    importer.ensure_visible(r)?;
    let merged = ws_jj.new_merge(l, r)?;
    publish(pushes, &merged, Some(r));
    Ok(())
}

/// After adopting, drop the stale byte-identical previous working copy —
/// guarded so nothing built upon, bookmarked, or independently changed is
/// ever abandoned. (jj itself auto-abandons an empty undescribed @ on leave.)
fn abandon_stale_wc(git: &Git, jj: &Jj, old: &str, untouched: bool) -> Result<()> {
    debug!(
        "abandon_stale_wc {old}: untouched={untouched} visible={} children={:?}",
        jj.is_visible(old),
        jj.has_children(old)
    );
    if !untouched || !jj.is_visible(old) || jj.has_children(old)? {
        return Ok(());
    }
    let ws_now: Vec<String> = jj.workspaces()?.into_iter().map(|(_, t)| t).collect();
    if ws_now.iter().any(|t| t == old) {
        return Ok(());
    }
    if git
        .refs_with_prefix("refs/heads/")?
        .values()
        .any(|v| v == old)
    {
        return Ok(());
    }
    debug!("abandoning stale wc {old}");
    jj.abandon(old)
}

/// Reconcile one bookmark per the ADR 0003 table. Divergence freezes the
/// bookmark until `jjsync resolve` (present in `resolve`) says local wins.
#[allow(clippy::too_many_arguments)]
fn sync_bookmark(
    jj: &Jj,
    importer: &mut Importer,
    pushes: &mut Vec<RefPush>,
    problems: &mut Vec<Problem>,
    preserve_s: &mut BTreeSet<String>,
    resolve: &[String],
    name: &str,
    l: Option<&String>,
    s: Option<&String>,
    r: Option<&String>,
) -> Result<()> {
    let item = format!("bookmarks/{name}");
    let mut adopt = |sha: &String| -> Result<()> {
        importer.ensure_visible(sha)?;
        jj.bookmark_set(name, sha)
    };
    match (l, s, r) {
        (None, None, None) => {}
        // Created here → publish.
        (Some(l), None, None) => {
            pushes.push(RefPush {
                item,
                new: Some(l.clone()),
                expected: None,
            });
        }
        // Created there → adopt.
        (None, None, Some(r)) => adopt(r)?,
        // Deleted here; if it moved there since, the edit wins over the deletion.
        (None, Some(s), Some(r)) => {
            if r == s {
                pushes.push(RefPush {
                    item,
                    new: None,
                    expected: Some(s.clone()),
                });
            } else {
                adopt(r)?;
            }
        }
        // Deleted there; a local move since resurrects it.
        (Some(l), Some(s), None) => {
            if l == s {
                jj.bookmark_delete(&[name.to_string()])?;
            } else {
                pushes.push(RefPush {
                    item,
                    new: Some(l.clone()),
                    expected: None,
                });
            }
        }
        // Gone on both sides; the S rewrite forgets it.
        (None, Some(_), None) => {}
        // Exists on both but never synced here (e.g. same name created on two
        // machines, or S was lost): agreement is silent, disagreement freezes.
        (Some(l), None, Some(r)) => {
            if l != r {
                divergence(pushes, problems, preserve_s, resolve, name, item, l, r);
            }
        }
        (Some(l), Some(s), Some(r)) => {
            if l == r {
                // agree — nothing to do
            } else if r == s {
                pushes.push(RefPush {
                    item,
                    new: Some(l.clone()),
                    expected: Some(s.clone()),
                });
            } else if l == s {
                adopt(r)?;
            } else {
                divergence(pushes, problems, preserve_s, resolve, name, item, l, r);
            }
        }
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn divergence(
    pushes: &mut Vec<RefPush>,
    problems: &mut Vec<Problem>,
    preserve_s: &mut BTreeSet<String>,
    resolve: &[String],
    name: &str,
    item: String,
    l: &str,
    r: &str,
) {
    if resolve.iter().any(|n| n == name) {
        pushes.push(RefPush {
            item,
            new: Some(l.to_string()),
            expected: Some(r.to_string()),
        });
    } else {
        // Frozen: no ref moves, and S must keep recording the divergence.
        preserve_s.insert(item);
        problems.push(Problem::FrozenBookmark {
            bookmark: name.to_string(),
            local: l.to_string(),
            remote: r.to_string(),
        });
    }
}

fn first_line(s: &str) -> String {
    s.lines().next().unwrap_or("").trim().to_string()
}
