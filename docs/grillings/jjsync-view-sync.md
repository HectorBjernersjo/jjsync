# Grilling: jjsync full view sync — Milestone 2 (2026-08-14)

Trigger: "I really don't want to just sync the cwd but the whole repo" — anonymous
changes not on any branch, bookmarks, everything.

# Questions and Answers

- **Build order?** Two milestones. M1 = workspace-@ sync (the MVP grilling), usable alone. M2 = view sync layered on the same architecture; wire format designed now for both, so M2 is addition, not rebuild.
- **What is "the whole repo"?** jj's **view**: all visible anonymous heads + all local bookmark positions + workspace @s. Explicitly per-machine (not synced): op log (`jj undo` is local — shell-history semantics), evolog/predecessors, working-copy tracking, local config. Everything structural (graph, descriptions, conflicts, change-ids) already travels with the commits.
- **Which bookmarks?** All local bookmarks by default, including `main`, with per-repo exclude patterns. jjsync auto-moves local bookmarks on adopt (rebase `main` on the laptop → desktop's `main` follows). Never pushed to the project's normal branch namespace. Remote-tracking bookmarks (`main@origin`) not synced — they come from origin.
- **Deletion vs modification?** Modification wins. An abandon only applies to state untouched since; a change amended elsewhere **resurrects** in its new version. Bookmark deleted on one machine + moved on the other = real conflict → freeze + ⚠. Rationale: worst case of "modification wins" is an annoying zombie; worst case of "deletion wins" is silently lost work (invariant 3).
- **Wire format?** `refs/jj-sync/<ws>` (workspace @, M1) · `refs/jj-sync/heads/<sha>` (set semantics; divergent versions of one change coexist as two refs — no ref-level conflict) · `refs/jj-sync/bookmarks/<name>`. Atomic batch pushes (`git push --atomic`), CAS per ref incl. deletions (`--force-with-lease` against expected sha). Leak gate covers everything published. **No tombstones** (see Research).
- **Frozen bookmark resolution?** Explicit `jjsync resolve <bookmark>` = "my current local position wins". Implicit resolution (next local move counts as the decision) rejected — a routine move could silently overwrite the other machine's choice.
- **Status in M2?** One line per problem (frozen bookmarks with both positions, blocked leak gate, persistent push failures), otherwise just `✓ synced`. No listings of healthy heads.
- **M1→M2 migration?** None needed — M2 adds namespaces next to M1's refs.

# Research

## The "deleted there vs created here" problem

Tombstones were initially proposed, then dropped: the actual disambiguator is
**S — a local mirror of the ref set as it stood after this machine's last
successful sync** (generalizing M1's `__jj_sync` bookmark; still no state
files, S lives in local git refs). With three sets (local, S, remote):

| Situation | Conclusion | Action |
|---|---|---|
| local only, not in S, not remote | created here | publish |
| remote only, not in S, not local | created there | adopt |
| in S, gone locally, still remote | I deleted it | delete remote ref |
| in S, local untouched, gone remotely | deleted there | abandon locally |
| in S, local modified, gone remotely | deletion meets edit | edit wins → resurrect |
| in S, both moved differently | divergence | heads: coexist · bookmarks: freeze · @: merge |

Guards: a head is "abandoned locally" only if no visible commit shares its
change-id (amend = ref moves) and it isn't an ancestor of a current head
(built-upon = ref superseded). The same check runs before abandoning on adopt,
so nothing the new remote set builds on is ever abandoned.

Tombstones only protect against **S loss** (re-cloned repo, nuked `.jj`), where
old abandoned changes can resurrect as zombies — annoying but loss-free,
i.e. exactly the accepted worst case. Not worth a permanent namespace; can be
added later as a pure addition if it ever bites.

## Verified facts (jj 0.41)

- Anonymous commit made visible on a receiving machine via temp branch +
  `jj git import` + bookmark delete stays a visible anonymous head.
- jj shows two commits with one change-id as a divergent change — the natural
  representation for a head edited on two machines; left unmerged for non-@ heads.
- jj has conflicted-bookmark support internally but no CLI to create one →
  freeze + explicit resolve instead.
