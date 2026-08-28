# 0004 — Cross-machine rewrites propagate as rebases, not forks

## Status

Accepted (2026-08-15)

## Context

Inside one repo, jj propagates a rewrite by rebasing the change's descendants
onto the successor — amend a commit and everything stacked on it follows. The
record that makes this possible (predecessors) lives in each clone's op log
and does not travel over git refs (ADR 0001).

So when machine A rewrites a change that machine B has stacked children on,
B's reconcile sees two visible commits of one change with no git ancestry
between the working copies. The pre-0004 cycle treated that as a fork: it
merged the working copies (ADR 0002), leaving a permanently divergent change,
an empty merge commit, and history that jj-in-one-repo would never have
produced. The most common everyday flow — one machine amends, the other
builds on top — degraded into the representation reserved for genuine
conflicts.

## Decision

Before the fork branch of the workspace reconcile merges, repair
cross-machine rewrites: for a change with exactly two visible commits where
one copy is reachable from the last-synced state S (ADR 0003) and the other
is not, the S-reachable copy is stale and the other is its rewrite. Rebase
the stale copy's children onto the rewrite and abandon the stale copy —
exactly what jj would have done had the rewrite happened locally. Then rerun
the ancestry checks; whatever relationship remains (usually plain
publish/adopt) proceeds as before.

Everything ambiguous is left for the merge fallback: no S, both or neither
copy reachable from S, three-plus visible copies, copies outside the two
working-copy chains, or a stale copy pinned by a workspace or local ref.
Genuine divergence — the same change edited on both machines — stays
divergent and merges per ADR 0002.

## Consequences

- "One machine amends, the other stacks" converges to the linear history jj
  itself would produce: no divergent change, no anonymous merge commit.
- The repair runs on whichever machine syncs second and the result is
  published; the other machine adopts it like any other move.
- A rebase can surface conflicts; they land in the files as ordinary jj
  conflicts, same as the merge fallback, and remain undoable via the op log.
- The stale-copy orientation relies on S, so a machine with lost S (fresh
  clone, interrupted cycle) degrades to the old merge behavior for one cycle
  rather than guessing wrong.

## Addendum (2026-08-28): stale copies outside the workspace reconcile

The same shape appears outside the fork branch. When B adopts A's rewritten
@ or moves a bookmark to A's rewritten child, B's old copy can survive the
cleanup (its old child still hangs on it) and then stand exposed one step
later as a childless anonymous head. The heads phase used to read that as
"created here" and publish it, handing A its own predecessor back as a
divergent twin; every further rewrite on A repeated it (observed: one change
with five visible copies).

The heads phase now applies this ADR's orientation to every divergent change
before publishing: a copy reachable from S with a sibling S cannot reach is
the predecessor, and where it stands exposed it is abandoned (with the usual
cascade to what it exposes) instead of published. Copies the remote still
reaches are pinned and stay, so a divergence the other machine keeps is not
resolved behind its back. The "deletion meets edit" guard for heads deleted
on the other machine counts only siblings S cannot reach as an edit: a
sibling both machines already synced is a divergent twin the other machine
kept, so abandoning one copy of a synced pair now propagates instead of
bouncing back.
