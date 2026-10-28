# Product requirements: nested repositories surfaced before capture

Status: implemented 2026-10-27 (nine synthetic regressions in `tests/nested_repositories.rs`); field acceptance pending. Source: `docs/FIELD-LEARNINGS.md`, 2026-10-27 checkpoint entry.

## Problem

`checkpoint` refuses a worktree that contains a nested repository, and `SUPPORTED-STATES.md` documents that refusal. In the field the refusal arrived late: `inspect` had reported the same worktree with zero blockers, so the operator planned a capture that could not run. The nested repositories were 75 disposable `.git` directories left under an artifact folder by an earlier test run; nothing about them was worth preserving, yet they blocked preservation of 20 modified and 2,642 untracked files around them.

## Requirements

1. **Inspect names the blocker.** `inspect` lists every nested repository found below the worktree (path, whether the path is tracked, untracked or ignored, and a count) under that worktree's `blockers`, with the message "checkpoint will refuse this worktree until nested repositories are handled". The JSON and the HTML report both carry it.
2. **Explicit exclusion for declared-disposable paths.** `checkpoint --exclude <path>` (repeatable, relative to the worktree) skips the named paths and records each exclusion in the manifest as `excluded_by_operator`, so a restore states what was left out. An excluded path that contains a nested repository no longer triggers the refusal. Exclusion never applies to tracked paths with staged or working changes; Timber refuses such an exclusion with the path named.
3. **No silent widening.** Without `--exclude`, behavior is unchanged: nested repositories refuse capture. Exclusions are per invocation, never remembered.
4. **Verification reports exclusions.** `verify` and `restore` print the excluded paths from the manifest so a reviewer cannot mistake a partial capture for a complete one.
5. **Regression tests.** Synthetic fixtures: a worktree with a nested repository under an untracked folder is refused without `--exclude`, captured with `--exclude` on that folder, refused when `--exclude` names a tracked modified path, and `inspect` reports the nested repository as a blocker in both cases.

## Out of scope

- Capturing nested repositories themselves; they remain separate repositories to handle on their own.
- Any heuristic that decides a nested repository is disposable. The operator declares it, per run.

## Acceptance

The three gates pass; on the field case, `inspect` shows the 75 nested repositories as a blocker, and `checkpoint --exclude <artifact folder>` captures the remaining staged, working and untracked paths with the exclusion recorded in the manifest.

## Implementation note (2026-10-27)

Requirement 2 was implemented more strictly than written: `--exclude` refuses every tracked path, clean ones included, not only tracked paths with staged or working changes. Reason: a clean tracked file absent from the working-layer capture would read as a deletion in a consolidation preview and would trigger the staged-versus-working review blocker. Allowing clean tracked exclusions would require preview and planning to substitute the index blob for operator-excluded tracked paths; that is a separate change if ever wanted. No schema change: operator exclusions use the existing `excluded` list with reason `excluded_by_operator`; the only new field is `Verification.excluded_by_operator`, which defaults to empty when absent.

## Follow-up (2026-10-27, field): registered linked worktrees inside a worktree

Agent tools place a repository's own linked worktrees inside its main checkout under ignored folders. Each has a `.git` file whose gitdir points at `<common dir>/worktrees/<name>`. The nested-repository walk counts them as foreign repositories, and `checkpoint` refuses on them unless the operator excludes each folder. Proposed: when a nested `.git` resolves into this repository's own common directory, classify it as `linked_worktree` with its registration name, report it separately from foreign nested repositories, and let `checkpoint` skip it automatically with a manifest record of reason `linked_worktree_captured_separately`; foreign nested repositories keep today's refusal. Regression: a fixture with one linked worktree under an ignored folder and one foreign nested repository; inspect classifies both; checkpoint succeeds with the linked worktree skipped and still refuses the foreign one without `--exclude`.

Status: implemented 2026-10-27 (seven synthetic regressions added to `tests/nested_repositories.rs`); field acceptance pending.

Implementation note: classification is stricter than "resolves into the common directory". A `.git` file is a linked worktree only when it is a regular file of at most 4096 bytes with exactly one `gitdir: <path>` line, the path reaches `<canonical common dir>/worktrees/<name>` with its lexical and canonical forms equal (no symbolic link anywhere, inside or outside the repository), and the registration's own `gitdir` file names this same `.git` file back. The back-pointer check is what separates a registered worktree from a copied folder that carries a valid-looking pointer. Everything else is foreign and keeps the refusal; a `gitdir:` line with trailing whitespace, which Git itself would accept, is foreign here by design. Two further refusals were added: a linked worktree whose folder holds a tracked path of the outer worktree stays a blocker and is refused by name (skipping it would read as deleting the tracked file), and an `--exclude` path strictly inside a linked worktree is refused because that folder is skipped as a whole. `--exclude` naming a linked worktree folder exactly is accepted and recorded as `excluded_by_operator`. Schema: no manifest schema change (a new `Exclusion::reason` value); `Verification` gains `linked_worktrees_captured_separately`, omitted when empty; inspect results gain the per-worktree `linked_worktrees_inside` list without an inspect-schema bump, because the change only adds a field.
