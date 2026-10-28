# Field learnings

Dated notes from running Timber on real repositories, kept so that what breaks or surprises in practice flows back into the tool. Each entry states what happened, what Timber did, and the smallest change that followed. Entries are genericized: no private paths, project names, filenames or report contents. The synthetic fixtures in `tests/` remain the regression record; this file records why they exist.

## 2026-10-28: field acceptance of the three changes

- **Submodules as pointers (acceptance passed).** The repository whose 31 worktrees were all refused the day before now checkpoints: one worktree captured and verified, with its vendored submodule recorded as `{gitlink_commit, checked_out_commit: null}` and nothing of its contents in the store.
- **Operator-settable timeout (acceptance passed).** The largest root, partial the day before at the fixed 120 s limit, completed a full scan of 7 of 8 registered worktrees in 734 s with `--git-timeout 600`; the eighth is a registration on an unmounted volume, reported as absent, not pruned.
- **Bounded output (acceptance passed).** The same 31-worktree repository that produced a 39 MB result on 2026-10-27 produced 1.8 MB under schema 2, and its 44 MB report became 2.2 MB, with identical staged, working and untracked counts. The largest root's result is 92 KB.
- **Nested repositories (acceptance passed, with a correction).** `inspect` listed 91 nested repositories in the main checkout before `checkpoint` was attempted; 75 were disposable fixtures and 16 were the repository's own linked worktrees, which led to the linked-worktree classification. The main checkout is still uncaptured because background daemons write into it; the convention file proposal exists so those exclusions stop being retyped.
- **Convention file and linked worktrees (acceptance passed).** The main checkout that four captures could not take on 2026-10-27 was captured on the first attempt with no command-line flags: a committed-style `timber.toml` supplied six exclusions (a fixture folder, daemon output folders and files, a build target), and the 16 linked worktrees inside the checkout were recorded as `linked_worktree_captured_separately` without any operator action. The checkpoint verified.
- **Untracked bytes deserve the same summary as ignored bytes.** That checkpoint is 20 GB across 12,288 entries, and most of it is one untracked folder of earlier recovery archives (compressed filesystem snapshots and Git bundles of 0.6 to 0.8 GB each). Inspect reports untracked paths in full but not their total size, so the operator learned the capture cost only after paying it. Proposed small change: give the untracked list the same `{count, bytes, top_dirs}` summary companion that ignored paths now have, so the size of a capture is visible before it starts.
- **Store reclamation (now `timber gc`, 0.1.1).** The 19 GB of incomplete objects was reclaimed by rebuilding the store: a fresh store, three re-captures verified, then the old store deleted. That is the manual form of what `timber gc` will do.

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

### Same day: thirteen more repositories in one pass

Setting: every registered repository of one developer's portfolio, 13 roots after de-duplicating checkouts that share a Git common directory, inspected read-only with `--all-worktrees` by a wrapper script. Totals: 139 registered worktrees, 102 deeply scanned, 76 dirty, 11 minutes of wall time, reports between 16 KB and 203 MB.

- **Submodules are the dominant blocker, by far.** One submodule in a worktree makes the whole worktree uncapturable, and a submodule is usually shared by every worktree of that repository. One repository had 31 of 31 worktrees refused by a single vendored submodule; three others lost 6, 5 and 2 worktrees the same way. Counting across the portfolio, 46 of the 76 dirty worktrees cannot be checkpointed today. The refusal is documented and correct; it is also the single largest gap between the tool and the repositories agents actually work in. See `docs/PRD-SUBMODULES.md`.
- **A 120 s Git timeout turned one scan partial.** The largest root (eight worktrees, hundreds of thousands of ignored paths) hit "Git operation exceeded 120 seconds; source was not changed". The source stayed untouched, the result said `partial`, and the rest of the run continued. The missing piece is an operator-settable timeout and a note in the result about which operation timed out. Folded into the submodules Product Requirements Document as a small second item.
- **An absent worktree can be an unmounted volume.** Three registrations pointed at an external disk that was not mounted. Timber reported "Worktree directory is absent" and did not call it a prune candidate; that is exactly right. For operators: never prune a registration whose path starts with a volume mount point until the volume has been checked.
- **A broken gitdir pointer is reported, not guessed.** One worktree's `.git` file pointed into a submodule's `modules` directory that no longer existed; Timber returned the Git error verbatim and marked the worktree `failed`.
- **Partial results still render.** The wrapper skipped `report` whenever `inspect` exited non-zero, which hid the four most interesting repositories until the reports were rendered by hand. `report` handles a `partial` result; operators should render regardless of exit code and read the status field.
- **Scale data points** (same build, one machine, Apple silicon): 42 worktrees in 31 s; 31 worktrees in 74 s; 31 worktrees with a shared submodule in 248 s (all refused after full index scans); one worktree with 122,935 untracked files produced a 203 MB report. The JSON and HTML sizes track the untracked and ignored path counts, which is the bounded-output proposal's motivation.

### Same day: the nested-repository blocker in the field

With `inspect` now listing nested repositories, the main checkout of the large repository showed 91, not the 75 fixture repositories counted by hand. The other 16 were the repository's own linked worktrees: agent tools keep their worktrees inside the main checkout (under ignored folders such as `.claude/worktrees/`, `.kilo/worktrees/` and `.worktrees/`), and each carries a `.git` file, so to a nested-repository walk they look like foreign repositories. The first `checkpoint --exclude` of the fixture folder was therefore refused on the first sibling worktree. Excluding those folders as well is correct for one run, since each sibling worktree is a registration that gets its own checkpoint, but the tool could tell the two cases apart: a nested `.git` whose gitdir points into this repository's own `worktrees/` metadata is a registered linked worktree, not a foreign repository. Recorded as a follow-up in `docs/PRD-NESTED-REPOSITORIES.md`.

- **A concurrent write during capture is refused, and the writer was the operator's own orchestrator.** The second attempt at the main checkout ran for two minutes and was refused with "source changed", naming a status file inside the repository that the agent orchestrating the run had edited meanwhile to record progress. The refusal is right: a capture whose source moved under it is not a checkpoint. The lesson for agent operators is a sequencing rule, not a tool change: while a worktree is being captured, nothing in that worktree may be written, including the orchestrator's own cards, logs and receipts. Keep run records outside the repository being captured, or write them after the capture returns. The aborted capture left the store 86 MB larger; see `docs/SAFETY.md` on interrupted captures for how incomplete material is treated.
- **Four refusals, 19 GB, no way to reclaim it.** Each refused capture of the main checkout kept the objects it had already written (a correct choice: an interrupted capture must not be mistaken for a checkpoint), and after four refusals the store held about 19 GB of incomplete material beside 385 MB of verified checkpoints, with no command to tell them apart or remove the former. On a disk at 99 percent that turned a safety property into a cost. The refusals themselves were all right; the gap is a reclamation command with a dry run. See `docs/PRD-RECLAMATION.md`. Not a job for an external index or a hook: the store is the only thing that knows which objects a refused run contributed.
- **Background daemons are concurrent writers.** The fourth refusal came from launchd jobs that write status files and snapshots inside the checkout every few minutes. A repository that hosts its own monitoring output can only be captured with those output paths excluded for the run, or with the writers paused. The inspect summary made this visible after the fact: one ignored folder of daemon snapshots held 12 GB and 47,875 files inside the checkout.

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
