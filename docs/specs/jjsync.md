# jjsync — Specification

Background synchronization for Jujutsu repositories — "Google Drive for jj":
edit files on one computer, continue from the same state on another. No
`jj describe`, no manual commits, no bookmark juggling. Git transports the
state; Jujutsu defines the state and protects concurrent edits.

Vocabulary: see `docs/CONTEXT.md`. Decisions and their rationale: `docs/adr/`.
Raw design sessions: `docs/grillings/`.

## Core model

A jj working copy is a commit (`@`), and jj 0.41 writes change-id headers into
git commits — so a logical workspace's working copy can be **the same jj change
on every machine**. Snapshots amend it; sync reconciles its versions. Concurrent
edits become a jj *divergent change*, merged by jjsync; a conflicting merge is
an ordinary jj conflict in the files. jjsync adds no version-control semantics
of its own.

The unit of synchronization is the **logical workspace**: `(repository,
workspace name)`, machine-independent. A repo without explicit `jj workspace`
use is workspace `default`. Workspace names are assumed identical across
machines; workspaces are auto-discovered via `jj workspace list`; a machine
lacking a workspace skips it.

## Transport (ADR 0001)

`jj git push` refuses undescribed commits, so jjsync bypasses jj's push
entirely and moves **raw git refs**:

```text
refs/jj-sync/<workspace>         workspace working copy      (M1)
refs/jj-sync/heads/<sha>         anonymous visible heads     (M2)
refs/jj-sync/bookmarks/<name>    local bookmark positions    (M2)
```

- Sync remote: the repo's `origin` by default; per-repo override.
- `refPrefix` per repo, default `refs/jj-sync/` (invisible as branches on
  GitHub). Work setups may use e.g. `refs/heads/users/hector/jjsync/`.
- Every ref update and deletion is CAS: `git push --force-with-lease=<ref>:<expected>`.
  Never an unconditional force. Batches are atomic (`git push --atomic`).
- Receiving: fetch the namespace, point local `refs/heads/__jj_sync/...`
  branches at the commits, `jj git import`. Change-ids, divergent changes, and
  conflicted commits all survive transport (verified, jj 0.41).
- Repos must be colocated (`.git` beside `.jj`). Non-colocated: out of scope for now.

## Sync cycle

One idempotent pass per repo, safe to repeat or interrupt:

```text
jj util snapshot → fetch sync namespace → three-way reconcile → publish (CAS)
```

All state lives in git refs. Each machine keeps **S**, a local mirror of the
sync-ref set after its last successful cycle, in plain local git refs
(`refs/jj-sync/s/*` — not a jj bookmark: jj moves bookmarks along with
rewrites, see ADR 0001 addendum). Reconciliation per item (ADR 0003):

| local | S | remote | meaning | action |
|---|---|---|---|---|
| ✓ | – | – | created here | publish |
| – | – | ✓ | created there | adopt |
| – | ✓ | ✓ | deleted here | delete remote ref |
| ✓ untouched | ✓ | – | deleted there | abandon locally |
| ✓ modified | ✓ | – | deletion meets edit | edit wins — resurrect |
| moved | ✓ | moved | divergence | see below |

Divergence by item type:

- **Workspace `@`**: merge immediately — `jj new <local> <remote>` becomes the
  new working copy and is published. Clean merge or ordinary jj conflict in the
  files on both machines; nothing waits for the user.
- **Anonymous heads**: no merge — both versions coexist (jj divergent change).
- **Bookmarks**: freeze — stop syncing that bookmark, show both positions,
  wait for explicit `jjsync resolve <bookmark>` ("my local position wins").

Adopt mechanics (ADR 0002): `jj edit` the fetched commit; the stale
byte-identical local `@` is abandoned. A head is only treated as abandoned if
no visible commit shares its change-id and it is not an ancestor of any current
head — the same guard runs before abandoning anything on adopt.

## Leak gate

Before every publish, the outgoing content is scanned with gitleaks. On a hit,
that repo's publishing blocks (⚠ status + desktop notification; local jj use
and other repos unaffected) until fixed or allowlisted. Default on, per-repo
opt-out — sync state on a public repo's refs is publicly fetchable.

## Background service

`jjsync sync` runs as a systemd user **timer** + oneshot (default 60 s,
configurable). No daemon process: the timer is the debounce, and crash-safety
follows from cycle idempotence. `pause`/`resume` wrap `systemctl --user`.
Linux/Arch + Arch WSL (requires `systemd=true` in `/etc/wsl.conf`; syncs only
while the WSL VM runs).

Offline is a non-event: cycles fail silently on fetch, local jj use is never
affected, and reconnection is handled by the same reconcile rules as any other
concurrent change.

## Configuration

`~/.config/jjsync/config.json`, managed by `jjsync init` (run inside a repo;
refuses non-colocated) and hand-editable:

```json
{
  "intervalSeconds": 60,
  "repos": [
    {
      "path": "~/dotfiles",
      "url": "git@github.com:you/dotfiles.git",
      "remote": "origin",
      "refPrefix": "refs/jj-sync/",
      "leakScan": true,
      "excludeBookmarks": []
    }
  ]
}
```

`config.json` is the shared file: it travels with the user's dotfiles so every
machine sees the same repo list, and `url` (recorded by `init` from the
remote) is what `jjsync bootstrap` clones from on a machine where `path` does
not exist yet. `config.local.json` beside it is machine-only and never synced:
its `repos` concatenate onto the shared list (`jjsync init --local`), and its
`ignoreRepos` — entries matching a repo's path or directory name — drops
shared repos on this machine. A configured repo whose path is absent is a
non-event during sync (like offline) and shows as "not cloned" in `status`.

## CLI

```text
jjsync init        register cwd repo in the config (--local: this machine only)
jjsync bootstrap   clone configured repos missing on this machine (colocated)
jjsync sync        run one cycle for all repos (what the timer fires)
jjsync status      per-repo state; one line per problem, ✓ when healthy
jjsync resolve     unfreeze a diverged bookmark: local position wins
jjsync pause       stop the timer
jjsync resume      start the timer
```

Successful sync is silent. Divergence conflicts and persistent failures (auth
errors — not mere offline) raise `notify-send` and appear in `status`:

```text
dotfiles/default     ✓ synced
dotfiles   main      ⚠ frozen: moved on both machines (a1b2c3 / d4e5f6)
project              ⚠ publish blocked: gitleaks hit in .env.local
```

## Milestones

**M1 — working copies.** Config + init, workspace-`@` sync for all workspaces,
adopt/publish/divergence-merge, leak gate, timer unit, status. Usable alone:
everything reachable from `@` (described commits included) already travels as
ancestors.

**M2 — full view.** Anonymous heads (`heads/<sha>` set semantics + temp-branch
import), bookmark sync with freeze/resolve, deletion propagation per the
reconcile table. Pure addition to the M1 namespace — no migration.

Explicitly out of scope: op log / undo history (per machine, like shell
history), evolog/predecessors, working-copy tracking state, ignored files,
non-colocated repos, workspace-name mapping, filesystem notifications
(a later optimization — the cycle is trigger-agnostic).

## Testing

Few, broad, reality-close integration tests — no mocks. A test harness builds
`tempdir/{remote.git, machine-a, machine-b}`: a bare git repo as the sync
remote and two colocated jj clones as the machines, driven by the real `jj`
and `git` binaries. `file://` transport behaves identically to a real remote
for everything jjsync does (refs, `--force-with-lease`, atomic pushes). Tests
point `HOME`/`XDG_CONFIG_HOME` into the tempdir so real user config never
leaks in — hermetic and parallelizable.

Each test is a full user scenario running real sync cycles alternately as A
and B, asserting on the resulting jj state. The scenarios follow from the
reconcile table and the invariants:

- roundtrip: edit on A → sync A → sync B → same content *and same change-id* on B
- divergence merging cleanly (different files), and conflicting (same line) —
  conflict markers land on both machines; resolving on B propagates to A
- CAS race: both changed, A publishes first → B's push fails the lease,
  B fetch-merge-republishes, nothing lost
- `jj describe` + `jj new` on A → described commit reaches B as an ancestor;
  `main` untouched on both machines
- leak gate: planted secret blocks that repo's publish; other repos unaffected
- idempotence: rerunning a cycle changes nothing; interrupted cycles resume safely
- M2: head created / abandoned / resurrected; bookmark move, freeze, resolve
- multi-workspace: workspaces sync independently; a missing workspace is skipped

Because the suite runs real binaries, it doubles as the canary for jj
upgrades — e.g. the change-id-header dependency in ADR 0001 must fail loudly
here if a new jj version breaks it.

The in-cycle race window (remote ref moves between fetch and publish) requires
the cycle to be a library function with injectable steps, so a test can move
the ref mid-cycle — structure the code accordingly.

Beyond `cargo test`: dogfood M1 with a private scratch GitHub repo and two
checkouts on one machine with the timer live (covers the systemd unit and real
network), then real dotfiles between the desktop and WSL.

## Safety invariants

1. Never publish to a project's normal branch namespace; never move a user
   bookmark except to mirror the user's own move from another machine.
2. Never require descriptions for sync.
3. Never silently discard concurrent remote or local state; modification
   always beats deletion.
4. Every ref write is CAS (`--force-with-lease`) — no unconditional force.
5. Fetch before publish whenever remote state may have changed.
6. jj conflicts may remain unresolved indefinitely and must survive transport.
7. Normal jj usage must never depend on jjsync functioning.
