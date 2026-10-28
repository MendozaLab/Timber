# User guide

Timber helps you review and consolidate work without changing the original worktrees. Start with inspection; build previews only from checkpoints you have verified.

## Storage and output

`--store PATH` selects the local checkpoint store. Use a location outside every captured source and preview. A same-disk store does not protect against disk loss. New store directories use owner-only permissions; an existing custom store must be owned by the current user with mode `0700`. If you create the directory yourself, use `mkdir -m 700`; a plain `mkdir` usually produces mode `0755`, which Timber refuses with the remedy in the message.

The preservation and preview examples below consistently use `--store ./timber-store`. Choose your own location and use it throughout the workflow.

`--json` emits a structured result rather than the terminal view. Treat saved JSON and HTML reports as private until reviewed: they can contain source paths and filenames.

Restoration and preview destinations must be new directories. Timber refuses existing destinations and paths nested inside captured sources.

`--git-timeout SECONDS` sets the limit for each single Git operation (default 120, from 1 to 86400) for one run. Raise it for a known-large repository, for example `timber --git-timeout 600 inspect --repo ./project`. A timeout never changes the source: inspection reports the worktree as partial and checkpoint refuses, and the message names the Git operation and the limit that applied.

## 1. Inspect the relevant project

```sh
timber inspect --repo ./project --folder ./saved-work --folder ./old-agent-copy
```

Inspection lists registrations for the selected repository, but deeply checks only the requested worktree by default. Other existing worktrees are marked `not_scanned`. This focused default replaces the original automatic scan of every worktree. It does not crawl your entire computer. Supply preservation folders explicitly.

```sh
# Add one registered source to the deep scan.
timber inspect --repo ./project --worktree ./agent-navigation

# Explicitly inspect every registered worktree.
timber inspect --repo ./project --all-worktrees

# Compare supplied saved folders with the current target, including receipt-declared Git versions.
timber --json inspect --repo ./project --folder ./saved-work \
  --compare-folders --out inspection.json
```

Untracked counts are per file: `git status --short` collapses an untracked directory into one line, so Timber's untracked number is usually larger. Both are correct; state which one a report quotes. Progress goes to standard error; structured output stays on standard output. Scope, elapsed time, partial results and skipped worktrees are recorded. Indexed Git content is read through a streaming batch instead of launching a Git process for each tracked file. Ignored files remain metadata-only; `--compare-folders` explicitly enables reads of supplied recovery-folder payloads and their proposed current target paths.

Ignored files are summarized by default: the count, the total recorded bytes, and the 20 top-level paths that contribute the most ignored entries. Add `--list-ignored` when you need every ignored path, for example to choose what to pass to `checkpoint --include-ignored`. Staged, working and untracked lists are always complete, because they are the recovery evidence. `report` shows the summary as a small table and shows the full list only when the saved JSON carries one.

```sh
timber --json inspect --repo ./project --list-ignored --out inspection-full.json
```

Inspect results now carry result `schema` 2 (it was 1). Compatibility: tools that read `inspect --json` should detect the `ignored` shape by that schema number, and in schema 2 by `scope.list_ignored`; schema 1 results always carried the full list. Checkpoint manifests and other stored records keep their own schema and are unchanged.

Same-path comparisons require equal content hashes, byte lengths, file types and executable flags. Different versions need review. Absent means absent at the proposed target path, not absent everywhere. Saved-copy duplicate groups require the same proposed target and exact fingerprints. They can overlap the target-match count, so do not add them as independent cleanup candidates.

Keep these classes separate:

- An active registered worktree has inspectable current Git and filesystem state.
- A stale registration refers to a worktree location that is absent or unavailable; it is diagnosis, not permission to prune.
- A preservation folder with a receipt contains a producer's claims to check.
- A folder without a manifest contains available material whose historical completeness is unknown.

## 2. Preserve selected state

```sh
timber --store ./timber-store checkpoint ./agent-navigation --include-ignored assets/diagram.png
```

A worktree checkpoint preserves committed history, the logical staged index, current tracked files, untracked files, and selected ignored paths. Ignored omissions are listed explicitly.

`--include-ignored` accepts source-relative paths or directory prefixes. Review what you include: ignored material can be valuable source data, credentials, or large caches. Timber does not send it to a provider.

A nested repository below the worktree (a folder with its own `.git` directory or file) makes `checkpoint` refuse the whole worktree; `inspect` lists each one as a blocker first. Handle real nested repositories separately. When a nested repository sits inside a folder you have decided is disposable, name that folder for this run:

```sh
timber --store ./timber-store checkpoint ./agent-navigation --exclude test-output --exclude build/tmp
```

`--exclude` is repeatable and takes paths relative to the worktree. Timber skips each path and everything below it, and records it in the manifest as `excluded_by_operator`; `verify` and `restore` print the excluded paths, so a partial capture cannot pass for a complete one. Only untracked or ignored paths can be excluded: Timber refuses, naming the path, an exclusion that covers a tracked path (whether or not it has staged or working changes), matches nothing, passes through a symbolic link, or overlaps another exclusion or an `--include-ignored` selection. The decision is yours and lasts for one run; Timber never remembers an exclusion or guesses that something is disposable. Inspection takes no `--exclude` and keeps reporting the nested repository.

A linked worktree of the same repository that an agent tool keeps inside your checkout (created with `git worktree add`, so its `.git` file points at this repository's `.git/worktrees/<name>` and Git's registration points back at it) is not a foreign nested repository: `inspect` lists it under `linked_worktrees_inside` instead of the blocker, and `checkpoint` skips it without `--exclude` and records it as `linked_worktree_captured_separately`, which `verify` and `restore` print. Checkpoint each linked worktree on its own. A `.git` file Timber cannot prove is such a registration (unreadable, malformed, pointing elsewhere, or reached through a symbolic link) still refuses.

Submodule contents are never captured: a submodule is recorded as a pointer (its path, the gitlink commit id in the index, and the commit its checkout points at), `restore` recreates its path as an empty directory without fetching or initializing it, and plans compare the pointers by commit id only. An empty, uninitialized submodule directory records the index id alone. Timber refuses, naming the path, a submodule whose directory is missing, holds files without a `.git`, or whose HEAD cannot be read. Capture and restore each submodule repository on its own if its contents matter.

Pause your writers when possible. `--writers-paused` records your declaration; it does not enforce a lock against editors or agents. Matching capture scans establish observed stability, not proof that no concurrent write occurred.

```sh
timber --store ./timber-store import ./saved-work --map ./folder-map.json
```

A folder import captures the supported files present now. It does not synthesize the former repository's commit, index, or complete history. See [the mapping example](../examples/import-map.json) for a public schema that declares source groups, selected destination paths, and Git dependencies. Without an explicit dependency repository, dependencies remain unresolved declarations; a receipt's success flag does not establish recovery.

Inspection understands the canonical `import-map.json` and supported file-manifest or grouped receipt structures. It validates relative mappings and never follows absolute producer source/output paths. Supported receipts can produce a `derived_import_map` in the private inspection result. Review that map, then save it outside the originals before an import.

```sh
# Preserve declared Git dependencies from an explicitly selected repository.
timber --store ./timber-store import ./saved-work --map ./folder-map.json \
  --dependency-repo ./project
```

Dependency declarations pin a commit identity and may additionally pin its reference, source head, tracked paths and expected file fingerprints. A moved reference, missing object, unsupported topology or mismatched expected content blocks dependency capture. The source repository is read without changing its references, index or files. Captured dependencies have their own portable object closure, so recovery does not borrow objects from the original repository.

## 3. Verify and restore

```sh
timber --store ./timber-store checkpoints list
timber --store ./timber-store checkpoints show CHECKPOINT_ID
timber --store ./timber-store verify CHECKPOINT_ID
timber --store ./timber-store restore CHECKPOINT_ID --to ./restored-copy

# Reconstruct a captured saved Git group separately from the loose folder files.
timber --store ./timber-store restore-git CHECKPOINT_ID --group saved-agent --to ./restored-git-copy
```

Verification reconstructs into an isolated temporary destination and compares it. Restore reconstructs into a new destination you choose. Neither operation executes hooks, filters, builds, dependency installation, or project code.

A successful result covers the supported state in that checkpoint. Excluded files, historical completeness, independent backup, and application correctness stay separate.

`restore-git` requires the group to have been captured through `--dependency-repo`. Supported stash dependencies reconstruct their base, staged and working layers; a supported untracked parent is restored separately into that working copy. Ordinary pinned commits restore their recorded tree. Folder files and Git-backed state are separate recovery components, and neither proves that every file from a former checkout has been recovered.

## 4. Choose a target and sources

Checkpoint the target too, including any unfinished target changes. Then prepare an ordered plan:

```sh
timber --store ./timber-store plan --target TARGET_ID --source FIRST_SOURCE_ID \
  --source SECOND_SOURCE_ID --out plan.json
```

The plan records immutable checkpoint identities. Source order matters. Edit the generated plan to select one snapshot layer per source:

| Layer | Included state |
|---|---|
| `committed` | The source's original commit and its history. |
| `index` | The staged version. |
| `working` | Current tracked working files; default selection. |

`extra_paths` lists explicitly selected captured extra files, such as untracked files, for inclusion. Capturing a file and integrating it are separate choices.

When staged and working contents differ, selecting `working` does not incorporate the staged alternative. That alternative remains in the checkpoint and appears in the comparison.

Version one selects whole sources and one layer for each. It does not select individual commits or hunks, resolve semantic differences, or decide which experiment is better.

Folder imports need explicit source-to-target mappings for preview contributions. Missing target paths can accept additions. Different existing contents require manual review in version one, even when a mapping supplies `base_sha256`; automated overwrites are not implemented. Timber does not invent a merge base from a folder name.

## 5. Build the separate preview

```sh
timber --store ./timber-store preview plan.json --to ./review-preview
```

The preview has its own object store and generated Git configuration. It does not register another source worktree, borrow source object alternates, modify source branches, or fetch missing data.

On a clean sequence, review the resulting repository and source attribution. On a conflict, Timber stops at the first conflicting source and retains the partial preview and conflict evidence. Remaining sources are marked pending. Do not mistake a partial preview for successful consolidation.

Timber does not run the preview's project tests. Run your own review and tests deliberately after inspecting the result.

## 6. Recheck before a human cleanup decision

```sh
timber --store ./timber-store --json assess plan.json > assessment.json
timber --store ./timber-store report assessment.json --to report.html
```

Use the same checkpoint store when rendering a source-bound report.

Assessment rechecks original sources for drift and explains exclusions, retained alternatives, missing material, and other unresolved conditions. A reproducible old preview can remain useful while its cleanup assessment becomes stale.

Timber has no command that applies the preview to the target or removes the originals. Keep preservation, integration, testing, human approval, and any later cleanup as separate actions.
