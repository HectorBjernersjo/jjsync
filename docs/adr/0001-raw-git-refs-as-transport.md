# 0001 — Raw git refs as transport, not jj bookmark push

## Status

Accepted (2026-08-14). Amended (2026-08-15): the last-synced marker is a plain
git ref, not a jj bookmark — see Addendum.

## Context

jjsync must sync undescribed working-copy commits (safety invariant: never
require descriptions solely for sync). Verified on jj 0.41: `jj git push`
refuses commits with no description ("Won't push commit … since it has no
description"), with no flag to bypass. Pushing a `__jj_sync/<ws>` bookmark
through jj — the original spec's mechanism — is therefore impossible without
auto-describing commits, which would violate the invariant and pollute the
user's history.

Also verified on jj 0.41:

- `git push <remote> <sha>:refs/jj-sync/<ws>` works; non-branch namespaces are
  invisible in GitHub's branch UI.
- `git push --force-with-lease=<ref>:<expected-sha>` works on custom
  namespaces, giving an atomic compare-and-swap publish.
- jj does not index commits fetched into arbitrary ref namespaces, but pointing
  a local git branch (`refs/heads/__jj_sync/<ws>`) at the fetched commit and
  running `jj git import` makes it visible — and jj 0.41 writes change-id
  headers into git commits, so the change-id survives transport.
- A conflicted jj merge commit survives push/fetch with the conflict intact.

## Decision

Transport sync state as raw git refs, bypassing jj's push entirely:

- Publish: `git push <remote> --force-with-lease=<ref>:<last-synced-sha>
  <sha>:<refPrefix><workspace>` — CAS, never unconditional force.
- `refPrefix` is per-repo config, default `refs/jj-sync/`. Work setups can use
  a branch namespace like `refs/heads/users/hector/jjsync/` (visible as
  branches there, by choice).
- Receive: fetch the namespace, point local branch `refs/heads/__jj_sync/<ws>`
  at the fetched commit, `jj git import`.
- The local `__jj_sync/<ws>` bookmark position doubles as the last-synced
  marker: it moves only after a successful publish or adopt. All sync state
  lives in git refs; there are no state files.

## Consequences

- Requires colocated repos (git commands run directly in the working copy).
- Depends on change-id headers surviving transport (verified 0.41; a jj
  upgrade changing this must be caught by tests).
- jjsync owns the `__jj_sync/*` local bookmark namespace; user bookmarks are
  never touched.
- Crash-safe by construction: every step is idempotent and re-runnable.

## Addendum (2026-08-15)

Implementation falsified one detail: a local jj bookmark cannot be the
last-synced marker, because jj **moves local bookmarks along when the commit
they point at is rewritten** (verified on jj 0.41). After a snapshot amends
`@`, a `__jj_sync/<ws>` bookmark would silently follow, making S ≡ local and
"moved here" undetectable — the compare would misread every local edit as
"remote moved" and adopt backwards.

S therefore lives in plain local git refs outside `refs/heads/`, which jj
never touches: `refs/jj-sync/s/<item>`, mirroring the wire shape exactly
(the fetched remote state is mirrored the same way under `refs/jj-sync/r/`).
This also unifies M1 with ADR 0003's M2 mirror — one mechanism for the whole
set. Receiving still imports via a short-lived local branch
(`__jjsync-tmp-*`), deleted through `jj bookmark delete` so the imported
commit stays visible. Everything else in this ADR stands.
