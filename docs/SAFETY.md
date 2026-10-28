# Failure handling and safety boundaries

Timber preserves work and prepares review evidence. It does not decide that a developer's work deserves to be discarded.

## Source purity

Source worktree files, references, index, and configuration must stay unchanged during inspection, capture, verification, and preview. Timber does not stage, commit, stash, reset, clean, move, prune, or delete source work.

Git commands use a restricted environment. Checkpoint restoration and preview use generated repositories with independent objects. They do not copy source configuration, templates, hooks, remotes, signing setup, object alternates, external filters, or merge drivers.

No project code runs automatically. A clean mechanical merge still requires deliberate application review and testing.

## Interrupted or corrupt captures

A capture is not complete until its referenced data and completed manifest have been durably published. Interrupted operations remain incomplete. Reusing an incomplete operation as a verified checkpoint is prohibited.

An incomplete capture stays in the store, with its marker and every object it wrote, until an operator runs `timber gc`; Timber never deletes on its own, and a refusal message names the marker, its bytes and `timber gc --dry-run`. Each marker journals the objects its run added, for attribution only. `gc` removes markers and only those objects no completed manifest references, and it never touches completed checkpoints or anything outside the store. It refuses before deleting anything when a completed manifest references a missing object, and while a capture holds the store.

Verification recomputes stored-content hashes and reconstructs the captured supported state. Missing data, mismatched content, unexpected restored files, changed modes, or mismatched Git state cause failure. Preserve the error and original data; do not bypass verification to reclaim space.

## Concurrent writers

Prefer pausing editors, agents, and generators before capture. Timber records any pause declaration independently of its own scans. It does not claim exclusive access to a worktree merely because scans match, and Git's worktree-management lock does not stop file edits.

If state changes during capture, retry only after understanding the writer. If it changes afterward, the immutable checkpoint can remain valid while the current cleanup assessment becomes stale.

## Untrusted paths and reports

Treat manifests, plans, mappings, receipts, filenames, and reports as untrusted input.

- Absolute paths, traversal, duplicate entries, and conflicting file/directory shapes cannot direct restoration outside the destination.
- Stored symbolic links must never redirect subsequent writes.
- Stores and destinations must not be nested inside captured sources.
- User labels and errors are escaped in HTML and terminal output.
- HTML reports have no scripts, remote assets, active links, or local server.

A local report can reveal private paths and filenames. Review the exact report before sharing it.

## Historical folders and exclusions

A folder captured today can restore correctly while the history of its contents remains unknown. Existing receipts are preserved evidence, not automatic proof of their own claims.

Missing external Git dependencies, omitted ignored assets, retained staged alternatives, and unmapped folder files remain visible. Similar names and similar patches cannot authorize removing either version.

## Conflicted previews

A preview stops at the first conflict and keeps its partial result. Pending sources remain pending. Git's merge status determines success; a lack of visible conflict markers is not evidence of a clean merge.

Do not apply a partial preview as if every source had been consolidated. Change the plan or resolve competing versions in a separately reviewed workflow.

## Security reports

Describe a minimal synthetic reproduction without private project files or secrets. Until a private reporting channel is designated, contact the repository owner through an existing private channel before posting sensitive details publicly.
