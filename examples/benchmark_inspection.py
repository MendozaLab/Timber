#!/usr/bin/env python3
"""Matched, synthetic inspection benchmark. Never points at a real source repo."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import shutil
import statistics
import subprocess
import time


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--before", type=Path, required=True)
    parser.add_argument("--after", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--files", type=int, default=1000)
    parser.add_argument("--repeats", type=int, default=3)
    args = parser.parse_args()
    if args.output.exists() or args.files < 10 or args.repeats < 1:
        parser.error("use a fresh output directory, at least 10 files and one repeat")
    root = args.output.resolve()
    root.mkdir(parents=True, mode=0o700)
    binaries = {}
    for name, source in [("before", args.before), ("after", args.after)]:
        destination = root / f"timber-{name}"
        shutil.copyfile(source, destination)
        destination.chmod(0o700)
        binaries[name] = destination
    env = {key: value for key, value in os.environ.items() if not key.startswith("GIT_")}
    env.update(GIT_CONFIG_NOSYSTEM="1", GIT_CONFIG_GLOBAL="/dev/null",
               GIT_CONFIG_SYSTEM="/dev/null", GIT_ATTR_NOSYSTEM="1",
               GIT_OPTIONAL_LOCKS="0", GIT_TERMINAL_PROMPT="0")
    repo = root / "synthetic-repo"
    repo.mkdir()

    def git(*words):
        return subprocess.check_output(["git", "-c", "core.hooksPath=/dev/null",
                                        "-c", "commit.gpgSign=false", *words], cwd=repo, env=env)

    git("init", "-q", "--template=", "--initial-branch=main")
    git("config", "user.name", "Synthetic Benchmark")
    git("config", "user.email", "fixture@example.invalid")
    (repo / "files").mkdir()
    for index in range(args.files):
        (repo / "files" / f"{index:06}.txt").write_text(
            f"Synthetic distinct object {index}\n" + "benchmark fixture\n" * 12)
    (repo / ".gitignore").write_text("ignored.cache\n")
    git("add", ".")
    git("commit", "-qm", "Synthetic frozen baseline")
    (repo / "files/000000.txt").write_text("staged synthetic draft\n")
    git("add", "files/000000.txt")
    (repo / "files/000000.txt").write_text("working synthetic draft\n")
    (repo / "files/000001.txt").unlink()
    (repo / "untracked.txt").write_text("untracked synthetic input\n")
    (repo / "ignored.cache").write_text("synthetic ignored input\n")

    def fingerprint():
        files = {}
        for path in sorted(repo.rglob("*")):
            if ".git" in path.relative_to(repo).parts or not path.is_file():
                continue
            files[str(path.relative_to(repo))] = [hashlib.sha256(path.read_bytes()).hexdigest(),
                                                   path.stat().st_mode & 0o777]
        return {"files": files, "index": hashlib.sha256((repo / ".git/index").read_bytes()).hexdigest(),
                "head": git("rev-parse", "HEAD").decode().strip(),
                "refs": git("show-ref").decode(),
                "config": hashlib.sha256((repo / ".git/config").read_bytes()).hexdigest()}

    before_state = fingerprint()
    calls = []
    expected = None

    def run(name, round_number, warmup=False):
        nonlocal expected
        started = time.perf_counter()
        result = subprocess.run([str(binaries[name]), "--json", "inspect", "--repo", str(repo)],
                                env=env, capture_output=True, timeout=120)
        elapsed = time.perf_counter() - started
        label = f"{'warmup' if warmup else 'round'}-{round_number}-{name}"
        (root / f"{label}.stdout.json").write_bytes(result.stdout)
        (root / f"{label}.stderr.txt").write_bytes(result.stderr)
        if result.returncode != 0:
            raise RuntimeError(f"{label} failed with exit {result.returncode}; evidence retained")
        value = json.loads(result.stdout)["result"]
        rows = value["worktrees"]
        assert len(rows) == 1, "benchmark scope must be exactly one worktree"
        row = rows[0]
        finding = {key: row[key] for key in ["head", "staged_paths", "working_paths", "untracked", "ignored"]}
        if expected is None:
            expected = finding
        assert finding == expected, f"{label} differs in inspection findings"
        assert fingerprint() == before_state, f"{label} changed a source input"
        calls.append({"variant": name, "round": round_number, "warmup": warmup,
                      "seconds": elapsed, "exit": result.returncode,
                      "findings_match": True, "source_unchanged": True})

    run("before", 0, True)
    run("after", 0, True)
    for round_number in range(1, args.repeats + 1):
        order = ["before", "after"] if round_number % 2 else ["after", "before"]
        for name in order:
            run(name, round_number)
    timings = {name: [call["seconds"] for call in calls
                      if call["variant"] == name and not call["warmup"]]
               for name in binaries}
    medians = {name: statistics.median(times) for name, times in timings.items()}
    record = {"schema": 1, "kind": "matched_synthetic_inspection",
              "files_requested": args.files, "indexed_paths": args.files + 1,
              "registered_worktrees": 1, "repeats": args.repeats,
              "warmups_per_binary": 1, "order": "alternating, before first in odd rounds",
              "binary_sha256": {name: hashlib.sha256(path.read_bytes()).hexdigest()
                                for name, path in binaries.items()},
              "median_seconds": medians,
              "speedup_ratio": medians["before"] / medians["after"],
              "time_reduction_percent": 100 * (1 - medians["after"] / medians["before"]),
              "all_findings_match": True, "all_source_checks_unchanged": True,
              "calls": calls,
              "limits": ["Synthetic fixture only; no developer time-savings claim.",
                         "Warm-cache comparison; fixtures and measured code paths are identical.",
                         "Timing excludes setup, source-preservation checks and human review."]}
    (root / "metrics.json").write_text(json.dumps(record, indent=2) + "\n")
    print(json.dumps({key: value for key, value in record.items() if key != "calls"}, indent=2))


if __name__ == "__main__":
    main()
