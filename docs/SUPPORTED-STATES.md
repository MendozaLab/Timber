# Supported state and claim limits

Version 0.1 targets macOS and Linux with a local filesystem. Windows is unsupported. The implementation refuses unsupported capture states rather than silently degrading a complete-recovery claim.

| State | Initial behavior |
|---|---|
| Regular files and directories, including empty directories | Capture paths and bytes; restore the documented permission bits. |
| Symbolic links | Preserve link targets without following them during capture or restoration. |
| Executable bits | Preserve and compare. |
| Staged contents different from working contents | Capture both versions; preview selects one layer. |
| Tracked deletions | Preserve their absence and logical index state. |
| Untracked files | Capture; integration requires explicit extra-file selection. |
| Ignored files | List; capture only explicitly selected paths; report omissions. |
| SHA-1 Git repositories | Supported; other Git object formats are refused in version one. |
| Detached commit | Capture the commit identity and reconstruct independently with detached HEAD; original branch name is recorded only. |
| Preservation folder with or without a receipt | Capture current supported files; historical completeness remains unknown. |
| External Git dependency declared by a folder | Keep the declaration and unresolved limits separate from captured files. |
| Missing registered worktree | Report the missing location; do not prune or repair it. |
| Unresolved merge, rebase, cherry-pick, or similar operation | Refuse a worktree checkpoint. |
| Submodules or nested repositories | Refuse capture; handle them separately. |
| Sparse, shallow, or partial repository; missing objects | Refuse worktree capture; do not fetch data. |
| Unsupported index flags or stages | Refuse rather than manufacture equivalent index state. |
| Cloud placeholders | Refuse content capture without triggering hydration. |
| Special files, including sockets, devices, and pipes | Refuse capture. |
| Non-UTF-8 filenames or filesystem collisions | Refuse with the affected path. |
| Custom or union merge driver | Refuse preview semantics that the isolated engine does not support. |
| Existing restoration or preview directory | Refuse overwrite. |

The recovery guarantee concerns the supported captured paths, contents, symlink targets, executable bits, logical staged index, and recorded base commit. It does not preserve running processes, application state, credentials held in memory, open database transactions, ownership, access-control lists, arbitrary extended attributes, resource forks, hardlink topology, or every filesystem timestamp.

A content hash establishes byte identity. It does not establish truth, ownership, usefulness, completeness of a former worktree, or approval to delete another copy.

A verified local checkpoint is not automatically an independent backup. Independent backup remains unassessed unless separately verified outside this tool's initial contract.
