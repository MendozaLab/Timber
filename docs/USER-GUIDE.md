# User guide

Timber helps you review and consolidate work without changing the original worktrees. Start with inspection; build previews only from checkpoints you have verified.

## Storage and output

`--store PATH` selects the local checkpoint store. Use a location outside every captured source and preview. A same-disk store does not protect against disk loss. New store directories use owner-only permissions; an existing custom store must be owned by the current user with mode `0700`.

`--json` emits a structured result rather than the terminal view. Treat saved JSON and HTML reports as private until reviewed: they can contain source paths and filenames.

Restoration and preview destinations must be new directories. Timber refuses existing destinations and paths nested inside captured sources.

## 1. Inspect the relevant project

```sh
timber inspect --repo ./project --folder ./saved-work --folder ./old-agent-copy
```

Inspection checks the selected repository and its registered worktrees. It does not crawl your entire computer. Supply preservation folders explicitly.

Keep these classes separate:

- An active registered worktree has inspectable current Git and filesystem state.
- A stale registration refers to a worktree location that is absent or unavailable; it is diagnosis, not permission to prune.
- A preservation folder with a receipt contains a producer's claims to check.
- A folder without a manifest contains available material whose historical completeness is unknown.

## 2. Preserve selected state

```sh
timber checkpoint ./agent-navigation --include-ignored assets/diagram.png
```

A worktree checkpoint preserves committed history, the logical staged index, current tracked files, untracked files, and selected ignored paths. Ignored omissions are listed explicitly.

`--include-ignored` accepts source-relative paths or directory prefixes. Review what you include: ignored material can be valuable source data, credentials, or large caches. Timber does not send it to a provider.

Pause your writers when possible. `--writers-paused` records your declaration; it does not enforce a lock against editors or agents. Matching capture scans establish observed stability, not proof that no concurrent write occurred.

```sh
timber import ./saved-work --map ./folder-map.json
```

A folder import captures the supported files present now. It does not synthesize the former repository's commit, index, or complete history. See [the mapping example](../examples/import-map.json) for a public schema that declares source groups, selected destination paths, and Git dependencies. Dependencies remain declarations to assess; a receipt's success flag does not establish recovery.

## 3. Verify and restore

```sh
timber checkpoints list
timber checkpoints show CHECKPOINT_ID
timber verify CHECKPOINT_ID
timber restore CHECKPOINT_ID --to ./restored-copy
```

Verification reconstructs into an isolated temporary destination and compares it. Restore reconstructs into a new destination you choose. Neither operation executes hooks, filters, builds, dependency installation, or project code.

A successful result covers the supported state in that checkpoint. Excluded files, historical completeness, independent backup, and application correctness stay separate.

## 4. Choose a target and sources

Checkpoint the target too, including any unfinished target changes. Then prepare an ordered plan:

```sh
timber plan --target TARGET_ID --source FIRST_SOURCE_ID \
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
timber preview plan.json --to ./review-preview
```

The preview has its own object store and generated Git configuration. It does not register another source worktree, borrow source object alternates, modify source branches, or fetch missing data.

On a clean sequence, review the resulting repository and source attribution. On a conflict, Timber stops at the first conflicting source and retains the partial preview and conflict evidence. Remaining sources are marked pending. Do not mistake a partial preview for successful consolidation.

Timber does not run the preview's project tests. Run your own review and tests deliberately after inspecting the result.

## 6. Recheck before a human cleanup decision

```sh
timber --json assess plan.json > assessment.json
timber report assessment.json --to report.html
```

Assessment rechecks original sources for drift and explains exclusions, retained alternatives, missing material, and other unresolved conditions. A reproducible old preview can remain useful while its cleanup assessment becomes stale.

Timber has no command that applies the preview to the target or removes the originals. Keep preservation, integration, testing, human approval, and any later cleanup as separate actions.
