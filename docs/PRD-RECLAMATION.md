# Product requirements: reclaim incomplete captures

Status: implemented 2026-10-28 (see the implementation note below). Source: `docs/FIELD-LEARNINGS.md`, "main checkout" entries of 2026-10-27.

## Problem

Four refused captures of one large worktree (each refusal correct: nested repositories, an over-wide exclusion, a file written by the operator during capture, files written by background daemons during capture) left about 19 GB of content-addressed objects in the store beside three verified checkpoints totalling 385 MB. The store keeps incomplete material by design (`docs/SAFETY.md`, "Interrupted or corrupt captures") and has no command that reclaims it, so the only cleanup today is deleting the whole store, which also deletes the verified checkpoints. On a disk at 99 percent that is a real cost, and an agent operator cannot fix it without a human and a risky manual walk of the object directory.

## Requirements

1. **Each capture journals what it wrote.** The incomplete marker lists the object ids the run added to the store (not the ones that already existed). Objects are content-addressed and may be shared, so the journal is the only record of what a run contributed.
2. **`timber gc --dry-run`** lists every incomplete marker with its age, the objects referenced only by incomplete runs (never by a completed manifest), and the bytes that would be reclaimed. It writes nothing.
3. **`timber gc`** removes those markers and objects, then re-verifies the integrity of every completed manifest's object references and refuses to delete anything if that check finds a missing object before deletion begins. Output states what was removed and what was kept, with bytes.
4. **Refusal-time offer, never automatic deletion.** When a capture is refused, the message states the incomplete marker id and the bytes it holds, and names `timber gc --dry-run`. Timber does not delete on its own.
5. **Nothing outside the store is touched.** `gc` has no source path argument and cannot be pointed at a worktree.
6. **Regression tests.** Synthetic fixtures: a refused capture leaves a marker with a journal; dry-run reports the right bytes and lists no object referenced by a completed checkpoint; gc removes exactly those and the completed checkpoints still verify; gc refuses when a completed manifest references a missing object.

## Out of scope

- Deleting verified checkpoints (a separate, explicit command if ever wanted).
- Any scheduler or hook. Operators may schedule `timber gc --dry-run` themselves; Timber ships no background process.

## Acceptance

The three gates pass; on the field case, `timber gc --dry-run` reports the four incomplete markers and about 19 GB reclaimable, and `timber gc` reclaims them while the three verified checkpoints still verify.

## Implementation note (2026-10-28)

- **Journal.** The incomplete marker keeps its pretty-printed JSON header, now with `"journal": 1`, and each object the run adds is appended as one JSON line (`{"object": <id>, "bytes": <n>}`) and synced as the run proceeds. An object counts as added only when that run's write created it, so pre-existing shared objects are never claimed. A header without `journal` is an earlier version's marker and is reported as `journal absent`; a malformed tail is reported as `truncated` and the entries before it still count.
- **Reachability governs deletion.** `gc` removes every object no completed manifest references, whether or not a journal lists it (an unreferenced object no journal names is reported as unattributed). Journals attribute bytes to runs; they never authorize deleting a referenced object.
- **Order and integrity.** `gc` refuses before deleting anything when a completed manifest references an absent object, naming the checkpoint. The check is presence as a regular file; `timber verify` remains the content check. Deletion removes objects first and markers last, so an interrupted `gc` still leaves the markers for the next dry run, and it confirms afterwards that every completed checkpoint still has its objects. Leftover temporary files in `objects/` and bundle staging directories in `incomplete/` are reclaimed too; unrecognized items are kept and listed.
- **Concurrency.** A capture holds a shared advisory lock on the store root for its whole run and an exclusive lock on its own marker; `gc` takes the store lock exclusively and refuses while any capture holds it, and a capture refuses while `gc` runs. `gc --dry-run` takes no store lock and reports a locked marker as in progress. Lock attempts wait up to about one second before refusing.
- **Acceptance.** The three gates pass on synthetic fixtures (`tests/reclaim.rs`). The field case (four markers, about 19 GB) is not measured here.
