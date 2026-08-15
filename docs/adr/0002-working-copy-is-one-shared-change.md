# 0002 — The working copy is one jj change shared by all machines

## Status

Accepted (2026-08-14)

## Context

When a machine adopts newer sync state, its working copy could either become
*the same jj change* as on the other machine (`jj edit <fetched-commit>` —
possible because change-ids survive git transport on jj 0.41), or stay a
machine-local change that merely receives the content (`jj new`/`jj restore`).

With machine-local changes, "these two working copies represent the same
state" exists only in jjsync's own bookkeeping, and detecting concurrent edits
becomes custom logic. With one shared change, concurrent edits on two machines
are by definition two versions of the same change — jj's built-in *divergent
change* concept, which jj tracks, displays, and lets us merge.

## Decision

Adopt = `jj edit` the fetched commit. A logical workspace's working copy is the
same jj change on every machine; snapshots amend it, sync reconciles its
versions. Divergence is detected and represented by jj itself and is
auto-merged immediately (`jj new <local> <remote>`); a conflicting merge lands
in the files as an ordinary jj conflict on both machines. The stale local
working-copy commit (byte-identical to the last-synced state) is abandoned on
adopt.

## Consequences

- jjsync adds no version-control semantics of its own — the spec's core
  design principle.
- Conflict markers can appear in files while the user is editing; this is the
  accepted "Google Drive" trade-off, surfaced via desktop notification, and
  always undoable through jj's op log.
- Anonymous merge snapshots accumulate in the DAG only when divergence
  actually happens.
