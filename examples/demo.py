#!/usr/bin/env python3
"""Run Timber's synthetic recovery/consolidation demonstration without real project data.

Requires an already built Timber binary and Git. Every fixture and result is created
under a new output directory; existing directories are refused and never removed.
"""
from __future__ import annotations

import argparse
import hashlib
import json
import os
from pathlib import Path
import shutil
import stat
import subprocess
import time

PROJECT = Path(__file__).resolve().parents[1]


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--output", type=Path, default=PROJECT / "demo-output")
    parser.add_argument("--binary", type=Path, default=PROJECT / "target/debug/timber")
    args = parser.parse_args()
    binary = args.binary.resolve(strict=True)
    output = args.output.absolute()
    if output.exists() or output.is_symlink():
        parser.error("output already exists; select a new directory (the demo never deletes it)")
    output.mkdir(mode=0o700)
    output = output.resolve()
    # Freeze the executable so concurrent local builds cannot mix versions across calls.
    frozen_binary = output / "timber-demo"
    shutil.copy2(binary, frozen_binary)
    binary = frozen_binary
    binary_sha256 = hashlib.sha256(binary.read_bytes()).hexdigest()
    fixtures = output / "fixtures"
    fixtures.mkdir()
    results = output / "results"
    results.mkdir()
    store = output / "store"
    env = {k: v for k, v in os.environ.items() if not k.startswith("GIT_")}
    env.update({
        "GIT_CONFIG_NOSYSTEM": "1", "GIT_CONFIG_SYSTEM": "/dev/null",
        "GIT_CONFIG_GLOBAL": "/dev/null", "GIT_ATTR_NOSYSTEM": "1",
        "GIT_OPTIONAL_LOCKS": "0", "GIT_TERMINAL_PROMPT": "0", "LC_ALL": "C",
    })

    def git(repo: Path, *command: str) -> bytes:
        return subprocess.check_output([
            "git", "-C", str(repo), "-c", "core.hooksPath=/dev/null",
            "-c", "commit.gpgSign=false", "-c", "core.fsmonitor=false", *command,
        ], env=env, stderr=subprocess.PIPE)

    def write(path: Path, text: str) -> None:
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(text, encoding="utf-8")

    def snapshot(repo: Path) -> dict:
        """Content, modes, logical identity and exact index/config bytes of our fixtures."""
        files = {}
        for root, dirs, names in os.walk(repo, followlinks=False):
            relative = Path(root).relative_to(repo)
            if relative == Path("."):
                dirs[:] = [d for d in dirs if d != ".git"]
                names = [n for n in names if n != ".git"]
            for name in sorted(dirs + names):
                path = Path(root) / name
                metadata = path.lstat()
                key = str(path.relative_to(repo))
                if path.is_symlink():
                    content = os.fsencode(os.readlink(path))
                elif path.is_file():
                    content = path.read_bytes()
                else:
                    content = b""
                files[key] = [stat.S_IFMT(metadata.st_mode), stat.S_IMODE(metadata.st_mode), hashlib.sha256(content).hexdigest()]
        index = Path(os.fsdecode(git(repo, "rev-parse", "--git-path", "index").strip()))
        if not index.is_absolute():
            index = repo / index
        common = Path(os.fsdecode(git(repo, "rev-parse", "--git-common-dir").strip()))
        if not common.is_absolute():
            common = repo / common
        return {
            "files": files,
            "index_sha256": hashlib.sha256(index.read_bytes()).hexdigest(),
            "config_sha256": hashlib.sha256((common / "config").read_bytes()).hexdigest(),
            "head": git(repo, "rev-parse", "HEAD").decode().strip(),
            "head_reference": git(repo, "symbolic-ref", "HEAD").decode().strip(),
            "references": git(repo, "for-each-ref", "--format=%(refname) %(objectname)").decode(),
        }

    target = fixtures / "target"
    target.mkdir()
    git(target, "init", "-q", "--template=", "--initial-branch=main")
    git(target, "config", "user.name", "Synthetic Developer")
    git(target, "config", "user.email", "fixture@example.invalid")
    for name, text in {
        "shared.txt": "shared base\n", "target.txt": "target base\n",
        "a.txt": "agent a base\n", "b.txt": "agent b base\n",
        ".gitignore": "*.cache\n",
    }.items():
        write(target / name, text)
    git(target, "add", ".")
    git(target, "commit", "-qm", "Synthetic common base")
    agent_a, agent_b, conflict = (fixtures / name for name in ("agent-a", "agent-b", "conflict"))
    for path, branch in [(agent_a, "agent-a"), (agent_b, "agent-b"), (conflict, "conflict")]:
        git(target, "worktree", "add", "-q", "-b", branch, str(path))
    write(target / "target.txt", "target unfinished work\n")
    write(target / "shared.txt", "target shared change\n")
    write(agent_a / "a.txt", "agent a staged version\n")
    git(agent_a, "add", "a.txt")
    write(agent_a / "a.txt", "agent a working version\n")
    write(agent_a / "notes.txt", "explicitly selected untracked note\n")
    write(agent_a / "asset.cache", "ignored asset, explicitly preserved\n")
    write(agent_b / "b.txt", "agent b committed version\n")
    git(agent_b, "add", "b.txt")
    git(agent_b, "commit", "-qm", "Synthetic agent b committed work")
    write(agent_b / "b.txt", "agent b unfinished follow-up\n")
    write(conflict / "shared.txt", "conflicting agent change\n")
    old_folder = fixtures / "old-folder"
    old_folder.mkdir()
    write(old_folder / "legacy.txt", "preserved historical note\n")
    write(old_folder / "producer-receipt.json", json.dumps({
        "schema": "synthetic-producer-claim/v1", "claimed_complete": True,
        "warning": "This is a fixture claim, not proof of historical completeness.",
    }, indent=2))
    import_map = output / "import-map.json"
    write(import_map, json.dumps({
        "schema": 1, "groups": {"legacy": "Synthetic old folder; historical completeness unknown"},
        "files": [{"stored_path": "legacy.txt", "target_path": "notes/legacy.txt", "base_sha256": None}],
        "git_dependencies": [],
    }, indent=2))
    roots = [target, agent_a, agent_b, conflict]
    before = [snapshot(path) for path in roots]
    folder_before = {p.name: hashlib.sha256(p.read_bytes()).hexdigest() for p in old_folder.iterdir()}
    calls = []

    def timber(label: str, *command: str, expected: int = 0, selected_store: Path = store) -> dict:
        destination = results / f"{label}.json"
        argv = [str(binary), "--store", str(selected_store), "--json", "--out", str(destination), *command]
        started = time.perf_counter()
        process = subprocess.run(argv, env=env, capture_output=True, check=False)
        elapsed = time.perf_counter() - started
        (results / f"{label}.stdout").write_bytes(process.stdout)
        (results / f"{label}.stderr").write_bytes(process.stderr)
        calls.append({
            "label": label, "command": ["timber" if i == 0 else str(a).replace(str(output), "<demo-root>") for i, a in enumerate(argv)],
            "expected_exit": expected, "observed_exit": process.returncode, "elapsed_seconds": round(elapsed, 6),
        })
        if process.returncode != expected:
            raise AssertionError(f"{label}: expected exit {expected}, observed {process.returncode}\n{process.stderr.decode(errors='replace')}")
        return json.loads(process.stdout if process.stdout.strip() else process.stderr)

    timber("inspection", "inspect", "--repo", str(target), "--folder", str(old_folder))
    target_capture = timber("target-checkpoint", "checkpoint", str(target), "--writers-paused")
    a_capture = timber("a-checkpoint", "checkpoint", str(agent_a), "--include-ignored", "asset.cache", "--writers-paused")
    b_capture = timber("b-checkpoint", "checkpoint", str(agent_b), "--writers-paused")
    conflict_capture = timber("conflict-checkpoint", "checkpoint", str(conflict), "--writers-paused")
    folder_capture = timber("folder-import", "import", str(old_folder), "--map", str(import_map), "--writers-paused")
    captures = [target_capture, a_capture, b_capture, conflict_capture, folder_capture]
    assert all(c["result"]["verification"]["verified"] for c in captures)
    target_id, a_id, b_id, conflict_id, folder_id = [c["result"]["checkpoint"]["id"] for c in captures]
    assert folder_capture["result"]["verification"]["historical_completeness"] == "unknown"
    timber("a-verify", "verify", a_id)
    timber("a-restore", "restore", a_id, "--to", str(output / "a-restored"))
    assert (output / "a-restored/a.txt").read_text() == "agent a working version\n"
    assert git(output / "a-restored", "show", ":a.txt").decode() == "agent a staged version\n"
    generated = timber("plan-generated", "plan", "--target", target_id, "--source", a_id, "--source", b_id, "--source", folder_id)["result"]
    assert generated["comparisons"][0]["staged_alternates"] == ["a.txt"]
    generated["sources"][0]["extra_paths"] = ["notes.txt"]
    selected_plan = output / "plan-selected.json"
    write(selected_plan, json.dumps(generated, indent=2))
    preview_path = output / "preview-complete"
    preview_result = timber("preview-complete", "preview", str(selected_plan), "--to", str(preview_path))
    assert preview_result["result"]["status"] == "complete"
    expected_files = {
        "target.txt": "target unfinished work\n", "shared.txt": "target shared change\n",
        "a.txt": "agent a working version\n", "b.txt": "agent b unfinished follow-up\n",
        "notes.txt": "explicitly selected untracked note\n", "notes/legacy.txt": "preserved historical note\n",
    }
    assert all((preview_path / name).read_text() == text for name, text in expected_files.items())
    assert not (preview_path / "asset.cache").exists(), "captured ignored asset must not be silently integrated"
    assert not (preview_path / ".git/objects/info/alternates").exists()
    assert not git(preview_path, "remote").strip()
    assert a_capture["result"]["checkpoint"]["git"]["head"] in git(preview_path, "rev-list", "HEAD").decode()
    timber("complete-report", "report", str(results / "preview-complete.json"), "--to", str(output / "complete-report.html"))
    conflict_plan = timber("plan-conflict", "plan", "--target", target_id, "--source", a_id, "--source", conflict_id, "--source", b_id)["result"]
    conflict_plan_path = output / "plan-conflict.json"
    write(conflict_plan_path, json.dumps(conflict_plan, indent=2))
    conflict_result = timber("preview-conflict", "preview", str(conflict_plan_path), "--to", str(output / "preview-conflict"), expected=1)
    assert conflict_result["result"]["status"] == "conflicted"
    assert conflict_result["result"]["completed_sources"] == [a_id]
    assert conflict_result["result"]["pending_sources"] == [conflict_id, b_id]
    assert conflict_result["result"]["conflict_records"]
    assert (output / "preview-conflict/shared.txt").read_text() == "target shared change\n"
    timber("conflict-report", "report", str(results / "preview-conflict.json"), "--to", str(output / "conflict-report.html"))
    after = [snapshot(path) for path in roots]
    assert before == after, "Timber changed synthetic source files, refs, indexes, or configuration"
    assert folder_before == {p.name: hashlib.sha256(p.read_bytes()).hexdigest() for p in old_folder.iterdir()}
    write(results / "source-invariance.json", json.dumps({"checked": True, "sources": 4, "checks": ["file bytes and modes", "index bytes", "HEAD and branch", "shared references", "repository configuration", "folder payload bytes"], "matched": True}, indent=2))
    # This explicit fixture edit tests stale-source detection; it is not a Timber action.
    write(agent_a / "a.txt", "source changed after capture\n")
    assessment = timber("source-drift", "assess", str(selected_plan), expected=1)
    assert "source_contents_changed_since_checkpoint" in json.dumps(assessment)
    timber("drift-report", "report", str(results / "source-drift.json"), "--to", str(output / "drift-report.html"))
    # Make the original Git directories and linked-worktree pointers unavailable.
    # This is an explicit synthetic-fixture move, never a Timber cleanup operation.
    fixtures.rename(output / "fixtures-offline")
    timber("a-verify-offline", "verify", a_id)
    timber("preview-reproduced", "preview", str(selected_plan), "--to", str(output / "preview-reproduced"))
    assert git(preview_path, "rev-parse", "HEAD^{tree}") == git(output / "preview-reproduced", "rev-parse", "HEAD^{tree}")
    corrupt_store = output / "store-corrupt"
    shutil.copytree(store, corrupt_store)
    a_index = a_capture["result"]["checkpoint"]["git"]["index"]
    corrupt_digest = next(entry["blob"] for entry in a_index if entry["path"] == "a.txt")
    (corrupt_store / "objects" / corrupt_digest).write_bytes(b"deliberately corrupted synthetic copy")
    refused = timber("corrupt-verification", "verify", a_id, expected=4, selected_store=corrupt_store)
    assert refused["error"]["code"] == "OPERATION_FAILED"
    assert "hash verification" in refused["error"]["message"]
    try:
        revision = git(PROJECT, "rev-parse", "HEAD").decode().strip()
    except subprocess.CalledProcessError:
        revision = "uncommitted-initial-implementation"
    store_bytes = sum(p.stat().st_size for p in store.rglob("*") if p.is_file())
    metrics = {
        "schema": 1, "fixture": "Timber synthetic local CLI demonstration", "source_revision_at_run": revision,
        "binary_sha256": binary_sha256, "original_sources_unavailable_for_reproduced_preview": True,
        "timber_version": subprocess.check_output([str(binary), "--version"], env=env).decode().strip(),
        "git_version": subprocess.check_output(["git", "--version"], env=env).decode().strip(),
        "platform": os.uname().sysname + " " + os.uname().release,
        "timber_invocations": len(calls), "command_elapsed_seconds": round(sum(c["elapsed_seconds"] for c in calls), 6),
        "checkpoint_store_bytes": store_bytes, "checkpoints": 5,
        "source_invariance_verified_before_explicit_drift": True, "preview_tree_reproduced_after_drift": True,
        "successful_preview": True, "conflict_refusal_verified": True, "corruption_refusal_verified": True,
        "independent_backup": "unassessed", "application_correctness": "not_tested", "comparative_savings": "unmeasured", "calls": calls,
    }
    write(output / "metrics.json", json.dumps(metrics, indent=2))
    print(json.dumps({"status": "passed", "output": str(output), "timber_invocations": len(calls), "command_elapsed_seconds": metrics["command_elapsed_seconds"], "checkpoint_store_bytes": store_bytes, "reports": ["complete-report.html", "conflict-report.html", "drift-report.html"]}, indent=2))


if __name__ == "__main__":
    main()
