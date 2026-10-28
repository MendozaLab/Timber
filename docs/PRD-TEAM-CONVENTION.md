# Product requirements: team convention file and `timber init`

Status: implemented 2026-10-28 (proposed 2026-10-28). Source: `docs/DEPLOYMENT-MODELS.md`, small-team row.

## Problem

Two people using Timber on one repository each pick a store, an exclusion list and a cadence by hand, and their agents do the same. The field runs of 2026-10-27 needed four exclusions (fixture folders, agent-tool worktree folders, daemon output) that every future run on that repository will need again, and nothing records them.

## Requirements

1. **One file, in the repository, committed:** `timber.toml` at the repository root with `store` (a path outside every source; may be absolute or an environment-variable reference), `exclude` (paths relative to the worktree, applied to every `checkpoint` as if passed with `--exclude`), `git_timeout_seconds`, and `schedule` (a free-text note; Timber never schedules anything itself).
2. **`timber init`** writes a commented template and refuses to overwrite an existing file.
3. **Explicit beats implicit.** Command-line `--exclude` and `--git-timeout` add to or override the file; the manifest records which exclusions came from the file and which from the command line, so a reviewer can tell convention from operator choice.
4. **Nothing silent.** `inspect` and `checkpoint` print the convention file they read, or that none was found. A convention file with an unknown key is refused, naming the key.
5. **Tests.** Synthetic fixtures: init writes and refuses to overwrite; checkpoint applies file exclusions and records their origin; command-line exclusions override; unknown keys refuse.

## Out of scope

- Any scheduling, daemon or service.
- Any shared-store synchronization.

## Acceptance

The three gates pass; on the field repository, a committed `timber.toml` with the four known exclusions lets `timber checkpoint <main checkout>` run without command-line flags and the manifest shows the exclusions with reason `excluded_by_convention_file`.

## Implementation note

Implemented in `src/convention.rs`, with the wiring in `src/main.rs` and `src/capture.rs` and tests in `tests/convention.rs` plus unit tests in the module.

- **No new dependency.** No TOML crate is in `Cargo.lock`, so a minimal parser reads only what the four keys need: `#` comments, bare keys, basic and literal single-line strings, decimal integers and string arrays that may span lines. Tables, dotted keys, multi-line strings, repeated keys and other value types are refused with the line named. Unknown keys are refused as `unknown key in timber.toml: <key> (line N)`.
- **Manifest reason.** File exclusions are recorded with reason `excluded_by_convention_file`. Command-line exclusions keep `excluded_by_operator`. `Verification` gains `excluded_by_convention_file`, omitted when empty, so earlier records keep their shape. `model::is_skipped_folder` treats `excluded_by_operator`, `excluded_by_convention_file` and `linked_worktree_captured_separately` alike as declared folders, so planning's ignored-coverage check and the `assess` rescan handle all three the same way. No schema number changed.
- **Linked worktrees.** Convention exclusions coexist with the automatic `linked_worktree_captured_separately` records: the linked-worktree walk does not enter any declared exclusion, a convention entry naming a linked worktree folder is accepted and recorded as `excluded_by_convention_file`, and a declared exclusion of either origin strictly inside a linked worktree is refused, naming its origin. Manifest order is operator exclusions, convention exclusions, linked worktrees, then ignored omissions.
- **Same refusals as `--exclude`.** File exclusions go through the same validation (tracked path, no match, symbolic link, overlap, overlap with `--include-ignored`); messages name the origin as `timber.toml exclude`. A file entry that matches nothing refuses the run, as `--exclude` does; a team whose disposable folder is sometimes absent must edit the file or create the folder.
- **Override rules.** `--store` and `--git-timeout` win over the file. `--exclude` adds; when it equals or contains a file entry, that entry yields for the run. An `--include-ignored` selection that overlaps a file entry overrides it. An `--exclude` strictly inside a file entry is refused, naming both. Overrides are reported under `convention.exclusions_overridden`.
- **Store precedence.** `--store`, then the file's `store`, then `TIMBER_STORE`, then the OS default. `store` must resolve to an absolute path; one leading `$NAME` or `${NAME}` is expanded and an unset variable is refused by name. The existing outside-every-source check still applies.
- **Where the file is read.** `inspect` and `checkpoint` locate the repository root with `git rev-parse --show-toplevel`, read `timber.toml` there, print `read convention file <path>` or `no convention file found at <path>` to standard error before other work, and add a `convention` object to the result. Commands without a repository argument do not read the file. `init` writes only at the worktree root, never overwrites, and refuses a symbolic link at the name.
- **Store creation timing.** As with `--exclude`, a checkpoint whose exclusions are refused may leave a newly created, empty store directory; no checkpoint or incomplete marker is written.
