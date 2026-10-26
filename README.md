# Timber!

**Untangle dirty worktrees. Keep the useful work.**

See what changed across your Git worktrees, preserve unfinished work, and assemble a consolidation preview in a separate repository. Understand what remains before cleaning up.

Timber is for the moment when several worktrees contain useful changes and you can no longer confidently answer: *what is unique, what has already landed, and what can I safely put aside?*

**Status: experimental 0.1.** macOS has been checked locally. Linux is a target with automated checks configured, but has not yet been run in this development record. Review the [supported-state matrix](docs/SUPPORTED-STATES.md) before using it on important work. Keep another copy: a verified same-disk checkpoint is not independent backup.

## What Timber does

- Inventories registered worktrees and explicitly supplied preservation folders.
- Preserves staged contents separately from working files, including selected ignored assets.
- Restores checkpoints in isolation and compares the reconstructed state.
- Compares frozen sources with a chosen target, using exact ancestry and content evidence.
- Builds an ordered consolidation preview in its own repository, stopping on conflicts.
- Explains exclusions, retained alternatives, source drift, and cleanup blockers in the terminal or a local HTML report.

Timber has no move, delete, prune, or apply-to-source command. A clean preview establishes a mechanical merge, not application correctness or permission to discard the originals.

## Install from source

Prerequisites: macOS or Linux, Rust 1.85 or newer, and Git with `merge-tree --write-tree` support (Git 2.38 or newer).

```sh
cargo install --path . --locked
```

The package is named `timber-worktrees`; its installed command is `timber`. Registry publication is separate from the source repository. Until a package release is published, install from source.

## A first review

Choose a store outside your repositories, preferably on a different physical device if you need independent backup. For this example, `./timber-store` is a sibling of the example repositories, not a child of one.

```sh
# Inspect registered worktrees and an old folder of saved files.
timber --store ./timber-store inspect --repo ./project \
  --folder ./saved-agent-work

# Preserve the target and each useful source. Copy the printed checkpoint IDs.
timber --store ./timber-store checkpoint ./project
timber --store ./timber-store checkpoint ./agent-navigation
timber --store ./timber-store import ./saved-agent-work

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

`TARGET_ID` and `SOURCE_ID` are placeholders for real checkpoint IDs printed by your run. The [user guide](docs/USER-GUIDE.md) explains layer selection, extra files, folder mappings, and structured reports.

## Review visually

Commands can emit structured JSON for local automation. Save a result and render it into a self-contained HTML file:

```sh
timber --store ./timber-store --json assess plan.json > assessment.json
timber report assessment.json --to report.html
```

Open `report.html` in a browser. It uses no scripts, remote assets, or local server. Paths, filenames, and errors remain data rather than executable markup. Review reports before sharing: they can reveal private project names and filenames.

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

## Development

```sh
cargo fmt --check
cargo clippy --all-targets --locked -- -D warnings
cargo test --locked
```

The automated checks use disposable synthetic repositories. They must not depend on private source repositories, cloud services, or an AI provider.

Read the [architecture](docs/ARCHITECTURE.md), [failure handling](docs/SAFETY.md), [supported-state matrix](docs/SUPPORTED-STATES.md), and [recorded demonstration](docs/DEMO.md) for the design and the evidence actually obtained.

## License

Licensed under either the [MIT license](LICENSE-MIT) or the [Apache License, Version 2.0](LICENSE-APACHE), at your option.

Unless you explicitly state otherwise, contributions intentionally submitted for inclusion are offered under the same dual license.
