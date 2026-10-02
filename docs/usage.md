# Using jjsync

Back to the [README](../README.md). The design lives in [specs/jjsync.md](specs/jjsync.md),
the vocabulary in [CONTEXT.md](CONTEXT.md) and the decisions in [adr/](adr/).

## Requirements

- Linux with systemd (Arch and Arch WSL are the tested targets; WSL needs
  `systemd=true` in `/etc/wsl.conf`)
- `jj` 0.41+, `git`, and [`gitleaks`](https://github.com/gitleaks/gitleaks) on PATH
  (the leak gate fails closed without it)
- Colocated repos (`.git` beside `.jj`)

## Setup

```console
$ cargo install --path .
$ cd ~/dotfiles          # any colocated jj repo with an origin remote
$ jjsync init            # registers the repo, installs + starts the systemd user timer
```

On the next machine, either repeat `jjsync init` in its clone of each repo, or
— if `config.json` reaches the machine some other way, e.g. synced with your
dotfiles — run `jjsync bootstrap` once: it clones every configured repo that
is missing (colocated, from the recorded `url`) and installs the timer. The
timer runs a sync cycle every 60 s (configurable in
`~/.config/jjsync/config.json`).

## Commands

```text
jjsync init        register the cwd repo in the config
                   (--local: this machine only, in config.local.json)
jjsync bootstrap   clone configured repos missing on this machine
jjsync sync        run one cycle for all repos (what the timer fires)
jjsync status      one line per repo (✓/○/⚠, unsynced local state as ● pending,
                   last-sync age), plus timer health
jjsync resolve X   unfreeze diverged bookmark X: local position wins
jjsync pause       stop the timer
jjsync resume      start the timer
```

Successful sync is silent. Diverged bookmarks, leak-gate hits, and auth failures
raise a desktop notification and show in `jjsync status`. Offline is a non-event.

## Configuration

`~/.config/jjsync/config.json`, managed by `jjsync init` and hand-editable:

```json
{
  "intervalSeconds": 60,
  "idleAfterSeconds": 604800,
  "idleIntervalSeconds": 900,
  "repos": [
    {
      "path": "/home/you/dotfiles",
      "url": "git@github.com:you/dotfiles.git",
      "remote": "origin",
      "refPrefix": "refs/jj-sync/",
      "leakScan": true,
      "excludeBookmarks": ["wip/*"]
    }
  ]
}
```

A repo with no jj operation for `idleAfterSeconds` is dormant: nobody is
working in it and no cycle has moved anything, so it runs one cycle every
`idleIntervalSeconds` instead of every tick. Ten idle repos otherwise cost ten
SSH handshakes a minute for nothing, and GitHub answers a rate-limited
handshake with `Permission denied (publickey)`. Editing anything puts the repo
back on the fast lane; `jjsync status` shows each repo's own last-sync age.
A single auth failure is treated as that same throttling: it shows as
`○ auth failed once (retrying)` and only becomes a ⚠ if the next cycle fails
the same way.

`config.json` is meant to be shared across machines (symlink it from your
dotfiles): every machine acts on the same repo list, and `jjsync bootstrap`
clones whatever is missing using each repo's `url`. Machine-only state goes in
`config.local.json` next to it, which is never meant to leave the machine: its
`repos` are added to the shared list (register with `jjsync init --local` —
e.g. a work repo that shouldn't appear in personal dotfiles), and its
`ignoreRepos` (paths or directory names) opts this machine out of shared
repos:

```json
{
  "repos": [{ "path": "/home/you/work-repo", "url": "..." }],
  "ignoreRepos": ["some-shared-repo"]
}
```

`refPrefix` can point into a branch namespace (e.g.
`refs/heads/users/you/jjsync/`) where custom namespaces are blocked. The leak
gate scans every outgoing commit with gitleaks; ignore a false positive by
putting a `gitleaks:allow` comment on the offending line, allowlist patterns
repo-wide with a `.gitleaks.toml`, or set `"leakScan": false` per repo.

## Development

`cargo test` runs the integration suite: real `jj`/`git` binaries against a bare
remote and two clones in a tempdir, one full user scenario per test. It doubles
as the canary for jj upgrades — the transport depends on jj writing change-id
headers into git commits (ADR 0001). `JJSYNC_DEBUG=1` traces every reconcile
decision.
