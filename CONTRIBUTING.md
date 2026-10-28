# Contributing to Timber

Timber is experimental developer tooling for preservation, recovery, and consolidation review. Contributions should make those operations easier to understand and verify while keeping original repositories unchanged.

## Report a problem

Open a [bug report](https://github.com/MendozaLab/Timber/issues/new?template=bug_report.md) with:

- Your operating system, `timber --version`, and `git --version`.
- The command, expected result, and actual result or error.
- A small reproduction using invented files and disposable repositories.
- Relevant state, such as staged versus working changes, ignored paths, nested repositories, or a recovery-folder mapping.

Do not upload private repositories, checkpoint stores, credentials, correspondence, or unreviewed reports. Reports can contain identifying filenames and paths. For sensitive security findings, follow the [security-report guidance](docs/SAFETY.md#security-reports) before posting details publicly.

## Prepare a change

Use the Rust toolchain and Git requirements in the [README](README.md). Keep changes focused and include a synthetic regression for a behavior change. A useful regression should distinguish the intended behavior from a plausible failure, including relevant refusal cases.

```sh
cargo fmt --all --check
cargo clippy --all-targets --locked -- -D warnings
cargo test --locked
```

For performance changes, compare the same frozen input and build profile. Preserve individual results, verify matching findings and unchanged sources, and state what is outside the measurement. The [inspection benchmark](docs/PERFORMANCE.md) describes the existing method.

Pin third-party GitHub Actions to full commit IDs, with a comment identifying the reviewed version. The repository's Actions policy rejects moving tags and branch references before checks can run.

Before opening a pull request:

- Describe the behavior change and the problem it solves.
- State the commands and conditions actually checked.
- Update affected command examples and supported-state documentation.
- Keep private fixtures, raw local reports, generated stores, and executables out of the change.

## Preserve the product boundary

Exact Git and filesystem evidence governs recovery checks. Similarity, a successful build, or a model's confidence must not become permission to discard work. Keep staged and working alternatives distinct, expose incomplete results, and preserve the original failure when repairing a bug.

Original source repositories remain outside the preview workspace. Project code, hooks, content filters, and provider calls must not run automatically during preservation or consolidation.

See the [architecture](docs/ARCHITECTURE.md), [safety boundaries](docs/SAFETY.md), and [supported-state matrix](docs/SUPPORTED-STATES.md) for the current contract. Contributions are offered under the repository's Apache-2.0 terms (see [NOTICE](NOTICE)) unless explicitly stated otherwise.
