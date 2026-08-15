# jjsync — Context

Glossary for the jjsync domain. jjsync makes a Jujutsu working copy available on
other machines without touching normal Git history ("Google Drive for jj repos").

## Terms

**Logical workspace** — the unit of synchronization: `(repository, workspace name)`.
Machine-independent: `~/dotfiles` on the laptop and `~/src/dotfiles` on the desktop
with workspace `default` are the *same* logical workspace. A repo where the user
never runs `jj workspace` has exactly one logical workspace, `default`.

**Sync state** — the current working-copy commit of a logical workspace, including
unresolved jj conflicts. What jjsync transports. Never includes jj operation
history, ignored files, or anything outside the jj snapshot.

**Sync remote** — the git remote that carries sync state. By default the
project's ordinary `origin`; sync state lives in its own ref namespace and never
appears in the project's normal branches unless deliberately configured to.

**Sync ref** — the single ref per logical workspace on the sync remote that points
at its latest sync state. Its namespace is configurable per repository.

**Sync cycle** — one idempotent pass for one logical workspace:
snapshot → fetch → reconcile → publish. The only operation jjsync performs;
running it again is always safe.

**Publish** — updating the sync ref to the local sync state. Always conditional on
the sync ref not having moved since last observed (never an unconditional force).

**Adopt** — moving the local working copy to a newer sync state fetched from the
sync remote. Happens automatically when the local working copy is unchanged since
the last cycle.

**View** — everything jj considers current in a repository: the visible
anonymous heads, local bookmark positions, and each workspace's working copy.
The full unit of synchronization. Operation history (undo), change-evolution
history, and working-copy tracking stay per machine.

**Last-synced state (S)** — the sync state a machine and the sync remote last
agreed on, mirrored locally. Comparing local state and the remote against it is
what tells "created/changed here", "created/changed there", "deleted there",
and "both changed" apart.

**Divergence** — both machines changed the same logical workspace since their last
common sync state. Resolved immediately by merging; a clean merge becomes the new
sync state, a conflicting merge becomes an ordinary jj conflict in the working
copy. Neither side is ever discarded, and nothing waits for the user.

**Leak gate** — a secret scan that every publish must pass. On a hit the repo's
publishing is blocked and surfaced to the user until fixed or allowlisted; local
work and other repos are unaffected. On by default, per-repo opt-out.

**Resurrection** — a deleted change or bookmark reappearing because another
machine modified it after the deletion. Deletion only applies to untouched
state; modification always wins over deletion.

**Freeze** — jjsync's response to a bookmark moved differently on two machines:
it stops syncing that bookmark, surfaces both positions, and waits for the user
to resolve it explicitly. The only situation where synchronization requires a
user decision.

**User bookmark** — any jj bookmark the user manages (`main`, feature branches).
jjsync moves them only to mirror the user's own moves from another machine, and
never publishes them to a project's normal branch namespace.
