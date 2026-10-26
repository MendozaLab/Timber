# Architecture

Timber is one Rust package with separate internal boundaries for discovery, capture, storage, restoration, comparison, planning, preview construction, and presentation.

## Data flow

```text
Explicit repository and folder roots
                │
                ▼
       Read-only inspection
                │
                ▼
  Immutable checkpoint + exclusions
                │
                ▼
    Independent restore comparison
                │
                ▼
   Ordered plan of selected snapshots
                │
                ▼
 Independent integration repository
                │
                ▼
  Partial or complete preview + evidence
                │
                ▼
Terminal / JSON / self-contained HTML
```

The source repositories do not become the integration workspace. Every preview consumes checkpoint identities, so source changes cannot silently alter a recorded integration attempt.

## Checkpoints

A versioned manifest records source identity, selected filesystem entries, content hashes, exclusions, capture conditions, and any Git state. Objects are stored by content identity. Source paths provide lineage; they are not needed as object-store alternates during recovery.

Git history and the staged index are distinct data. Reachable committed history is bundled, while staged blob contents are separately preserved. Working files are captured from their actual bytes without passing through Git filters.

A folder import represents the files present at import time. Optional mappings declare group names, destination paths, and external Git dependencies. They do not manufacture a former index or establish complete historical recovery.

## Integration previews

The plan selects one snapshot layer per source: original commit, staged index, or current working contents. Explicit extra-file selections control additions beyond the tracked tree.

For a changed snapshot, Timber creates a synthetic commit parented to the source's original commit. It merges selected sources in plan order inside an independent repository. Original history retains its original authorship; synthetic snapshots identify Timber and the source checkpoint.

The preview uses Git's tree merge plumbing and structured conflict records. A clean merge may proceed to the next source. A conflict stops the sequence and preserves the partial result, conflict tree, and pending source list.

## Reporting

Terminal, structured JSON, and HTML are views of the same records. Presentation cannot strengthen the underlying claim. Recovery verification, historical completeness, backup independence, mechanical integration, and human acceptance stay separate.

The HTML view is a static local file. It escapes every data string, uses native disclosure elements for complete evidence, and loads no scripts or external assets.

## Deliberately absent infrastructure

Timber requires no AI provider, vector database, background service, account login, or private orchestration framework. Exact Git and filesystem rules govern the first release.

Jev audit completed: no bounded semantic decision in this task would benefit from Jev. Similarity and usefulness judgments remain with the developer rather than being converted into cleanup permission.
