//! Broad integration tests: a bare git remote + two colocated jj clones,
//! driven by the real `jj` and `git` binaries. Each test is a full user
//! scenario running real sync cycles alternately as machine A and machine B.
//! HOME/XDG_* point into the tempdir, so real user config never leaks in.
//!
//! Because these run real binaries they double as the canary for jj upgrades
//! (e.g. the change-id-header dependency in ADR 0001).

use jjsync::config::RepoConfig;
use jjsync::cycle::{sync_repo, CycleOpts, Hooks, Problem, RepoOutcome};
use jjsync::exec::{run_ok, Env};
use std::fs;
use std::path::{Path, PathBuf};
use tempfile::TempDir;

struct World {
    _tmp: TempDir,
    root: PathBuf,
    remote: PathBuf,
    env: Env,
}

impl World {
    fn new() -> World {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path().to_path_buf();
        let home = root.join("home");
        fs::create_dir_all(home.join(".config/jj")).unwrap();
        fs::write(
            home.join(".gitconfig"),
            "[user]\n\tname = Test\n\temail = test@example.com\n[init]\n\tdefaultBranch = main\n",
        )
        .unwrap();
        fs::write(
            home.join(".config/jj/config.toml"),
            "user.name = \"Test\"\nuser.email = \"test@example.com\"\n",
        )
        .unwrap();
        let env = Env {
            vars: vec![
                ("HOME".into(), home.display().to_string()),
                (
                    "XDG_CONFIG_HOME".into(),
                    home.join(".config").display().to_string(),
                ),
                (
                    "XDG_STATE_HOME".into(),
                    home.join(".state").display().to_string(),
                ),
                (
                    "XDG_CACHE_HOME".into(),
                    home.join(".cache").display().to_string(),
                ),
                ("GIT_CONFIG_NOSYSTEM".into(), "1".into()),
                ("JJSYNC_NO_NOTIFY".into(), "1".into()),
                ("JJSYNC_NO_SYSTEMD".into(), "1".into()),
            ],
        };
        let remote = root.join("remote.git");
        run_ok(
            "git",
            &["init", "-q", "--bare", remote.to_str().unwrap()],
            &root,
            &env,
        )
        .unwrap();
        World {
            _tmp: tmp,
            root,
            remote,
            env,
        }
    }

    fn machine(&self, name: &str) -> Machine {
        let dir = self.root.join(name);
        fs::create_dir_all(&dir).unwrap();
        run_ok("git", &["init", "-q"], &dir, &self.env).unwrap();
        run_ok(
            "git",
            &["remote", "add", "origin", self.remote.to_str().unwrap()],
            &dir,
            &self.env,
        )
        .unwrap();
        run_ok("jj", &["git", "init", "--colocate"], &dir, &self.env).unwrap();
        Machine {
            cfg: RepoConfig {
                path: dir.display().to_string(),
                ..Default::default()
            },
            dir,
            env: self.env.clone(),
        }
    }

    /// Sha of a ref on the sync remote, if it exists.
    fn remote_ref(&self, name: &str) -> Option<String> {
        let out = run_ok(
            "git",
            &["for-each-ref", "--format=%(objectname)", name],
            &self.remote,
            &self.env,
        )
        .unwrap();
        let sha = out.stdout.trim().to_string();
        if sha.is_empty() {
            None
        } else {
            Some(sha)
        }
    }
}

struct Machine {
    dir: PathBuf,
    env: Env,
    cfg: RepoConfig,
}

impl Machine {
    fn jj(&self, args: &[&str]) -> String {
        run_ok("jj", args, &self.dir, &self.env).unwrap().stdout
    }

    fn git(&self, args: &[&str]) -> String {
        run_ok("git", args, &self.dir, &self.env).unwrap().stdout
    }

    fn write(&self, rel: &str, content: &str) {
        let p = self.dir.join(rel);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, content).unwrap();
    }

    fn read(&self, rel: &str) -> String {
        fs::read_to_string(self.dir.join(rel)).unwrap()
    }

    fn template(&self, revset: &str, template: &str) -> String {
        self.jj(&["log", "--no-graph", "-r", revset, "-T", template])
            .trim()
            .to_string()
    }

    fn wc_sha(&self) -> String {
        self.template("@", "commit_id")
    }

    fn wc_change(&self) -> String {
        self.template("@", "change_id")
    }

    fn is_visible(&self, sha: &str) -> bool {
        // `present(<hidden sha>) & all()` still resolves — hiddenness must be
        // read off the commit itself.
        self.template(&format!("present({sha})"), r#"if(hidden, "0", "1")"#) == "1"
    }

    fn sync(&self) -> RepoOutcome {
        self.sync_with(CycleOpts {
            env: self.env.clone(),
            ..Default::default()
        })
    }

    fn sync_with(&self, opts: CycleOpts) -> RepoOutcome {
        sync_repo(&self.cfg, &opts)
    }

    fn sync_ok(&self) -> RepoOutcome {
        let out = self.sync();
        assert!(
            out.problems.is_empty() && !out.offline,
            "expected healthy sync, got: {:?}",
            out.problems
        );
        out
    }

    fn pending(&self) -> Vec<String> {
        jjsync::cycle::pending_items(&self.cfg, &self.env).unwrap()
    }
}

/// Edit on A → sync A → sync B → same content *and same change-id* on B.
#[test]
fn roundtrip_preserves_content_and_change_id() {
    let w = World::new();
    let (a, b) = (w.machine("machine-a"), w.machine("machine-b"));

    a.write("notes.txt", "hello from a\n");
    a.sync_ok();
    assert!(w.remote_ref("refs/jj-sync/default").is_some());

    b.sync_ok();
    assert_eq!(b.read("notes.txt"), "hello from a\n");
    assert_eq!(
        a.wc_change(),
        b.wc_change(),
        "working copy must be the same jj change"
    );
    assert_eq!(a.wc_sha(), b.wc_sha());

    // And back: B edits, A follows.
    b.write("notes.txt", "hello from a\nand b\n");
    b.sync_ok();
    a.sync_ok();
    assert_eq!(a.read("notes.txt"), "hello from a\nand b\n");
    assert_eq!(a.wc_change(), b.wc_change());
}

/// Divergence in different files merges cleanly; the same line conflicts,
/// markers land on both machines, and resolving on B propagates to A.
#[test]
fn divergence_merges_and_conflicts_resolve() {
    let w = World::new();
    let (a, b) = (w.machine("machine-a"), w.machine("machine-b"));

    a.write("base.txt", "base\n");
    a.sync_ok();
    b.sync_ok();

    // Clean case: different files.
    a.write("from-a.txt", "a\n");
    b.write("from-b.txt", "b\n");
    a.sync_ok();
    b.sync_ok(); // B merges and publishes
    a.sync_ok(); // A adopts the merge
    assert_eq!(a.read("from-a.txt").trim(), "a");
    assert_eq!(a.read("from-b.txt").trim(), "b");
    assert_eq!(b.read("from-a.txt").trim(), "a");
    assert_eq!(a.wc_sha(), b.wc_sha());

    // Conflict case: same line.
    a.write("base.txt", "changed by a\n");
    b.write("base.txt", "changed by b\n");
    a.sync_ok();
    b.sync_ok();
    a.sync_ok();
    assert!(
        b.read("base.txt").contains("<<<<<<<"),
        "conflict markers must land on B"
    );
    assert!(
        a.read("base.txt").contains("<<<<<<<"),
        "conflict markers must land on A"
    );
    assert_eq!(a.wc_sha(), b.wc_sha());

    // Resolving on B propagates to A.
    b.write("base.txt", "resolved\n");
    b.sync_ok();
    a.sync_ok();
    assert_eq!(a.read("base.txt"), "resolved\n");
    assert!(!a.read("base.txt").contains("<<<<<<<"));
}

/// Both changed; A publishes between B's fetch and B's publish. B's push
/// fails the lease, B fetch-merge-republishes, nothing is lost.
#[test]
fn cas_race_loses_nothing() {
    let w = World::new();
    let (a, b) = (w.machine("machine-a"), w.machine("machine-b"));

    a.write("f0.txt", "base\n");
    a.sync_ok();
    b.sync_ok();

    a.write("fa.txt", "first a edit\n");
    a.sync_ok();
    b.write("fb.txt", "b edit\n");

    // Inside B's cycle, right after B fetched: A publishes again.
    let hook = |attempt: u32| {
        if attempt == 0 {
            a.write("fa2.txt", "second a edit\n");
            let out = a.sync();
            assert!(out.problems.is_empty());
        }
    };
    let out = b.sync_with(CycleOpts {
        env: b.env.clone(),
        hooks: Hooks {
            after_fetch: Some(&hook),
        },
        ..Default::default()
    });
    assert!(
        out.problems.is_empty(),
        "B must recover from the lost race: {:?}",
        out.problems
    );

    a.sync_ok();
    for m in [&a, &b] {
        assert_eq!(m.read("fa.txt").trim(), "first a edit");
        assert_eq!(m.read("fa2.txt").trim(), "second a edit");
        assert_eq!(m.read("fb.txt").trim(), "b edit");
    }
    assert_eq!(a.wc_sha(), b.wc_sha());
}

/// `jj describe` + `jj new` on A → the described commit reaches B as an
/// ancestor; the user bookmark `main` is untouched on both machines.
#[test]
fn described_commit_travels_as_ancestor() {
    let w = World::new();
    let (a, b) = (w.machine("machine-a"), w.machine("machine-b"));

    // main points at a stable described commit — not at @, which jj rewrites
    // on every snapshot (and moves bookmarks along with).
    a.write("base.txt", "base\n");
    a.jj(&["describe", "-m", "base"]);
    a.jj(&["new"]);
    a.jj(&["bookmark", "create", "main", "-r", "@-"]);
    a.sync_ok();
    b.sync_ok();
    let main_before = w.remote_ref("refs/jj-sync/bookmarks/main").unwrap();

    a.write("feature.txt", "feature work\n");
    a.jj(&["describe", "-m", "feature work"]);
    a.jj(&["new"]);
    a.sync_ok();
    b.sync_ok();

    assert_eq!(a.wc_change(), b.wc_change());
    assert_eq!(b.template("@-", "description").trim(), "feature work");
    assert_eq!(b.read("feature.txt").trim(), "feature work");
    // main untouched everywhere, and never published as a normal branch.
    assert_eq!(
        w.remote_ref("refs/jj-sync/bookmarks/main").unwrap(),
        main_before
    );
    assert_eq!(a.git(&["rev-parse", "refs/heads/main"]).trim(), main_before);
    assert_eq!(b.git(&["rev-parse", "refs/heads/main"]).trim(), main_before);
    assert!(w.remote_ref("refs/heads/main").is_none());
}

/// A planted secret blocks that repo's publish until fixed or allowlisted;
/// local jj use and other repos are unaffected.
#[test]
fn leak_gate_blocks_until_fixed_or_allowlisted() {
    let w = World::new();
    let (a, b) = (w.machine("machine-a"), w.machine("machine-b"));
    let other = w.machine("machine-other"); // an unrelated healthy repo
    let mut other = other;
    other.cfg.ref_prefix = "refs/jj-sync-other/".into(); // own namespace on the shared test remote

    a.write("ok.txt", "fine\n");
    a.sync_ok();
    b.sync_ok();

    let fake_pat = "ghp_Zx9q8W7v6U5t4S3r2Q1p0O9n8M7l6K5j4I"; // gitleaks:allow
    a.write(".env.local", &format!("token={fake_pat}\n"));
    let out = a.sync();
    assert!(
        out.problems
            .iter()
            .any(|p| matches!(p, Problem::LeakBlocked { .. })),
        "expected leak block, got {:?}",
        out.problems
    );
    // Remote untouched by the blocked publish.
    b.sync_ok();
    assert!(
        !b.dir.join(".env.local").exists(),
        "the secret must not reach B"
    );

    // Other repos unaffected.
    other.write("other.txt", "other\n");
    other.sync_ok();

    // Allowlisting arm: a `gitleaks:allow` comment on the line unblocks —
    // the user has declared the finding fine, so it syncs as-is.
    a.write(
        ".env.local",
        &format!("token={fake_pat} # gitleaks:allow\n"),
    );
    a.sync_ok();
    b.sync_ok();
    assert!(b.read(".env.local").contains(fake_pat));

    // Fixing arm: a fresh secret blocks again; removing it unblocks.
    a.write("cred.txt", &format!("key={fake_pat}\n"));
    let out = a.sync();
    assert!(out
        .problems
        .iter()
        .any(|p| matches!(p, Problem::LeakBlocked { .. })));
    a.write("cred.txt", "key=redacted\n");
    a.sync_ok();
    b.sync_ok();
    assert_eq!(b.read("cred.txt").trim(), "key=redacted");
}

/// Rerunning a cycle changes nothing; a lost S (interrupted cycle, re-clone)
/// self-heals without touching the remote.
#[test]
fn idempotence_and_interrupted_cycles() {
    let w = World::new();
    let (a, b) = (w.machine("machine-a"), w.machine("machine-b"));

    a.write("f.txt", "content\n");
    a.sync_ok();
    b.sync_ok();
    a.sync_ok();

    let remote_before = w.remote_ref("refs/jj-sync/default").unwrap();
    let wc_before = a.wc_sha();
    let ops_before = a
        .jj(&["op", "log", "--no-graph", "-T", r#"id ++ "\n""#])
        .lines()
        .count();
    a.sync_ok();
    a.sync_ok();
    assert_eq!(w.remote_ref("refs/jj-sync/default").unwrap(), remote_before);
    assert_eq!(a.wc_sha(), wc_before);
    let ops_after = a
        .jj(&["op", "log", "--no-graph", "-T", r#"id ++ "\n""#])
        .lines()
        .count();
    assert_eq!(
        ops_before, ops_after,
        "an idempotent cycle must not create jj operations"
    );

    // Interrupted cycle: publish landed but S was never updated. The next
    // cycle must recognize agreement instead of duplicating work.
    a.git(&["update-ref", "-d", "refs/jj-sync/s/default"]);
    a.sync_ok();
    assert_eq!(w.remote_ref("refs/jj-sync/default").unwrap(), remote_before);
    assert_eq!(a.wc_sha(), wc_before);
    assert_eq!(
        a.git(&["rev-parse", "refs/jj-sync/s/default"]).trim(),
        remote_before
    );
}

/// M2: an anonymous head is created on A and appears on B; abandoning it on A
/// removes it on B; a version modified on B survives A's deletion (resurrection).
#[test]
fn anonymous_heads_create_abandon_resurrect() {
    let w = World::new();
    let (a, b) = (w.machine("machine-a"), w.machine("machine-b"));

    a.write("work.txt", "some work\n");
    a.sync_ok();
    b.sync_ok();

    // Park the current work as an anonymous head; continue on a fresh @.
    let head = a.wc_sha();
    a.jj(&["new", "root()"]);
    a.sync_ok();
    assert_eq!(
        w.remote_ref(&format!("refs/jj-sync/heads/{head}"))
            .as_deref(),
        Some(&head[..])
    );
    b.sync_ok();
    assert!(
        b.is_visible(&head),
        "the anonymous head must be visible on B"
    );

    // Abandon on A → disappears on B (untouched there).
    a.jj(&["abandon", &head]);
    a.sync_ok();
    assert!(w
        .remote_ref(&format!("refs/jj-sync/heads/{head}"))
        .is_none());
    b.sync_ok();
    assert!(
        !b.is_visible(&head),
        "the abandoned head must disappear on B"
    );

    // Resurrection: B modifies a head that A abandons — the edit wins.
    b.write("park.txt", "parked on b\n");
    let parked = b.wc_sha();
    let parked_change = b.wc_change();
    b.jj(&["new", "root()"]);
    b.sync_ok();
    a.sync_ok();
    assert!(a.is_visible(&parked));
    a.jj(&["abandon", &parked]);
    a.sync_ok(); // deletes heads/<parked> remotely
    b.jj(&["describe", "-m", "still needed", &parked]); // modified on B before it hears of it
    let modified = b.template(&parked_change, "commit_id");
    assert_ne!(modified, parked);
    b.sync_ok(); // must resurrect the modified version, not abandon it
    assert!(b.is_visible(&modified));
    a.sync_ok();
    let on_a = a.template(&parked_change, "commit_id");
    assert_eq!(on_a, modified, "the modified head must resurrect on A");
}

/// A chain abandoned on B (abandon + fresh @) must stay abandoned everywhere:
/// when A adopts the new @, the parents its stale @ exposes must be abandoned
/// too, not republished as anonymous heads — that republish used to bounce
/// the whole chain back to B (resurrection loop), with the delayed republish
/// firing on A's *second* cycle once S had caught up.
#[test]
fn abandoned_chain_is_not_resurrected() {
    let w = World::new();
    let (a, b) = (w.machine("machine-a"), w.machine("machine-b"));

    // Shared chain: root ← "work" (described) ← @ (undescribed, non-empty).
    a.write("work.txt", "work\n");
    a.jj(&["describe", "-m", "work"]);
    let work = a.wc_sha();
    a.jj(&["new"]);
    a.write("more.txt", "more\n");
    a.sync_ok();
    b.sync_ok();
    let tip = b.wc_sha();
    assert_eq!(tip, a.wc_sha());

    // B throws the whole chain away.
    b.jj(&["new", "root()"]);
    b.jj(&["abandon", "all() ~ root() ~ @"]);
    b.sync_ok();

    // A adopts the fresh @ (cycle 1), then cycles again (where the delayed
    // republish used to fire); B must stay clean afterwards.
    a.sync_ok();
    a.sync_ok();
    b.sync_ok();

    assert_eq!(a.wc_change(), b.wc_change());
    assert!(!a.dir.join("work.txt").exists());
    for m in [&a, &b] {
        assert!(!m.is_visible(&tip), "the old @ must stay abandoned");
        assert!(
            !m.is_visible(&work),
            "the described ancestor must stay abandoned"
        );
    }
    assert!(
        w.remote_ref("refs/jj-sync/heads").is_none(),
        "no head refs may be published for the abandoned chain"
    );
}

/// Divergent twin heads abandoned together must not keep each other alive:
/// the deleting machine drops the remote refs, and the other machine's
/// "modified here" sibling guard used to see each twin as the other's live
/// sibling — deadlocking both into a republish that bounced the whole chain
/// back. The abandon must also cascade to the twins' exposed parent, or it
/// resurfaces as a "new" head one cycle later.
#[test]
fn divergent_heads_abandoned_together_stay_abandoned() {
    let w = World::new();
    let (a, b) = (w.machine("machine-a"), w.machine("machine-b"));

    // Park a chain root ← "base" (described) ← parked, and move @ elsewhere.
    a.write("base.txt", "base\n");
    a.jj(&["describe", "-m", "base"]);
    let base = a.wc_sha();
    a.jj(&["new"]);
    a.write("park.txt", "parked\n");
    let parked_change = a.wc_change();
    a.jj(&["new", "root()"]);
    a.sync_ok();
    b.sync_ok();

    // Both machines rewrite the parked change concurrently → a divergent
    // twin pair, both published as anonymous heads.
    a.jj(&["describe", "-m", "from a", &parked_change]);
    b.jj(&["describe", "-m", "from b", &parked_change]);
    a.sync_ok();
    b.sync_ok();
    a.sync_ok();
    let twins: Vec<String> = b
        .jj(&[
            "log",
            "--no-graph",
            "-r",
            &format!("change_id({parked_change})"),
            "-T",
            "commit_id ++ \"\\n\"",
        ])
        .lines()
        .map(|l| l.trim().to_string())
        .collect();
    assert_eq!(twins.len(), 2, "expected a divergent pair, got {twins:?}");

    // B throws the twins (and thereby the chain) away.
    b.jj(&["abandon", "all() ~ root() ~ @"]);
    b.sync_ok();
    a.sync_ok(); // A must mirror the abandon, not republish the twins
    a.sync_ok(); // and not republish the exposed parent one cycle later
    b.sync_ok();

    for m in [&a, &b] {
        for t in &twins {
            assert!(!m.is_visible(t), "twin {t} must stay abandoned");
        }
        assert!(
            !m.is_visible(&base),
            "the exposed parent must stay abandoned"
        );
    }
    assert!(
        w.remote_ref("refs/jj-sync/heads").is_none(),
        "no head refs may survive for the abandoned chain"
    );
}

/// One machine rewrites the shared @'s change while the other stacks a child
/// on the old copy: sync must do what jj would have done inside one repo —
/// rebase the child onto the rewrite — not fork into a divergent change plus
/// an empty merge commit. Covered in both directions: the stacking machine
/// syncing second, and the rewriting machine syncing second.
#[test]
fn rewrite_propagates_as_rebase_not_merge() {
    let w = World::new();
    let (a, b) = (w.machine("machine-a"), w.machine("machine-b"));

    a.write("f.txt", "shared\n");
    a.sync_ok();
    b.sync_ok();
    let shared_change = a.wc_change();

    // A rewrites the shared change and publishes; B stacked a child on the
    // old copy and syncs second.
    a.write("f.txt", "rewritten\n");
    a.jj(&["describe", "-m", "rewritten"]);
    b.jj(&["new"]);
    b.write("stacked.txt", "stacked\n");
    b.jj(&["describe", "-m", "stacked"]);
    a.sync_ok();
    b.sync_ok(); // B rebases its child onto A's rewrite
    a.sync_ok(); // A adopts the rebased child

    for m in [&a, &b] {
        assert_eq!(m.read("f.txt"), "rewritten\n");
        assert_eq!(m.read("stacked.txt"), "stacked\n");
        assert_eq!(m.template("@", "description").trim(), "stacked");
        assert_eq!(m.template("@-", "description").trim(), "rewritten");
        assert_eq!(
            m.template(&format!("change_id({shared_change})"), "\"x\""),
            "x",
            "the rewritten change must have exactly one visible commit"
        );
    }
    assert_eq!(a.wc_sha(), b.wc_sha());

    // Other direction: A rewrites the (new) shared @'s change, but B stacks
    // on the old copy and publishes first — A syncs holding the rewrite.
    let stacked_change = a.wc_change();
    a.write("stacked.txt", "stacked v2\n");
    b.jj(&["new"]);
    b.write("top.txt", "top\n");
    b.jj(&["describe", "-m", "top"]);
    b.sync_ok();
    a.sync_ok(); // A rebases the incoming child onto its rewrite and lands on it
    b.sync_ok(); // B adopts

    for m in [&a, &b] {
        assert_eq!(m.read("stacked.txt"), "stacked v2\n");
        assert_eq!(m.read("top.txt"), "top\n");
        assert_eq!(m.template("@", "description").trim(), "top");
        assert_eq!(
            m.template(&format!("change_id({stacked_change})"), "\"x\""),
            "x",
            "the rewritten change must have exactly one visible commit"
        );
    }
    assert_eq!(a.wc_sha(), b.wc_sha());
    assert!(
        w.remote_ref("refs/jj-sync/heads").is_none(),
        "a propagated rewrite must not leave stray head refs"
    );
}

/// Visible commit ids of a change, sorted.
fn copies_of(m: &Machine, change: &str) -> Vec<String> {
    let mut v: Vec<String> = m
        .jj(&[
            "log",
            "--no-graph",
            "-r",
            &format!("change_id({change})"),
            "-T",
            "commit_id ++ \"\\n\"",
        ])
        .lines()
        .map(|l| l.trim().to_string())
        .filter(|l| !l.is_empty())
        .collect();
    v.sort();
    v
}

/// The flow that left one change with five divergent copies in practice.
/// A's @ carries an empty described child (parked head). A splits @ with
/// `jj commit <paths>`, which rewrites @'s change and rebases the child. B
/// adopts: its stale @ copy survives the working-copy cleanup because the
/// old child still hangs on it; when the child's stale twin is then replaced
/// by the adopted rewrite, the parent stands exposed as a childless head and
/// used to be published as "created here" — handing A its own predecessor
/// back as a divergent twin. Every further rewrite on A repeated it.
#[test]
fn stale_predecessor_exposed_on_adopt_is_not_republished() {
    let w = World::new();
    let (a, b) = (w.machine("machine-a"), w.machine("machine-b"));

    a.write("skill.md", "translated\n");
    a.write("media.md", "record\n");
    a.jj(&["describe", "-m", "translate"]);
    a.jj(&["new", "-m", "leftover"]);
    a.jj(&["edit", "@-"]);
    a.sync_ok();
    b.sync_ok();
    let change = a.wc_change();
    let stale = a.wc_sha();
    assert_eq!(b.wc_sha(), stale);

    // Rewrite 1: split @. Then rewrite the committed part twice more, the way
    // two follow-up squashes did in practice.
    a.jj(&["commit", "-m", "pr-media", "media.md"]);
    a.sync_ok();
    b.sync_ok();
    a.sync_ok();
    for round in 1..=2 {
        a.write("media.md", &format!("record v{round}\n"));
        a.jj(&["squash", "--into", "@-", "--use-destination-message"]);
        a.sync_ok();
        b.sync_ok();
        a.sync_ok();
        b.sync_ok();
    }

    for m in [&a, &b] {
        assert!(!m.is_visible(&stale), "the pre-split @ must stay hidden");
        assert_eq!(
            copies_of(m, &change).len(),
            1,
            "the rewritten change must have exactly one visible copy"
        );
    }
    assert_eq!(a.wc_sha(), b.wc_sha());
    assert_eq!(copies_of(&a, &change), copies_of(&b, &change));
    assert!(
        w.remote_ref(&format!("refs/jj-sync/heads/{stale}"))
            .is_none(),
        "the stale copy must never be published as a head"
    );
}

/// Same exposure through a bookmark: A's @ has a bookmarked child. A amends
/// @ (child and bookmark follow). B adopts @ and moves the bookmark to the
/// rebased child, leaving the old child as an unbookmarked, childless head —
/// which must be dropped, not published as a new head.
#[test]
fn stale_copy_exposed_by_bookmark_move_is_not_republished() {
    let w = World::new();
    let (a, b) = (w.machine("machine-a"), w.machine("machine-b"));

    a.write("base.txt", "base\n");
    a.jj(&["describe", "-m", "base"]);
    a.jj(&["new", "-m", "feature"]);
    a.write("feat.txt", "feat\n");
    a.jj(&["bookmark", "create", "feat", "-r", "@"]);
    a.jj(&["edit", "@-"]);
    a.sync_ok();
    b.sync_ok();
    let base_change = a.wc_change();
    let feat_change = a.template("feat", "change_id");
    let old_feat = a.template("feat", "commit_id");

    a.write("base.txt", "base v2\n");
    a.sync_ok();
    b.sync_ok();
    a.sync_ok();
    b.sync_ok();

    for m in [&a, &b] {
        assert!(
            !m.is_visible(&old_feat),
            "the old bookmark target must stay hidden"
        );
        assert_eq!(copies_of(m, &base_change).len(), 1);
        assert_eq!(copies_of(m, &feat_change).len(), 1);
        assert_eq!(m.template("feat", "description").trim(), "feature");
    }
    assert_eq!(a.wc_sha(), b.wc_sha());
    assert!(
        w.remote_ref("refs/jj-sync/heads").is_none(),
        "no anonymous head may be published for the bookmark's old target"
    );
}

/// Abandoning one copy of a divergent pair (both already synced) must
/// propagate. The other machine's "deletion meets edit" guard used to treat
/// the surviving twin as a local edit and republish the deleted copy — so a
/// user cleaning up divergence saw it bounce straight back.
#[test]
fn abandoning_a_synced_divergent_copy_propagates() {
    let w = World::new();
    let (a, b) = (w.machine("machine-a"), w.machine("machine-b"));

    a.write("park.txt", "parked\n");
    a.jj(&["describe", "-m", "parked"]);
    let parked_change = a.wc_change();
    a.jj(&["new", "root()"]);
    a.sync_ok();
    b.sync_ok();

    a.jj(&["describe", "-m", "from a", &parked_change]);
    b.jj(&["describe", "-m", "from b", &parked_change]);
    a.sync_ok();
    b.sync_ok();
    a.sync_ok();
    let twins = copies_of(&a, &parked_change);
    assert_eq!(twins.len(), 2, "expected a divergent pair, got {twins:?}");
    assert_eq!(copies_of(&b, &parked_change), twins);

    // A picks a winner by abandoning the copy that came from B.
    let from_b = twins
        .iter()
        .find(|t| a.template(t, "description").trim() == "from b")
        .unwrap()
        .clone();
    let from_a = twins.iter().find(|t| **t != from_b).unwrap().clone();
    a.jj(&["abandon", &from_b]);
    a.sync_ok();
    b.sync_ok();
    a.sync_ok();

    for m in [&a, &b] {
        assert!(
            !m.is_visible(&from_b),
            "the abandoned copy must stay abandoned"
        );
        assert!(
            m.is_visible(&from_a),
            "the surviving copy must stay visible"
        );
        assert_eq!(copies_of(m, &parked_change), vec![from_a.clone()]);
    }
    assert!(w
        .remote_ref(&format!("refs/jj-sync/heads/{from_b}"))
        .is_none());
}

/// M2: bookmark moves mirror across machines; moving differently on both
/// freezes the bookmark until `resolve` (local wins), which then propagates.
#[test]
fn bookmarks_move_freeze_resolve() {
    let w = World::new();
    let (a, b) = (w.machine("machine-a"), w.machine("machine-b"));

    a.write("f.txt", "one\n");
    a.jj(&["describe", "-m", "one"]);
    a.jj(&["new"]);
    a.jj(&["bookmark", "create", "feat", "-r", "@-"]);
    a.sync_ok();
    b.sync_ok();
    let one = b.template("feat", "commit_id");
    assert_eq!(
        one,
        a.template("feat", "commit_id"),
        "bookmark must reach B"
    );

    // A moves it; B follows.
    a.write("f.txt", "two\n");
    a.jj(&["describe", "-m", "two"]);
    a.jj(&["new"]);
    a.jj(&["bookmark", "set", "feat", "-r", "@-"]);
    a.sync_ok();
    b.sync_ok();
    assert_eq!(b.template("feat", "description").trim(), "two");

    // Both move it differently: freeze on the machine that syncs second.
    a.jj(&["bookmark", "set", "feat", "-r", "@", "--allow-backwards"]);
    b.jj(&["new", "feat", "-m", "b version"]);
    b.jj(&["bookmark", "set", "feat", "-r", "@"]);
    a.sync_ok();
    let frozen = b.sync();
    assert!(
        frozen
            .problems
            .iter()
            .any(|p| matches!(p, Problem::FrozenBookmark { bookmark, .. } if bookmark == "feat")),
        "expected feat to freeze, got {:?}",
        frozen.problems
    );
    let b_pos = b.template("feat", "commit_id");
    // Still frozen on the next cycle; nothing moved silently.
    let still = b.sync();
    assert!(still
        .problems
        .iter()
        .any(|p| matches!(p, Problem::FrozenBookmark { .. })));
    assert_eq!(b.template("feat", "commit_id"), b_pos);

    // The frozen bookmark differs from S by design, so it counts as pending —
    // but status renders it as ⚠ and must not repeat it in the pending list.
    assert!(b.pending().contains(&"bookmark feat".to_string()));
    let pending: std::collections::BTreeMap<String, Vec<String>> =
        [("machine-b".to_string(), b.pending())]
            .into_iter()
            .collect();
    let rendered = jjsync::report::Report {
        when: 0,
        repos: vec![still.clone()],
    }
    .render(&[], &[], &pending, 0);
    assert!(rendered.contains("⚠ feat"), "rendered: {rendered}");
    assert!(!rendered.contains("bookmark feat"), "rendered: {rendered}");

    // Explicit resolve: B's local position wins and propagates to A.
    let resolved = b.sync_with(CycleOpts {
        env: b.env.clone(),
        resolve: vec!["feat".into()],
        ..Default::default()
    });
    assert!(
        resolved.problems.is_empty(),
        "resolve must unfreeze: {:?}",
        resolved.problems
    );
    a.sync_ok();
    assert_eq!(
        a.template("feat", "commit_id"),
        b_pos,
        "A must follow the resolved position"
    );
}

/// Workspaces sync independently; a machine lacking a workspace skips it and
/// picks it up once the workspace exists there.
#[test]
fn multi_workspace_independent_and_skipped() {
    let w = World::new();
    let (a, b) = (w.machine("machine-a"), w.machine("machine-b"));

    a.write("main.txt", "main ws\n");
    a.sync_ok();
    b.sync_ok();

    // A gains a second workspace — no registration needed, the cycle
    // discovers workspace directories from jj itself.
    let a_feat = w.root.join("machine-a-feat");
    a.jj(&[
        "workspace",
        "add",
        "--name",
        "feat",
        a_feat.to_str().unwrap(),
    ]);
    fs::write(a_feat.join("feat.txt"), "feature ws\n").unwrap();
    let out = a.sync_ok();
    assert!(out.synced_workspaces.contains(&"feat".to_string()));
    assert!(w.remote_ref("refs/jj-sync/feat").is_some());

    // B lacks the workspace: skipped, default unaffected.
    let out = b.sync_ok();
    assert_eq!(out.synced_workspaces, vec!["default".to_string()]);
    assert_eq!(b.read("main.txt").trim(), "main ws");

    // B creates the workspace: the next cycle adopts A's state into it.
    let b_feat = w.root.join("machine-b-feat");
    b.jj(&[
        "workspace",
        "add",
        "--name",
        "feat",
        b_feat.to_str().unwrap(),
    ]);
    b.sync_ok();
    assert_eq!(
        fs::read_to_string(b_feat.join("feat.txt")).unwrap().trim(),
        "feature ws"
    );

    // Workspaces move independently.
    fs::write(a_feat.join("feat.txt"), "feature v2\n").unwrap();
    let default_sha = a.wc_sha();
    a.sync_ok();
    b.sync_ok();
    assert_eq!(
        fs::read_to_string(b_feat.join("feat.txt")).unwrap().trim(),
        "feature v2"
    );
    assert_eq!(a.wc_sha(), default_sha, "default workspace must not move");
}

/// Everything local that S doesn't record shows as pending — plain disk
/// edits, bookmark creates/moves/deletes, parked anonymous heads — and a
/// fully synced repo shows nothing.
#[test]
fn pending_reflects_unsynced_local_state() {
    let w = World::new();
    let a = w.machine("machine-a");

    // Fresh repo: the empty undescribed @ holds nothing to publish.
    assert!(a.pending().is_empty(), "got {:?}", a.pending());

    // A plain disk edit counts (the check snapshots), and syncing clears it.
    a.write("f.txt", "one\n");
    assert_eq!(a.pending(), vec!["@ (default)"]);
    a.sync_ok();
    assert!(a.pending().is_empty(), "got {:?}", a.pending());

    // Bookmark created (and @ rewritten by describe/new).
    a.jj(&["describe", "-m", "base"]);
    a.jj(&["new"]);
    a.jj(&["bookmark", "create", "feat", "-r", "@-"]);
    let p = a.pending();
    assert!(p.contains(&"@ (default)".to_string()), "got {p:?}");
    assert!(p.contains(&"bookmark feat".to_string()), "got {p:?}");
    a.sync_ok();
    assert!(a.pending().is_empty(), "got {:?}", a.pending());

    // A bookmark deletion is itself an unsynced change.
    a.jj(&["bookmark", "delete", "feat"]);
    assert_eq!(a.pending(), vec!["bookmark feat"]);
    a.sync_ok();
    assert!(a.pending().is_empty(), "got {:?}", a.pending());

    // Parking work: the old @ becomes an unpublished anonymous head and the
    // fresh @ is an unpublished working-copy move.
    a.write("park.txt", "parked\n");
    a.sync_ok();
    a.jj(&["new", "root()"]);
    let p = a.pending();
    assert!(p.contains(&"@ (default)".to_string()), "got {p:?}");
    assert!(p.contains(&"1 head".to_string()), "got {p:?}");
    a.sync_ok();
    assert!(a.pending().is_empty(), "got {:?}", a.pending());
}

/// Excluded bookmarks never leave the machine; offline is a silent non-event.
#[test]
fn excluded_bookmarks_and_offline() {
    let w = World::new();
    let mut a = w.machine("machine-a");
    a.cfg.exclude_bookmarks = vec!["wip/*".into()];

    a.write("f.txt", "content\n");
    a.jj(&["bookmark", "create", "wip/scratch", "-r", "@"]);
    a.jj(&["bookmark", "create", "keep", "-r", "@"]);
    a.sync_ok();
    assert!(w.remote_ref("refs/jj-sync/bookmarks/keep").is_some());
    assert!(w.remote_ref("refs/jj-sync/bookmarks/wip/scratch").is_none());
    // The unsynced-but-excluded bookmark must not show as pending either.
    assert!(a.pending().is_empty(), "got {:?}", a.pending());

    // Unreachable remote: no problems raised, nothing local changes.
    let wc = a.wc_sha();
    a.cfg.remote = "unreachable".into();
    a.git(&[
        "remote",
        "add",
        "unreachable",
        "https://127.0.0.1:1/nope.git",
    ]);
    let out = a.sync();
    assert!(out.offline, "an unreachable remote must count as offline");
    assert!(
        out.problems.is_empty(),
        "offline must be silent: {:?}",
        out.problems
    );
    assert_eq!(a.wc_sha(), wc);
}

/// The machine-fleet story end to end: `init` records the clone URL in the
/// shared config, `init --local` keeps a work repo out of it, a fresh machine
/// bootstraps colocated clones from the shared config alone, and `ignoreRepos`
/// in the local config opts a machine out of a shared repo.
#[test]
fn cli_local_config_bootstrap_and_ignore() {
    let w = World::new();
    let a = w.machine("machine-a");
    a.write("shared.txt", "shared content\n");

    // A second repo with its own remote — the work repo that must never
    // enter the shared (dotfiles-synced) config.
    let work_remote = w.root.join("work-remote.git");
    run_ok(
        "git",
        &["init", "-q", "--bare", work_remote.to_str().unwrap()],
        &w.root,
        &w.env,
    )
    .unwrap();
    let work = w.root.join("machine-a-work");
    fs::create_dir_all(&work).unwrap();
    run_ok("git", &["init", "-q"], &work, &w.env).unwrap();
    run_ok(
        "git",
        &["remote", "add", "origin", work_remote.to_str().unwrap()],
        &work,
        &w.env,
    )
    .unwrap();
    run_ok("jj", &["git", "init", "--colocate"], &work, &w.env).unwrap();
    fs::write(work.join("work.txt"), "work content\n").unwrap();

    let bin = env!("CARGO_BIN_EXE_jjsync");
    let run_cli = |args: &[&str], cwd: &Path, env: &Env| {
        let mut cmd = std::process::Command::new(bin);
        cmd.args(args).current_dir(cwd);
        for (k, v) in &env.vars {
            cmd.env(k, v);
        }
        let out = cmd.output().unwrap();
        assert!(
            out.status.success(),
            "jjsync {args:?} failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).into_owned()
    };

    run_cli(&["init"], &a.dir, &w.env);
    assert!(run_cli(&["init", "--local"], &work, &w.env).contains("this machine only"));

    let cfg_dir = w.root.join("home/.config/jjsync");
    let shared_text = fs::read_to_string(cfg_dir.join("config.json")).unwrap();
    let local_text = fs::read_to_string(cfg_dir.join("config.local.json")).unwrap();
    assert!(shared_text.contains(&a.dir.display().to_string()));
    assert!(
        shared_text.contains(&w.remote.display().to_string()),
        "init must record the clone url: {shared_text}"
    );
    assert!(
        !shared_text.contains("machine-a-work"),
        "--local must keep the work repo out of the shared config"
    );
    assert!(local_text.contains("machine-a-work"));
    assert!(local_text.contains(&work_remote.display().to_string()));
    assert_eq!(
        fs::read_to_string(cfg_dir.join(".gitignore")).unwrap(),
        "config.local.json\n",
        "the config dir must be safe to track in dotfiles"
    );

    // One sync covers both config files.
    run_cli(&["sync"], &w.root, &w.env);
    assert!(w.remote_ref("refs/jj-sync/default").is_some());
    let out = run_ok(
        "git",
        &[
            "for-each-ref",
            "--format=%(objectname)",
            "refs/jj-sync/default",
        ],
        &work_remote,
        &w.env,
    )
    .unwrap();
    assert!(
        !out.stdout.trim().is_empty(),
        "the local-config repo must sync too"
    );

    // "Machine B": a fresh home whose dotfiles delivered only the shared
    // config, with paths under this machine's own directory.
    let home2 = w.root.join("home2");
    fs::create_dir_all(home2.join(".config/jjsync")).unwrap();
    fs::create_dir_all(home2.join(".config/jj")).unwrap();
    fs::write(
        home2.join(".gitconfig"),
        "[user]\n\tname = Test\n\temail = test@example.com\n[init]\n\tdefaultBranch = main\n",
    )
    .unwrap();
    fs::write(
        home2.join(".config/jj/config.toml"),
        "user.name = \"Test\"\nuser.email = \"test@example.com\"\n",
    )
    .unwrap();
    let env2 = Env {
        vars: vec![
            ("HOME".into(), home2.display().to_string()),
            (
                "XDG_CONFIG_HOME".into(),
                home2.join(".config").display().to_string(),
            ),
            (
                "XDG_STATE_HOME".into(),
                home2.join(".state").display().to_string(),
            ),
            (
                "XDG_CACHE_HOME".into(),
                home2.join(".cache").display().to_string(),
            ),
            ("GIT_CONFIG_NOSYSTEM".into(), "1".into()),
            ("JJSYNC_NO_NOTIFY".into(), "1".into()),
            ("JJSYNC_NO_SYSTEMD".into(), "1".into()),
        ],
    };
    fs::write(
        home2.join(".config/jjsync/config.json"),
        shared_text.replace("machine-a", "machine-b"),
    )
    .unwrap();

    let st = run_cli(&["status"], &w.root, &env2);
    assert!(st.contains("○ not cloned"), "status was: {st}");

    run_cli(&["bootstrap"], &w.root, &env2);
    let b_dir = w.root.join("machine-b");
    assert!(
        b_dir.join(".jj").is_dir() && b_dir.join(".git").exists(),
        "bootstrap must produce a colocated clone"
    );

    run_cli(&["sync"], &w.root, &env2);
    assert_eq!(
        fs::read_to_string(b_dir.join("shared.txt")).unwrap(),
        "shared content\n"
    );
    let b_change = run_ok(
        "jj",
        &["log", "--no-graph", "-r", "@", "-T", "change_id"],
        &b_dir,
        &env2,
    )
    .unwrap()
    .stdout;
    assert_eq!(
        a.wc_change(),
        b_change.trim(),
        "the bootstrapped clone must join the shared working-copy change"
    );

    // And it is a full citizen: an edit on B reaches A.
    fs::write(b_dir.join("from-b.txt"), "hello from b\n").unwrap();
    run_cli(&["sync"], &w.root, &env2);
    a.sync_ok();
    assert_eq!(a.read("from-b.txt"), "hello from b\n");

    // ignoreRepos in the local config opts this machine out of a shared repo.
    fs::write(
        home2.join(".config/jjsync/config.local.json"),
        "{ \"ignoreRepos\": [\"machine-b\"] }\n",
    )
    .unwrap();
    let st = run_cli(&["status"], &w.root, &env2);
    assert!(
        st.contains("no repos configured"),
        "the ignored repo must vanish from status: {st}"
    );
}

/// The CLI end to end: init registers the repo, sync publishes, status reports.
#[test]
fn cli_init_sync_status() {
    let w = World::new();
    let a = w.machine("machine-a");
    a.write("f.txt", "via cli\n");

    let bin = env!("CARGO_BIN_EXE_jjsync");
    let run_cli = |args: &[&str], cwd: &Path| {
        let mut cmd = std::process::Command::new(bin);
        cmd.args(args).current_dir(cwd);
        for (k, v) in &w.env.vars {
            cmd.env(k, v);
        }
        let out = cmd.output().unwrap();
        assert!(
            out.status.success(),
            "jjsync {args:?} failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).into_owned()
    };

    assert!(run_cli(&["init"], &a.dir).contains("registered"));
    run_cli(&["sync"], &a.dir);
    assert!(w.remote_ref("refs/jj-sync/default").is_some());
    let status = run_cli(&["status"], &a.dir);
    assert!(status.contains("✓ synced"), "status was: {status}");

    // An edit after the sync shows as pending until the next cycle.
    a.write("f.txt", "edited after sync\n");
    let status = run_cli(&["status"], &a.dir);
    assert!(
        status.contains("● pending: @ (default)"),
        "status was: {status}"
    );
    run_cli(&["sync"], &a.dir);
    let status = run_cli(&["status"], &a.dir);
    assert!(status.contains("✓ synced"), "status was: {status}");
}

/// A lone auth failure is the remote throttling us, not a broken key: the
/// first one stays quiet and only a repeat in the next cycle escalates.
#[test]
fn auth_failure_escalates_only_when_it_repeats() {
    use std::os::unix::fs::PermissionsExt;

    let w = World::new();
    let mut a = w.machine("machine-a");
    a.write("f.txt", "content\n");
    a.sync_ok();

    // An ssh that denies every connection, the way a rate-limited GitHub does.
    let ssh = w.root.join("deny-ssh");
    fs::write(
        &ssh,
        "#!/bin/sh\necho 'git@github.com: Permission denied (publickey).' >&2\nexit 255\n",
    )
    .unwrap();
    fs::set_permissions(&ssh, fs::Permissions::from_mode(0o755)).unwrap();
    a.cfg.remote = "denied".into();
    a.git(&["remote", "add", "denied", "ssh://git@example.invalid/r.git"]);
    let mut env = a.env.clone();
    env.vars
        .push(("GIT_SSH_COMMAND".into(), ssh.display().to_string()));

    let denied = |env: &Env| {
        a.sync_with(CycleOpts {
            env: env.clone(),
            ..Default::default()
        })
    };
    let first = denied(&env);
    assert!(
        first.problems.iter().any(is_auth),
        "the wire error must classify as auth: {:?}",
        first.problems
    );

    // First strike: held back, silent in status apart from the retry marker.
    let empty = std::collections::BTreeMap::new();
    let mut report = jjsync::report::Report {
        when: 0,
        repos: vec![first],
    };
    report.debounce_auth(None);
    assert!(
        !report.repos[0].problems.iter().any(is_auth),
        "a single auth failure must not surface as a problem"
    );
    let rendered = report.render(&[], &[], &empty, 0);
    assert!(rendered.contains("retrying"), "rendered: {rendered}");
    assert!(!rendered.contains('⚠'), "rendered: {rendered}");

    // Same failure the next cycle: now it is real, and it is loud.
    let mut next = jjsync::report::Report {
        when: 0,
        repos: vec![denied(&env)],
    };
    next.debounce_auth(Some(&report));
    assert!(
        next.repos[0].problems.iter().any(is_auth),
        "a repeated auth failure must escalate"
    );
    let rendered = next.render(&[], &[], &empty, 0);
    assert!(rendered.contains("⚠ auth error"), "rendered: {rendered}");

    // And once the remote lets us in again, the repo goes back to healthy.
    a.cfg.remote = "origin".into();
    a.sync_ok();
}

fn is_auth(p: &Problem) -> bool {
    matches!(p, Problem::Auth { .. })
}

/// A repo nobody has touched in a week drops to the slow lane: the timer skips
/// it until the idle interval is up, and syncs it normally once it does.
#[test]
fn dormant_repos_sync_on_the_slow_lane() {
    let w = World::new();
    let (a, b) = (w.machine("machine-a"), w.machine("machine-b"));
    a.write("shared.txt", "from a\n");

    let bin = env!("CARGO_BIN_EXE_jjsync");
    let cli = |args: &[&str], cwd: &Path| {
        let mut cmd = std::process::Command::new(bin);
        cmd.args(args).current_dir(cwd);
        for (k, v) in &w.env.vars {
            cmd.env(k, v);
        }
        let out = cmd.output().unwrap();
        assert!(
            out.status.success(),
            "jjsync {args:?} failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).into_owned()
    };
    let cfg_path = w.root.join("home/.config/jjsync/config.json");
    let set = |key: &str, from: &str, to: &str| {
        let text = fs::read_to_string(&cfg_path).unwrap();
        let (old, new) = (format!("\"{key}\": {from}"), format!("\"{key}\": {to}"));
        assert!(text.contains(&old), "config has no {key}: {text}");
        fs::write(&cfg_path, text.replace(&old, &new)).unwrap();
    };

    cli(&["init"], &a.dir);
    cli(&["sync"], &a.dir);
    b.sync_ok();

    // B publishes something new while A has been idle for over a week
    // (idleAfterSeconds 0: every repo counts as dormant).
    b.write("from-b.txt", "from b\n");
    b.sync_ok();
    set("idleAfterSeconds", "604800", "0");
    cli(&["sync"], &a.dir);
    assert!(
        !a.dir.join("from-b.txt").exists(),
        "a dormant repo must not fetch on every tick"
    );
    // ...and its status line still shows the cycle that actually ran it.
    assert!(cli(&["status"], &a.dir).contains("✓ synced"));

    // The quarter hour comes around (idleIntervalSeconds 0 stands in for it).
    set("idleIntervalSeconds", "900", "0");
    cli(&["sync"], &a.dir);
    assert_eq!(
        a.read("from-b.txt"),
        "from b\n",
        "the slow lane must still sync"
    );

    // Back to a week: A has just worked, so it is on every tick again.
    set("idleAfterSeconds", "0", "604800");
    set("idleIntervalSeconds", "0", "900");
    b.write("again.txt", "b again\n");
    b.sync_ok();
    cli(&["sync"], &a.dir);
    assert_eq!(a.read("again.txt"), "b again\n", "an active repo syncs now");
}
