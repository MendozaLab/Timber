# Product requirements: capture around submodules, and an operator-settable Git timeout

Status: implemented 2026-10-27 (proposed the same day). Source: `docs/FIELD-LEARNINGS.md`, "thirteen more repositories" entry.

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

## Implementation note

- **Manifest.** `model::EntryKind` gains `Submodule`. A submodule entry keeps the existing `Entry` shape with an empty `sha256`, zero `mode` and zero `size`; its ids live in the new `GitState::submodules` list of `SubmoduleRecord { path, gitlink_commit, checked_out_commit }`, one record per submodule entry, validated as a pair on load. `GitState::index` keeps only regular files and symbolic links, so every existing reader of staged blobs is unchanged. The new list is `#[serde(default, skip_serializing_if = "Vec::is_empty")]`: manifests without submodules serialize to the same bytes and keep the same identity, and older manifests load unchanged. `model::SCHEMA` stays 1, because an older reader cannot misinterpret a new manifest: it refuses the unknown entry kind `submodule` instead of silently dropping the path. `Verification` gains `submodules_recorded_not_captured`, defaulted and omitted when empty.
- **Uninitialized submodules.** Item 1 calls for the HEAD id only when the submodule is initialized, and for a refusal of an uninitialized or unreadable gitlink. Both hold as implemented: an empty submodule directory, which is what Git leaves for a submodule that was never initialized, is recorded with the index id alone, while a gitlink whose path is missing, is not a real directory, holds files without a `.git`, or whose HEAD does not resolve inside that directory is refused, naming the path. The existing regression test for a gitlink with no directory stays green.
- **Comparison and preview.** Plans compare submodule pointers as scalars: a differing gitlink lands in `differing_paths` with a note that says "different at the same path" and gives both ids. The working layer of a submodule is the commit its checkout pointed at, the index layer is the gitlink. Before each merge the preview runs a three-way check of every gitlink against the merge base; a pointer moved on both sides to different commits stops the preview with a `submodule_gitlink` conflict record naming the path, so Git is never asked to merge submodule contents. A one-sided pointer change merges, and the preview shows the submodule path as an empty directory.
- **Inspect.** Readable submodules are listed per worktree under `submodules_recorded_not_captured` and are no longer blockers or unknown paths; the nested-repository blocker skips directories at gitlink paths and still names every other nested repository. An unreadable gitlink remains a blocker (`submodule unreadable: PATH: ...`) and leaves the scan partial. `INSPECT_SCHEMA` is unchanged; the field is additive.
- **Assessment.** `assess` compares the current index including gitlinks, rechecks each recorded submodule checkout, and always adds `submodule_contents_not_captured` when a checkpoint holds submodules.
- **Timeout.** `git::set_timeout_seconds` holds the limit for the process; the command line sets it from `--git-timeout` (1 to 86400, default `git::DEFAULT_TIMEOUT_SECONDS`, 120). Every single Git operation, including the streaming `cat-file --batch` readers, uses it. The message reads "Git operation `ls-files --others --exclude-standard -z` exceeded the 1-second limit (--git-timeout 1); source was not changed". The regression test puts a synthetic `git` wrapper first on `PATH` that sleeps before `ls-files --others`; a repository `core.fsmonitor` cannot slow Timber, because every Git call already forces it off.
- **Tests.** `tests/submodules.rs` builds two tempfile repositories with invented content, adds one as a submodule of the other, and covers capture, restore, verify output, plan comparison and preview conflicts, refusals of unreadable gitlinks, an uninitialized submodule, inspect output, the unchanged shape of manifests without submodules, and the timeout.
- **Not done here.** The acceptance runs on the field repositories (31 worktrees sharing one submodule, and the largest repository under `--git-timeout 600`) need the operator's own machine and are not part of this change.
