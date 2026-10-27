# Field learnings

Dated notes from running Timber on real repositories, kept so that what breaks or surprises in practice flows back into the tool. Each entry states what happened, what Timber did, and the smallest change that followed. Entries are genericized: no private paths, project names, filenames or report contents. The synthetic fixtures in `tests/` remain the regression record; this file records why they exist.

## 2026-10-27: first run on a large multi-worktree repository

Setting: a 57 GB research monorepo with 31 registered worktrees spread across the main checkout, several agent-tool worktree directories, and dated folders elsewhere on the disk. Two further registrations pointed at folders under the system temporary directory that no longer existed. The run was driven by an AI coding agent under human approval, which is the use Timber was built for.

### What worked as designed

- **Read-only inspect across all worktrees** (`--all-worktrees`) scanned 31 worktrees in 74 s with zero failed or partial scans and zero blockers; progress lines on standard error made the run observable while it ran.
- **The store privacy check** refused a store directory that a shell `mkdir -p` had created with mode 0755. The refusal was correct. The follow-up: the error now names the remedy (`chmod 700`, or let Timber create the path), with a regression in `tests/checkpoints.rs`.
- **Registrations with missing folders** were reported, not pruned. `git worktree prune --dry-run` confirmed the same entries before a human-approved metadata prune; branch references survived.

### What surprised the operator

- **Untracked counts are per file.** `git status --short` collapses an untracked directory into one line, so a worktree that `git status` showed as 24 entries carried 2,883 untracked files under Timber. Both numbers are right; reports must say which count they quote. Documented in the user guide.
- **Output scales with the ignored list.** One worktree listed 73,925 ignored paths from metadata. The `--json` result for the repository was 39 MB and the rendered HTML report 44 MB. The report rendered fine in an embedded browser, but automation that feeds the JSON to another program pays for every ignored path even when it only needs counts. See the bounded-output Product Requirements Document (`docs/PRD-BOUNDED-OUTPUT.md`).
- **Embedded browsers may refuse `file://` URLs.** The agent's in-app browser could not open the report from disk. Serving the report directory on a loopback ephemeral port (`python3 -m http.server --bind 127.0.0.1 0` or equivalent) worked without changing the report. Documented in the user guide.

### Checkpoint on the same repository, later the same day

- **Three worktrees checkpointed and verified** into one store (385 MB on disk): each `--json checkpoint` returned the checkpoint id with its verification, and a separate `--json verify` returned `verified: true`, `historical_completeness: captured_supported_state_only` and `independent_backup: unassessed`. Those last two fields are the honest ceiling: the store sat on the same disk as the sources.
- **The main checkout was refused: nested repository.** An artifact folder under the main checkout held 75 throwaway `.git` directories left by a test run a month earlier. Timber refused the whole-worktree capture, which is the documented behavior for nested repositories. The surprise was sequencing: `inspect` had reported zero blockers for that worktree, so the operator learned about the nested repositories only when `checkpoint` refused. The smallest change is for `inspect` to list nested repositories as a capture blocker before anyone runs `checkpoint`; a larger one is an explicit exclusion for paths the operator declares disposable. See `docs/PRD-NESTED-REPOSITORIES.md`.
- **Basename collisions in operator scripts.** Two worktrees of the same repository share the basename of the repository directory, so a wrapper that names its per-worktree output files by basename overwrote one result with another. Timber was not at fault; the lesson for agent operators is to key outputs by checkpoint id or full path, never by basename.

### Where the real worktree work was hiding

Of 31 worktrees, 14 carried staged, working or untracked paths. The largest uncommitted payloads sat in worktrees nobody remembered: a detached-HEAD worktree holding 1,400 untracked recovered files, and a feature worktree holding 2,883 untracked assets. The main checkout mixed edits from several concurrent agents, which is why a one-line fix in one file could not be committed without carrying another lane's 304 lines. Inspection made this visible; no result authorized cleanup.

### Pattern that emerged: a four-row triage

The operator classified each registered worktree into one of four rows and gated every action on a human decision per worktree:

| Row | Evidence | Human-gated action |
|---|---|---|
| CLEAN_MERGED | no staged, working or untracked paths; branch tip reachable from the integration branch | prune candidate |
| DIRTY_UNIQUE | staged, working or untracked paths present | checkpoint, then verify |
| MISSING_FOLDER | registration whose folder no longer exists | `git worktree prune --dry-run`, then metadata prune |
| TMP_HAZARD | folder under a volatile path such as the system temporary directory | checkpoint first, then treat as DIRTY_UNIQUE |

Timber supplies the evidence for the first three columns; it does not perform the fourth. This may become a documented workflow for agent operators.
