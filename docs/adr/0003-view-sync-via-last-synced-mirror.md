# 0003 — View sync via ref sets and a local last-synced mirror, no tombstones

## Status

Accepted (2026-08-14)

## Context

Milestone 2 syncs the whole jj view: anonymous visible heads and local bookmark
positions, not just workspace working copies. Set synchronization has a classic
ambiguity: an item present on one side and absent on the other could mean
"deleted there" or "created here". Tombstone refs were considered to mark
deletions explicitly.

## Decision

No tombstones. Each machine keeps **S**: a local mirror (plain local git refs)
of the sync-ref set as it stood after its last successful cycle. Three-way
comparison of local state, S, and the remote resolves every case:

| local | S | remote | meaning | action |
|---|---|---|---|---|
| ✓ | – | – | created here | publish |
| – | – | ✓ | created there | adopt |
| – | ✓ | ✓ | deleted here | delete remote ref (CAS) |
| ✓ untouched | ✓ | – | deleted there | abandon locally |
| ✓ modified | ✓ | – | deletion meets edit | edit wins — resurrect |
| moved | ✓ | moved | divergence | heads coexist · bookmarks freeze · @ merges |

Wire format: `refs/jj-sync/<ws>` (workspace @), `refs/jj-sync/heads/<sha>`
(sha-keyed set — divergent versions of one change coexist as two refs),
`refs/jj-sync/bookmarks/<name>`. Pushes batched with `--atomic`; every update
and deletion is CAS via `--force-with-lease` against the S value.

Guard: a head counts as abandoned only if no visible commit shares its
change-id and it is not an ancestor of any current head; the same check runs
before abandoning anything on adopt.

## Consequences

- Deletion semantics ("modification beats deletion") fall out of the table
  instead of a second mechanism that must be kept correct.
- All sync state remains in git refs; still no state files.
- If S is lost (re-cloned repo, deleted `.jj`), abandoned changes can
  resurrect as zombies — annoying but loss-free, the accepted worst case.
  Tombstones can be added later as a pure namespace addition if this bites.
- The wire format is compatibility-critical: all machines must speak it, so
  changes require versioning thought.
