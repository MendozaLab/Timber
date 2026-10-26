# Reproducible local demonstration

The demo exercises the real Timber executable against disposable synthetic repositories. It checks successful consolidation, refused consolidation, source drift, independent recovery, and corrupt-checkpoint detection.

## Run

From the Timber project root:

```sh
cargo build --locked
python3 examples/demo.py
```

The default output is `demo-output/`, which Git ignores. An existing output directory is refused rather than overwritten or deleted. For another run, choose a fresh directory:

```sh
python3 examples/demo.py --output demo-output/another-run
```

Use a local nonsynced output location. The script copies the built executable into the output directory so concurrent builds cannot mix executable versions between commands. It needs Python 3 and Git; it installs no packages, starts no service, and calls no model provider.

## What is checked

The script constructs a common repository, a dirty target, three linked worktrees, and an old preservation folder. All inputs are invented fixture data.

| Scenario | Assertion |
|---|---|
| Target has unfinished changes | Those changes survive in the preview. |
| Agent A has different staged and working versions | The working version integrates; an independent restore still recovers both versions. |
| Agent A has an untracked note and an ignored asset | The note enters only after explicit plan selection. The asset is explicitly captured but stays out of the preview. |
| Agent B has a unique commit and unfinished follow-up | Committed ancestry and selected working contents survive. |
| Old folder has a producer receipt and a file mapping | The mapped note is added; historical completeness remains unknown. |
| A later source conflicts | The preview exits with code 1, retains the last clean candidate, and reports remaining sources. |
| An original changes after capture | Assessment exits with code 1 and reports source drift. |
| Original fixture repositories become unavailable | Verification still succeeds and the preview reproduces the same Git tree from checkpoints. |
| An object in a copied store is deliberately corrupted | Verification exits with code 4 and reports a hash-verification failure. |

Before the intentional source-drift edit, the script compares source file bytes and modes, exact index bytes, HEAD and branch identities, shared references, repository configuration, and old-folder payload bytes. Timber must leave them unchanged.

The script itself later edits one synthetic source and relocates the synthetic fixture directory to make the originals unavailable. These are deliberate test setup operations, not Timber cleanup features.

## Retained evidence

Under `<demo-root>`:

- `results/`: saved command results plus exact standard output and standard error.
- `plan-selected.json`: the plan with the untracked note explicitly selected.
- `preview-complete/`: the independent consolidated repository.
- `preview-conflict/`: the last clean partial candidate and retained conflict evidence.
- `a-restored/`: independently reconstructed staged and working contents.
- `fixtures-offline/`: retained original fixture repositories after the availability test.
- `store/` and `store-corrupt/`: the valid store and deliberately corrupted copy.
- `complete-report.html`, `conflict-report.html`, `drift-report.html`: local review reports.
- `metrics.json`: measured command durations, storage bytes, executable digest, versions, and assertions.

Raw local evidence contains local paths and is not a public example bundle. Keep it ignored. Public documentation uses `<demo-root>` in place of operator paths.

## Recorded development sample

One local run passed on 2026-10-26 with `timber 0.1.0`, Git 2.51.1, and Darwin 25.5.0. The implementation, tests, locked dependencies, and automated-check configuration match the initial public source snapshot. The original development history is retained locally; it is not part of the public repository.

The copied executable had SHA-256 digest:

```text
ab598344a4d8cf5ba291f5c1f52b25df5a642a0ac85ad422b569d56bee3b99ec
```

| Measurement | Observed value |
|---|---:|
| Timber command invocations | 19 |
| Sum of measured command durations | 17.275430 seconds |
| Checkpoints, including folder import | 5 |
| Checkpoint store payload and metadata | 16,240 bytes |
| Target checkpoint, including automatic verification | 0.951279 seconds |
| Agent A checkpoint, including automatic verification | 0.914880 seconds |
| Explicit Agent A verification | 0.327333 seconds |
| Explicit Agent A restore | 0.326937 seconds |
| Complete preview | 1.784560 seconds |
| Conflicted preview, correctly refused | 2.126322 seconds |
| Offline reproduced preview | 2.467119 seconds |
| Corrupt-copy verification, correctly refused | 0.008396 seconds |

These are single-run observations for tiny synthetic fixtures. Command time excludes fixture setup and human review. Store size depends partly on path lengths and manifests. There is no comparison with manual consolidation, no application test, and no measured savings or independent-backup guarantee.

Rerun after implementation changes. Preserve any failure output and its condition; a later successful run does not erase the earlier result.
