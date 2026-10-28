# Timber!

**Untangle dirty worktrees. Keep the useful work.**

Parallel AI coding agents and rapid local experiments multiply Git worktrees faster than people can audit them. Timber helps developers recover unfinished work scattered across dirty Git worktrees and saved folders, without spending model tokens on raw diff inspection or trusting an agent with destructive cleanup commands. Inspect what changed, preserve staged and working versions separately, and assemble selected changes in an independent consolidation preview.

Timber is for the moment when several worktrees contain useful changes and you can no longer confidently answer: *what is unique, what has already landed, and what can I safely put aside?*

[![Check](https://github.com/MendozaLab/Timber/actions/workflows/check.yml/badge.svg)](https://github.com/MendozaLab/Timber/actions/workflows/check.yml)

**Experimental 0.2.0.** Formatting, strict Clippy and the test suite run on macOS and Linux in GitHub Actions on every push; Windows is unsupported. Start with read-only inspection and review the [supported states](docs/SUPPORTED-STATES.md) before relying on a checkpoint.

The recovery workflow runs locally, without an AI provider, cloud account, or background service. A verified same-device checkpoint is not an independent backup.

## Why dirty worktrees pile up in agentic coding

Each parallel agent session or quick experiment usually gets its own worktree, and many finish with uncommitted changes. The cleanup that should follow tends to stall for four reasons:

- **Cleanup refuses dirty folders.** `git worktree remove` declines a worktree with modified or untracked files unless you add `--force`, which discards them. Without a way to see what each one holds, the safe choice is to leave it.
- **Build output multiplies.** Every stranded worktree keeps its own `target/`, `node_modules/` or `.venv/`, so disk use grows with the worktree count even when the source changes are small.
- **Agent cleanup is expensive and risky.** Asking a coding agent to triage means feeding it `git status` and `git diff` for every worktree, which floods its context, and then trusting it near commands such as `git worktree remove --force`, `git clean` or `git reset --hard`.
- **The truth is split across layers.** One change can exist as a commit, a staged version, a different working-file version and a copy in a saved folder. Similar filenames or patches do not show which version is current or unique.

Timber handles the inspection deterministically and locally. It calls no model, so the triage itself spends no tokens; an agent driving it reads one structured result, with large ignored-path lists summarized as counts, bytes and top folders, instead of raw diffs. Timber has no command that moves, deletes or prunes, so every decision to discard work stays with a person.

## Evidence from real repositories

Timber is run on the maintainers' own working repositories, and what each run teaches is written down in [field learnings](docs/FIELD-LEARNINGS.md) with the fix or proposal that followed. On 2026-10-27, one pass over 13 repositories with 139 registered worktrees scanned 102 of them read-only, wrote nothing into any source, and found uncommitted work in 76; the same day's refusals (a non-private store, nested repositories, an exclusion that covered a tracked file, a source that changed during capture, submodules, a timeout) each named the exact path. Operators driving Timber from an AI coding agent should start with the [agent operator guide](docs/AGENT-OPERATORS.md).

## What Timber does

- Inventories registered worktrees and explicitly supplied preservation folders.
- Deeply inspects the requested worktree by default; add `--all-worktrees` or select additional worktrees explicitly.
- Compares saved folder payloads and declared Git-backed versions against a chosen target when requested.
- Preserves staged contents separately from working files, including selected ignored assets.
- Restores checkpoints in isolation and compares the reconstructed state.
- Compares frozen sources with a chosen target, using exact ancestry and content evidence.
- Builds an ordered consolidation preview in its own repository, stopping on conflicts.
- Explains exclusions, retained alternatives, source drift, and cleanup blockers in the terminal or a local HTML report.

Timber has no move, delete, prune, or apply-to-source command. A clean preview establishes a mechanical merge, not application correctness or permission to discard the originals.

## Install from source

Prerequisites: macOS or Linux, Rust 1.85 or newer, and Git with `merge-tree --write-tree` support (Git 2.38 or newer).

```sh
git clone https://github.com/MendozaLab/Timber.git
cd Timber
cargo install --path . --locked
timber --version
```

The Cargo package is `timber-worktrees`; the executable is `timber`. This guide uses installation from source.

## Start with read-only inspection

Replace the example paths with your own. These commands inspect existing work without creating a checkpoint or modifying the sources.

```sh
timber inspect --repo /path/to/project

timber inspect --repo /path/to/project \
  --folder /path/to/saved-work --compare-folders
```

With `--compare-folders`, read the findings as follows:

| Finding | What it means |
|---|---|
| Identical at the same path | Matching content and recorded file properties; no competing version to merge at that path. |
| Different at the same path | A competing version to retain and review; neither version is automatically better. |
| Absent at the proposed path | A candidate addition; equivalent work may exist elsewhere. |
| Unresolved Git dependency | Saved tracked work still depends on Git data that needs explicit verification and capture. |

An exact match never authorizes deletion. Timber compares supported file content, including code, documents, and media; it does not make semantic or editorial selections.

## Preserve, verify, and preview

In this example, `project/` and `agent-navigation/` are related worktrees. Run from their parent directory. Choose a store outside every source repository, preferably on another physical device if you need independent backup. The preview destination must also be outside every source.

```sh
# Preserve the target and each useful source. Copy the printed checkpoint IDs.
timber --store ./timber-store checkpoint ./project
timber --store ./timber-store checkpoint ./agent-navigation

# Verify each checkpoint before using it.
timber --store ./timber-store verify TARGET_ID
timber --store ./timber-store verify SOURCE_ID

# The source order here becomes the integration order.
timber --store ./timber-store plan --target TARGET_ID \
  --source SOURCE_ID --out plan.json

# Review plan.json, then build outside the source repositories.
timber --store ./timber-store preview plan.json --to ./review-preview

# Recheck whether the original sources have changed since capture.
timber --store ./timber-store assess plan.json
```

`TARGET_ID` and `SOURCE_ID` are placeholders for real checkpoint IDs printed by your run. A plan selects one layer per source: committed, staged, or working. Untracked and ignored additions require explicit selection for integration. The [user guide](docs/USER-GUIDE.md) explains these selections, recovery-folder mappings, and structured reports.

Preview construction does not run your project's code, builds, tests, hooks, or provider calls. Applying selected changes and deciding what to clean up remain separate actions.

## Review visually

Commands can emit structured JSON for local automation. Save a result and render it into a self-contained HTML file:

```sh
timber --store ./timber-store --json assess plan.json > assessment.json
timber --store ./timber-store report assessment.json --to report.html
```

Use the same checkpoint store when rendering a source-bound report.

A `partial` inspection (a timeout, a failed worktree, a missing registration) still renders; run `report` regardless of the inspect exit code and read the status field. Open `report.html` in a browser. Some embedded or sandboxed viewers refuse `file://` URLs; serve the report directory on a loopback port (for example `python3 -m http.server --bind 127.0.0.1 0` from that directory) and open it from there. It uses no scripts, remote assets, or local server. Paths, filenames, and errors remain data rather than executable markup. Review reports before sharing: they can reveal private project names and filenames.

## Recovery before cleanup

The workflow is:

```text
Inspect → preserve → compare → select → preview → review cleanup blockers
```

Recovery, consolidation, and acceptance are distinct:

| Evidence | What it establishes |
|---|---|
| Checkpoint captured | The selected supported state has been recorded. |
| Independent restore compared | The captured state can be reconstructed under the documented contract. |
| Clean integration preview | Selected snapshots combine mechanically in the recorded order. |
| Current assessment | Original sources were rechecked against the captured state. |
| Human review and project tests | A separate decision about usefulness and application behavior. |

An old folder can be captured and restored successfully while its former worktree history remains unknown. Similar filenames or similar patches never establish that work is redundant.

Inspection distinguishes same-path matches, competing versions, additions at proposed paths, and unresolved Git dependencies. Exact duplicate groups can overlap with copies matching the target; their counts must not be added as independent cleanup opportunities. Missing registrations are metadata-review candidates, not permission to delete branches or saved work.

For folders whose tracked work is held in another repository, explicit `import --map ... --dependency-repo ...` can preserve pinned commit or stash dependencies. `restore-git` reconstructs a captured group independently; inspection alone is not that recovery proof. See the [user guide](docs/USER-GUIDE.md).

## Verification and performance

The current source passed **80 automated tests** locally on macOS with Rust 1.98.1 and Rust 1.85.1, along with formatting and strict Clippy checks. The tests use disposable synthetic repositories and require no private project data or provider account. A configured GitHub Actions matrix is separate from a passing hosted run.

The [reproducible demonstration](docs/DEMO.md) covers successful and refused operations, recovery without original repositories, source drift, and corruption detection. Its recorded sample remains bound to the revision and executable identified there.

See the [matched performance study](docs/PERFORMANCE.md) for the original-versus-batched comparison. Timings are scoped to its synthetic fixture, build profile, and local conditions; they are not developer-productivity or large-repository guarantees.

## Development and contributions

```sh
cargo fmt --all --check
cargo clippy --all-targets --locked -- -D warnings
cargo test --locked
```

Bug reports and focused contributions are welcome. Use invented data for reproductions and retain expected refusals as well as successful cases. Read [CONTRIBUTING.md](CONTRIBUTING.md) before sharing reports or changes.

Further documentation: [user guide](docs/USER-GUIDE.md), [agent operator guide](docs/AGENT-OPERATORS.md), [deployment models](docs/DEPLOYMENT-MODELS.md), [field learnings](docs/FIELD-LEARNINGS.md), [changelog](CHANGELOG.md), [architecture](docs/ARCHITECTURE.md), [supported states](docs/SUPPORTED-STATES.md), [safety boundaries](docs/SAFETY.md), [demonstration](docs/DEMO.md), and [performance](docs/PERFORMANCE.md).

## License

Licensed under the [Apache License, Version 2.0](LICENSE-APACHE). Redistributions and derivative works must keep the attribution in [NOTICE](NOTICE). Versions 0.1.0 and 0.1.1 were released under MIT OR Apache-2.0 and remain available under those terms; from 0.2.0 on, Timber is Apache-2.0 only.

To cite Timber, use the "Cite this repository" button or [CITATION.cff](CITATION.cff). HTML reports and JSON results name the Timber version that produced them.

Unless you explicitly state otherwise, contributions intentionally submitted for inclusion are offered under the Apache License, Version 2.0.
