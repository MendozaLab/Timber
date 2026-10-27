# Changelog

Entries describe behavior a user can observe. Dates are the day the change reached `main`.

## Unreleased

- `checkpoint --exclude <path>` (repeatable, relative to the worktree, per run only) skips a disposable untracked or ignored path and records it in the manifest as `excluded_by_operator`; `verify` and `restore` list the exclusions. Refused, naming the path, for tracked paths, overlapping or unmatched paths, and paths that overlap an `--include-ignored` selection. (2026-10-27)
- `inspect` lists nested repositories below a worktree as a blocker with their tracking state and the message "checkpoint will refuse this worktree until nested repositories are handled". (2026-10-27)
- The store privacy refusal names the remedy: `chmod 700`, or let Timber create the store path. (2026-10-27)
- Documentation: field learnings, three proposals (bounded output, nested repositories, submodules), an agent operator guide, notes on per-file untracked counts, viewers that refuse file URLs, and rendering partial results. (2026-10-27)

## 0.1.0 (2026-10-26)

- First public version: read-only `inspect`, `checkpoint` with staged and working layers captured separately, `verify` by independent restore, `plan`, `preview` in a separate repository, `assess` for source drift, `report` to self-contained HTML, `import` and `restore-git` for folders that depend on another repository's Git data.
