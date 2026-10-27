# Product requirements: capture around submodules, and an operator-settable Git timeout

Status: proposed 2026-10-27, not implemented. Source: `docs/FIELD-LEARNINGS.md`, "thirteen more repositories" entry.

## Problem

A worktree whose index contains a gitlink (mode 160000, a submodule) is refused by `checkpoint`, and `SUPPORTED-STATES.md` documents the refusal. In a portfolio of 13 repositories, 46 of 76 dirty worktrees were uncapturable for this reason alone, because one vendored submodule is shared by every worktree of its repository. The work that needs preserving is almost never inside the submodule; it is the staged, working and untracked paths beside it.

Separately, a fixed 120 s limit on a single Git operation turned the largest repository's scan `partial`. The limit is a sound default; the operator could not raise it for a known-large repository.

## Requirements

1. **Record the gitlink, skip its contents.** `checkpoint` captures the worktree with each submodule path recorded as a manifest entry of kind `submodule`, carrying the gitlink commit id from the index and, if the submodule's working directory is initialized, the commit id its HEAD points at. The submodule's files are not captured. The refusal for an uninitialized or unreadable gitlink is unchanged: refuse, naming the path.
2. **Restore reproduces the pointer, not the tree.** `restore` recreates the submodule path as an empty directory and writes the recorded commit ids into the restore report; it does not fetch or initialize submodules. `verify` prints each recorded submodule with both ids and states that its contents were not captured.
3. **Comparison treats the gitlink as a scalar.** Same-path comparison of a submodule entry compares the two commit ids; a difference is "different at the same path", never a content diff.
4. **Inspect names submodules without refusing.** `inspect` keeps reporting submodules, but as `submodules_recorded_not_captured` with the two ids, not as a checkpoint blocker, once item 1 exists.
5. **Operator-settable timeout.** A global `--git-timeout <seconds>` (default 120) applies to each single Git operation. A timeout still leaves the source untouched and the result `partial`, and the blocker names the operation and the limit that applied.
6. **Regression tests.** Synthetic fixtures with a real submodule (two tempfile repositories, one added as a submodule of the other): capture records the gitlink ids and omits the submodule's files; restore recreates the empty path and reports the ids; verify prints them; comparison of two checkpoints with different gitlink ids reports a competing version; an uninitialized gitlink is still refused; `--git-timeout 1` against a deliberately slow Git operation yields `partial` with the operation named.
7. **Documentation.** `SUPPORTED-STATES.md` replaces the submodule refusal row with the recorded-pointer behavior and keeps the nested-repository row; the user guide gains the timeout flag and a sentence that submodule contents are never captured.

## Out of scope

- Capturing submodule contents, recursive submodules, or running any `git submodule` command.
- Changing the default timeout.

## Acceptance

The three gates pass; on the field case, the repository with 31 worktrees sharing one submodule produces checkpoints for its dirty worktrees with the submodule recorded as a pointer, and the largest repository completes a full scan under an explicit `--git-timeout 600`.
