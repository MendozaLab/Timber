# Product requirements: team convention file and `timber init`

Status: proposed 2026-10-28, not implemented. Source: `docs/DEPLOYMENT-MODELS.md`, small-team row.

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

The three gates pass; on the field repository, a committed `timber.toml` with the four known exclusions lets `timber checkpoint <main checkout>` run without command-line flags and the manifest shows the exclusions as `from_convention_file`.
