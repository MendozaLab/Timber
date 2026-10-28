# Agent operator guide

For people who drive Timber from an AI coding agent, and for the agents themselves. Timber was built for this use: it observes and preserves, and it refuses rather than guesses, so an agent can run it without being trusted to decide what is disposable. Everything below was learned on real repositories; see [field learnings](FIELD-LEARNINGS.md) for the receipts.

## 1. Start read-only, render everything

```sh
timber --json inspect --repo /path/to/repo --all-worktrees > inspect.json
timber report inspect.json --to report.html
```

- Render the report regardless of the inspect exit code. A `partial` result (a timeout, a failed worktree, a missing registration) still renders, and the status field says what was not scanned.
- Keep the JSON and the report outside the repository. They are large (they scale with untracked and ignored path counts) and they contain paths you may not want committed.
- Open the report from a loopback web server if your viewer refuses `file://` URLs.
- Read untracked counts as per-file counts. `git status --short` collapses directories; both numbers are correct, so say which one a summary quotes.

## 2. Classify every registered worktree into one row

| Row | Evidence from `inspect` | Action, always decided by a human per worktree |
|---|---|---|
| CLEAN_MERGED | no staged, working or untracked paths; branch tip reachable from the integration branch | prune candidate |
| DIRTY_UNIQUE | staged, working or untracked paths present | checkpoint, then verify |
| MISSING_FOLDER | registration whose folder does not exist | `git worktree prune --dry-run` first; a folder on an unmounted volume is absent, not gone |
| TMP_HAZARD | folder under a volatile path such as the system temporary directory | checkpoint first, then treat as DIRTY_UNIQUE |

Unknown stays unknown. Timber supplies the evidence for the first three columns and never performs the fourth: it has no move, delete, prune or apply command.

## 3. Preserve, in this order

1. Create the store yourself with `mkdir -m 700`, or let Timber create it; a plain `mkdir` gives mode 0755 and Timber refuses it, naming the remedy.
2. Put the store outside every source repository. A same-disk store preserves; it is not an independent backup, and `verify` says so (`independent_backup: unassessed`).
3. Key your own output files by checkpoint id or full path, never by directory basename: two worktrees of one repository share a basename.
4. Do not write anything into a worktree while it is being captured. That includes your own progress cards, logs and receipts. A capture whose source changed is refused with the changed path named; keep run records outside the repository or write them after the capture returns.
5. Handle nested repositories before capture. `inspect` lists them as a blocker. Disposable ones (test fixtures, scratch clones) can be skipped for one run with `checkpoint --exclude <path>`; the exclusion is recorded in the manifest and printed by `verify` and `restore`. Tracked paths cannot be excluded. Linked worktrees that an agent tool keeps inside the main checkout no longer need an exclusion: `inspect` lists them under `linked_worktrees_inside`, and `checkpoint` skips each one automatically and records it as `linked_worktree_captured_separately`. Checkpoint each linked worktree on its own. A `.git` file that is not provably this repository's own registration still counts as a nested repository.
6. Verify every checkpoint (`timber --store <store> --json verify <id>`) and keep the verification output with the id.

## 4. What Timber will refuse, and what to do

| Refusal | Meaning | Operator action |
|---|---|---|
| store directory must be private | the store is not mode 0700 or not yours | `chmod 700`, or a fresh path |
| nested repository refused | a `.git` below the worktree | exclude a disposable folder for the run, or handle that repository separately |
| --exclude refused for tracked path | the exclusion covers a file Git tracks | narrow the exclusion to the untracked or ignored folder |
| source changed | something wrote into the worktree during capture | stop writers, rerun |
| submodule unreadable | a gitlink whose path is missing, holds files without a `.git`, or whose HEAD cannot be read | initialize or repair that submodule, or handle it separately; readable submodules are recorded as pointers and their contents are never captured |
| Git operation `...` exceeded the N-second limit | one Git call hit the time limit, named in the message | result is partial and the source unchanged; rerun with a larger `--git-timeout <seconds>` (default 120) or on a smaller scope |
| Worktree directory is absent | registration with no folder | check for an unmounted volume before any prune |

A clean preview is a mechanical merge. It is never permission to discard the originals; that decision stays with a person, one worktree at a time.
