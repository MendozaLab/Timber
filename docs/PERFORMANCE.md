# Inspection performance: matched synthetic test

This is a local wall-clock comparison of the original inspection approach and the batched implementation. It is not a measurement of developer productivity or a large-repository service guarantee.

## Method

- One generated repository with 1,001 indexed paths: 1,000 distinct text files and an ignore file.
- One registered worktree, so the original all-worktree default and new focused default cover the same source.
- Identical staged, working, deleted, untracked, and ignored fixture states for both executables.
- One unmeasured warmup per executable, then three measured rounds in alternating order.
- Both executables frozen and identified by their SHA-256 digests.
- Every run checks the same HEAD, staged and working differences, untracked and ignored findings.
- Before/after checks verify source file hashes and modes, index bytes, references, and configuration.
- Setup, preservation checks, and human review are outside the timed command.

Local background load and filesystem caching are not controlled beyond the warmups and alternating run order. Retain individual measurements rather than treating the median as a universal promise.

## Debug-mode result

Recorded on 2026-10-26, macOS, Git 2.51.1, using debug builds of Timber 0.1.0.

| Variant | Median wall time |
|---|---:|
| Original per-indexed-path Git subprocess | 10.098630 seconds |
| Streaming batched indexed-object comparison | 0.457985 seconds |

The ratio is **22.05 times faster**, or **95.46% less command wall time**, on this fixture. All compared findings matched, and every source-preservation check passed.

| Measured round | Original seconds | Batched seconds |
|---|---:|---:|
| 1, original first | 11.224695 | 0.457985 |
| 2, batched first | 10.098630 | 0.474236 |
| 3, original first | 9.881309 | 0.453729 |

The original executable has digest `ab598344a4d8cf5ba291f5c1f52b25df5a642a0ac85ad422b569d56bee3b99ec`. Its implementation matches the original public source snapshot. The measured optimized executable has digest `a7f71e9100bff48ab76daa57f8af3188ca1718e8ea6e3af81124aaf3010f8492`, built from development commit `22febd34fea8afadc10bc064981c12a09cc1ada8`. Subsequent minimum-toolchain lint repairs changed how newline-separated requests are constructed; they did not change the object requests or comparison rules. The measurement above remains bound to the exact executable digest stated here.

The structural improvement is independently covered by a synthetic test: 1,001 unique indexed blobs are fingerprinted through one streaming Git process instead of one process per path. Working files are hashed through a fixed-size buffer. This avoids retaining the whole indexed object set in memory.

## Release-mode result

A separate matched run on 2026-10-26 used **release builds for both variants** and the same 1,001-path synthetic fixture. Each executable received one unmeasured warmup, followed by three measured rounds in alternating order. All findings matched, every source-preservation check passed, and every invocation exited successfully.

| Variant | Median wall time |
|---|---:|
| Original per-indexed-path Git subprocess | 10.467170 seconds |
| Streaming batched indexed-object comparison | 0.425302 seconds |

The release-to-release median ratio is **24.61 times faster**, or **95.94% less command wall time**, on this fixture. This ratio compares the two release executables; it does not mix a debug baseline with an optimized release result.

| Measured round | Original release seconds | Batched release seconds |
|---|---:|---:|
| 1, original first | 10.119156 | 0.408828 |
| 2, batched first | 10.467170 | 0.425302 |
| 3, original first | 10.859345 | 0.516508 |

The release executables are bound to these source revisions and SHA-256 digests:

| Variant | Source revision | Executable digest |
|---|---|---|
| Original release | `6e43c5e22230138f569bb0b42ade82f23c373656` | `5a1a4d047532af9b6098061bae7ac9c8a29b9a88c51b11453d160bd7bd94debe` |
| Batched release | `4d84c0af6d89c9df64758f3c70093dd43d6a100e` | `92cd4a5a2ffd67cf0a11ea2060ddf57d0dc14a137d3da782f62aa34df786bfe9` |

The debug and release tables are separate observations with their own frozen executables. Do not interpret the difference between those tables as an isolated effect of compiler optimization: they were separate runs under uncontrolled background load. Neither table measures developer productivity or guarantees timings for real repositories.

## Reproduce

Build and retain the two executables with the **same build profile**, then run from the project root. For a release-mode comparison, build both source revisions with `cargo build --release --locked` and retain the resulting `timber` executables:

```sh
python3 examples/benchmark_inspection.py \
  --before /path/to/original/timber \
  --after /path/to/optimized/timber \
  --output private/benchmark-new-run \
  --files 1000 --repeats 3
```

The script refuses an existing output directory. It creates only synthetic data, retains individual outputs and timings, and reports failures rather than silently omitting them. Output under `private/` is ignored.

For real repositories, report requested scan scope, input identity, completed versus interrupted status, and concurrent-change limits. A partial original run and a completed optimized run are useful operational observations, but do not establish an exact matched speedup. Real source paths and recovery receipts belong in private evidence, not public benchmark fixtures.
