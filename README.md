# jjsync

"Google Drive for [Jujutsu](https://jj-vcs.github.io/)": edit files on one computer and
continue from the same state on another. No `jj describe`, no manual commits, no bookmark
juggling.

```console
laptop $ cd ~/notes && jjsync init    # register the repo, start the background timer
laptop $ vim todo.md                   # just edit, nothing to commit

desktop $ jjsync bootstrap             # once: clone the configured repos, start the timer
desktop $ jjsync status                # a minute later
notes                    ✓ synced (default) · 12s ago
desktop $ cat ~/notes/todo.md          # the laptop's edits are already here
```

## Install

Needs Linux with systemd, `jj` 0.41+, `git` and [`gitleaks`](https://github.com/gitleaks/gitleaks).

```sh
cargo install --git https://github.com/HectorBjernersjo/jjsync
```

## How it works

- **Git is only the transport.** State travels as raw refs under `refs/jj-sync/`, so it
  never shows up as branches.
- **Concurrent edits are a jj divergent change.** jjsync merges them automatically, and a
  real clash becomes an ordinary jj conflict in the files, not a lost edit.
- **Leak gate that fails closed.** Every outgoing commit is scanned with gitleaks. If
  gitleaks is missing or broken, nothing is published.
- **Quiet by default.** Successful syncs are silent and offline is a non-event. Diverged
  bookmarks, leak hits and auth failures raise a desktop notification.
- **Cheap when idle.** Repos nobody touches drop to a slow lane instead of costing an SSH
  handshake every minute.

## Docs

- [Usage](docs/usage.md): setup on more machines, commands, configuration
- [Design spec](docs/specs/jjsync.md): the full design
- [Decisions](docs/adr/): why raw refs, one shared working-copy change, and change-id
  rewrite propagation
- [Vocabulary](docs/CONTEXT.md)
