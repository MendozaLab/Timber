# Product requirements: bounded output for large repositories

Status: proposed 2026-10-27, not implemented. Source: `docs/FIELD-LEARNINGS.md`, 2026-10-27 entry.

## Problem

`inspect --json` and `report` enumerate every ignored path a worktree lists from Git metadata. On a repository whose main worktree ignores 73,925 paths, one inspection produced a 39 MB JSON result and a 44 MB HTML report. The information an operator needs from ignored paths at inspection time is usually the count, the total bytes, and the handful of ignored paths they may want to capture explicitly; the full list is only needed when selecting ignored assets for a checkpoint.

Agents and scripts that consume the JSON pay the full cost on every run, and a report that large is slow to open and awkward to share for review.

## Requirements

1. **Counts by default, lists on request.** `inspect` keeps `ignored` as a summary object by default: `count`, `bytes`, and the top-level directories that contribute most entries (bounded, for example the 20 largest). A new `--list-ignored` flag restores today's full list. Staged, working and untracked lists stay complete, because they are the recovery evidence.
2. **Schema version bump with a compatibility note.** The result `schema` increments; `SUPPORTED-STATES.md` and the user guide state the change. Consumers of the old shape detect it by the schema number, never by guessing.
3. **Report follows the JSON.** `report` renders the summary object as a small table and renders the full list only when the JSON carries one.
4. **Nothing changes in capture.** `checkpoint` still lists ignored paths it is asked to include and still reports omissions; the capture contract in `SUPPORTED-STATES.md` is unchanged.
5. **Regression tests.** Synthetic fixtures with a few thousand ignored paths assert the default summary shape, the `--list-ignored` full shape, and byte-identical staged, working and untracked lists between the two modes.
6. **Performance record.** The matched benchmark in `docs/PERFORMANCE.md` gains a row for the summary mode on the same frozen fixture, stating what is outside the measurement.

## Out of scope

- Any change to what is captured, verified, restored or previewed.
- Heuristic trimming of untracked paths; those stay complete.
- Compression or external storage of reports.

## Acceptance

`cargo fmt --all --check`, `cargo clippy --all-targets --locked -- -D warnings` and `cargo test --locked` pass; the field case that produced a 39 MB JSON result produces a result under 2 MB in the default mode with the same staged, working and untracked lists.

## Implementation note (2026-10-27)

The inspect result `schema` moved to 2 through a separate `INSPECT_SCHEMA` constant; stored manifests, plans and receipts keep schema 1, and the command envelope is unchanged. Because a schema 2 result can carry either shape, `scope.list_ignored` records which one was requested. The summary is computed from the same full list that inspection already builds internally, so nothing inside Timber lost information: capture, planning and assess enumerate ignored paths themselves, and the nested-repository classification uses `git check-ignore`. The default summary equals the summary of the `--list-ignored` list (regression-tested). The field case under 2 MB is not measured here; see `docs/PERFORMANCE.md`.
