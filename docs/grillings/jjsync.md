# Grilling: jjsync MVP (2026-08-14)

# Questions and Answers

- **Language?** Rust. Bash rejected (state logic, config, status table get painful); single static binary for multiple machines; sync outcomes modeled as enums.
- **Where does the project live?** Own repo: `~/Projects/jjsync`.
- **Background execution?** systemd user **timer** (default 60 s, configurable) firing a oneshot `jjsync sync` — no long-lived daemon, no own debounce (the timer coalesces edits). `pause`/`resume` = thin wrappers around `systemctl --user`. Crash-safe because one cycle is idempotent.
- **Platforms?** Arch desktop + Arch WSL (work laptop). Linux-only MVP. WSL needs `systemd=true` in `/etc/wsl.conf` and only syncs while the WSL VM runs — inherent, accepted.
- **Repo targeting?** Central `~/.config/jjsync/config.json`. `jjsync init` registers cwd (refuses non-colocated repos). Per repo: `path`, `remote` (default `origin`), `refPrefix` (default `refs/jj-sync/`; at work e.g. `refs/heads/users/hector/jjsync/` — then visible as branches, by choice), `leakScan` (default true).
- **Apply semantics?** Full auto — the working copy follows sync state without user action. "That's the point; worst case I get a jj conflict, which is easy to resolve."
- **Multi-workspace?** In MVP. Workspace names assumed identical across machines (no mapping config). Workspaces auto-discovered via `jj workspace list` — config lists repos only. A machine lacking a workspace skips its ref.
- **Sync remote?** `origin` (even though dotfiles is public) — a separate private store repo rejected as complexity. Mitigation: **leak gate** — gitleaks scan before every publish; on hit that repo's publishing blocks (⚠ status + notification, other repos unaffected) until fixed or allowlisted. Default on for all repos, per-repo opt-out.
- **Last-synced tracking?** No state files. The local `__jj_sync/<ws>` bookmark IS the marker (S); moves only after successful publish/adopt. Three-way compare of working copy (W), S, remote ref (R) decides: synced / publish / adopt / diverged.
- **Adopt mechanics?** `jj edit` the fetched commit — the working copy is literally the same jj change on all machines (change-ids survive transport). Stale byte-identical local @ is abandoned. Content-copy alternative rejected (reinvents divergence detection).
- **Divergence?** Immediate auto-merge (`jj new <local> <remote>`); clean merge or ordinary jj conflict lands in the files on both machines; result published. Parked-divergence alternative rejected (breaks Drive feel). "I want it to become a jj divergent change / merge conflict as fast as possible."
- **Surfacing?** Successful sync silent. Conflicts and persistent failures: `notify-send` + `jjsync status`. (WSL: status/log only.)
- **Sync all anonymous heads too?** Deferred at first, then designed fully — see `jjsync-view-sync.md`.

# Research

All verified experimentally with jj 0.41 in sandbox repos (bare remote + two colocated clones):

- `jj git push` **refuses undescribed commits** ("Won't push commit … since it has no description"), no bypass flag → jj bookmark push cannot be the transport.
- Raw ref push works: `git push origin <sha>:refs/jj-sync/default`. Non-branch namespaces don't appear as GitHub branches.
- jj does **not** index commits fetched into arbitrary namespaces — but pointing a local git branch (`refs/heads/__jj_sync/<ws>`) at the sha + `jj git import` makes it visible.
- **Change-id survives transport** — jj 0.41 writes change-id headers into git commits.
- Same change edited on two machines → jj **divergent change** (`oommsrtm/0` / `oommsrtm/1`); `jj new A B` merges; a same-line conflict becomes a normal jj conflict; the **conflicted commit survives push/fetch intact**.
- `git push --force-with-lease=<ref>:<expected-sha>` works on custom namespaces → atomic CAS publish.
- An anonymous commit **stays visible** as a head after temp-branch import + bookmark delete (enables view sync).
- jj has **no staging area** — everything in the working copy is in `@` after snapshot.
- `HectorBjernersjo/dotfiles` is **PUBLIC** on GitHub → leak gate motivation (custom refs on a public repo are publicly fetchable).
