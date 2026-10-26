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

## Recorded result

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

## Reproduce

Build and retain the two executables, then run from the project root:

```sh
python3 examples/benchmark_inspection.py \
  --before /path/to/original/timber \
  --after /path/to/optimized/timber \
  --output private/benchmark-new-run \
  --files 1000 --repeats 3
```

The script refuses an existing output directory. It creates only synthetic data, retains individual outputs and timings, and reports failures rather than silently omitting them. Output under `private/` is ignored.

For real repositories, report requested scan scope, input identity, completed versus interrupted status, and concurrent-change limits. A partial original run and a completed optimized run are useful operational observations, but do not establish an exact matched speedup. Real source paths and recovery receipts belong in private evidence, not public benchmark fixtures.
