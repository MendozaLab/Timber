//! Git subprocesses with inherited helpers and networking disabled.
use anyhow::{Context, Result, bail, ensure};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Command, Output, Stdio};
use std::time::Duration;
use wait_timeout::ChildExt;

fn configured_command(repo: &Path, args: &[&str]) -> Command {
    let mut command = Command::new("git");
    command.process_group(0);
    command.current_dir(repo);
    for (key, _) in std::env::vars_os() {
        if key.to_string_lossy().starts_with("GIT_") {
            command.env_remove(key);
        }
    }
    command.envs([
        ("GIT_CONFIG_NOSYSTEM", "1"),
        ("GIT_CONFIG_SYSTEM", "/dev/null"),
        ("GIT_CONFIG_GLOBAL", "/dev/null"),
        ("GIT_ATTR_NOSYSTEM", "1"),
        ("GIT_OPTIONAL_LOCKS", "0"),
        ("GIT_NO_LAZY_FETCH", "1"),
        ("GIT_NO_REPLACE_OBJECTS", "1"),
        ("GIT_TERMINAL_PROMPT", "0"),
        ("LC_ALL", "C"),
    ]);
    for setting in [
        "core.fsmonitor=false",
        "core.hooksPath=/dev/null",
        "core.attributesFile=/dev/null",
        "core.untrackedCache=false",
        "credential.helper=",
        "commit.gpgSign=false",
        "tag.gpgSign=false",
        "gc.auto=0",
        "maintenance.auto=false",
        "protocol.allow=never",
        "rerere.enabled=false",
        "merge.renormalize=false",
        "core.autocrlf=false",
        "core.quotePath=false",
        "core.pager=cat",
    ] {
        command.args(["-c", setting]);
    }
    command
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    command
}

pub fn output(repo: &Path, args: &[&str], input: Option<&[u8]>) -> Result<Output> {
    let mut command = configured_command(repo, args);
    let mut child = command.spawn().context("could not start Git")?;
    let mut stdout = child.stdout.take().context("missing Git stdout")?;
    let mut stderr = child.stderr.take().context("missing Git stderr")?;
    let mut stdin = child.stdin.take().context("missing Git stdin")?;
    let bytes = input.unwrap_or_default().to_vec();
    let writer = std::thread::spawn(move || stdin.write_all(&bytes));
    let out = std::thread::spawn(move || {
        let mut b = Vec::new();
        stdout.read_to_end(&mut b).map(|_| b)
    });
    let err = std::thread::spawn(move || {
        let mut b = Vec::new();
        stderr.read_to_end(&mut b).map(|_| b)
    });
    let status = child.wait_timeout(Duration::from_secs(120))?;
    let timed_out = status.is_none();
    if timed_out {
        // SAFETY: a new process group was created for this owned child. Negative
        // pid signals only that group, including pack-objects children holding pipes.
        if unsafe { libc::kill(-(child.id() as i32), libc::SIGKILL) } != 0 {
            child.kill().context("could not stop timed-out Git child")?;
        }
    }
    let status = status.unwrap_or(child.wait()?);
    let stdout = out
        .join()
        .map_err(|_| anyhow::anyhow!("Git reader failed"))??;
    let stderr = err
        .join()
        .map_err(|_| anyhow::anyhow!("Git error reader failed"))??;
    let write_result = writer
        .join()
        .map_err(|_| anyhow::anyhow!("Git input writer failed"))?;
    if timed_out {
        bail!("Git operation exceeded 120 seconds; source was not changed");
    }
    if status.success() {
        write_result.context("writing Git input")?;
    }
    Ok(Output {
        status,
        stdout,
        stderr,
    })
}

pub fn run(repo: &Path, args: &[&str]) -> Result<Vec<u8>> {
    run_input(repo, args, &[])
}
pub fn run_input(repo: &Path, args: &[&str], input: &[u8]) -> Result<Vec<u8>> {
    let result = output(repo, args, Some(input))?;
    if !result.status.success() {
        bail!(
            "Git {} failed ({}): {}",
            args.first().unwrap_or(&"operation"),
            result.status,
            String::from_utf8_lossy(&result.stderr).trim()
        );
    }
    Ok(result.stdout)
}
pub fn text(repo: &Path, args: &[&str]) -> Result<String> {
    Ok(String::from_utf8(run(repo, args)?)
        .context("Git returned a non-UTF-8 identity")?
        .trim_end()
        .to_owned())
}
pub fn init(path: &Path) -> Result<()> {
    run(
        path,
        &[
            "init",
            "--quiet",
            "--template=",
            "--initial-branch=timber-preview",
        ],
    )?;
    Ok(())
}

/// All source checkouts and shared metadata remain outside Timber write destinations.
pub fn protected_roots(repo: &Path) -> Result<Vec<std::path::PathBuf>> {
    let mut roots = Vec::new();
    let data = run(repo, &["worktree", "list", "--porcelain", "-z"])?;
    for item in data.split(|b| *b == 0) {
        if let Some(value) = item.strip_prefix(b"worktree ") {
            roots.push(std::path::PathBuf::from(std::str::from_utf8(value)?));
        }
    }
    let common = text(
        repo,
        &["rev-parse", "--path-format=absolute", "--git-common-dir"],
    )?;
    roots.push(common.into());
    roots.sort();
    roots.dedup();
    Ok(roots)
}

fn object_ids(oids: &[String]) -> Result<Vec<String>> {
    let unique: BTreeSet<_> = oids.iter().cloned().collect();
    for oid in &unique {
        ensure!(
            [40, 64].contains(&oid.len()) && oid.bytes().all(|b| b.is_ascii_hexdigit()),
            "invalid exact Git object identity"
        );
    }
    Ok(unique.into_iter().collect())
}

/// One bounded cat-file process, streaming every blob through a fixed-size buffer.
pub fn batch_blob_fingerprints(
    repo: &Path,
    oids: &[String],
) -> Result<BTreeMap<String, (String, u64)>> {
    let unique = object_ids(oids)?;
    if unique.is_empty() {
        return Ok(BTreeMap::new());
    }
    let input = (unique.join("\n") + "\n").into_bytes();
    let mut child = configured_command(repo, &["cat-file", "--batch"]).spawn()?;
    let mut stdin = child.stdin.take().context("missing batch input")?;
    let stdout = child.stdout.take().context("missing batch output")?;
    let mut stderr = child.stderr.take().context("missing batch errors")?;
    let writer = std::thread::spawn(move || stdin.write_all(&input));
    let errors = std::thread::spawn(move || {
        let mut bytes = Vec::new();
        stderr.read_to_end(&mut bytes).map(|_| bytes)
    });
    let reader = std::thread::spawn(move || -> Result<BTreeMap<String, (String, u64)>> {
        let mut reader = BufReader::new(stdout);
        let mut result = BTreeMap::new();
        for expected in unique {
            let mut header = Vec::new();
            ensure!(
                reader.read_until(b'\n', &mut header)? > 0,
                "truncated Git batch header"
            );
            let fields = std::str::from_utf8(&header)?
                .split_whitespace()
                .collect::<Vec<_>>();
            ensure!(
                fields.len() == 3 && fields[0] == expected && fields[1] == "blob",
                "missing or unsupported Git blob: {expected}"
            );
            let size: u64 = fields[2].parse()?;
            let mut remaining = size;
            let mut hash = Sha256::new();
            let mut buffer = [0u8; 65536];
            while remaining > 0 {
                let count = remaining.min(buffer.len() as u64) as usize;
                reader
                    .read_exact(&mut buffer[..count])
                    .context("truncated Git batch blob")?;
                hash.update(&buffer[..count]);
                remaining -= count as u64;
            }
            let mut separator = [0u8; 1];
            reader.read_exact(&mut separator)?;
            ensure!(separator == *b"\n", "invalid Git batch delimiter");
            result.insert(expected, (hex::encode(hash.finalize()), size));
        }
        Ok(result)
    });
    let status = child.wait_timeout(Duration::from_secs(120))?;
    let timed_out = status.is_none();
    if timed_out {
        // SAFETY: this invocation owns its new process group and Git children.
        if unsafe { libc::kill(-(child.id() as i32), libc::SIGKILL) } != 0 {
            let _ = child.kill();
        }
    }
    let status = match status {
        Some(s) => s,
        None => child.wait()?,
    };
    let result = reader
        .join()
        .map_err(|_| anyhow::anyhow!("Git batch reader failed"))?;
    let errors = errors
        .join()
        .map_err(|_| anyhow::anyhow!("Git batch error reader failed"))??;
    let written = writer
        .join()
        .map_err(|_| anyhow::anyhow!("Git batch writer failed"))?;
    ensure!(!timed_out, "Git batch exceeded 120 seconds");
    ensure!(
        status.success(),
        "Git batch failed: {}",
        String::from_utf8_lossy(&errors)
    );
    written?;
    result
}

/// Consume one blob at a time from one Git process, without buffering the whole object set.
pub fn for_each_blob<F>(repo: &Path, oids: &[String], mut consume: F) -> Result<()>
where
    F: FnMut(&str, Vec<u8>) -> Result<()> + Send,
{
    let unique = object_ids(oids)?;
    if unique.is_empty() {
        return Ok(());
    }
    let input = (unique.join("\n") + "\n").into_bytes();
    let mut child = configured_command(repo, &["cat-file", "--batch"]).spawn()?;
    let mut stdin = child.stdin.take().context("missing batch input")?;
    let stdout = child.stdout.take().context("missing batch output")?;
    let mut stderr = child.stderr.take().context("missing batch errors")?;
    std::thread::scope(|scope| -> Result<()> {
        let writer = scope.spawn(move || stdin.write_all(&input));
        let errors = scope.spawn(move || {
            let mut bytes = Vec::new();
            stderr.read_to_end(&mut bytes).map(|_| bytes)
        });
        let reader = scope.spawn(move || -> Result<()> {
            let mut reader = BufReader::new(stdout);
            for expected in unique {
                let mut header = Vec::new();
                ensure!(
                    reader.read_until(b'\n', &mut header)? > 0,
                    "truncated Git batch header"
                );
                let fields = std::str::from_utf8(&header)?
                    .split_whitespace()
                    .collect::<Vec<_>>();
                ensure!(
                    fields.len() == 3 && fields[0] == expected && fields[1] == "blob",
                    "missing or unsupported Git blob: {expected}"
                );
                let size: usize = fields[2].parse()?;
                let mut bytes = Vec::new();
                bytes
                    .try_reserve_exact(size)
                    .context("Git blob exceeds available memory")?;
                bytes.resize(size, 0);
                reader.read_exact(&mut bytes)?;
                let mut separator = [0];
                reader.read_exact(&mut separator)?;
                ensure!(separator == *b"\n", "invalid Git batch delimiter");
                consume(&expected, bytes)?;
            }
            Ok(())
        });
        let status = child.wait_timeout(Duration::from_secs(120))?;
        let timed_out = status.is_none();
        if timed_out {
            // SAFETY: this invocation owns the new Git process group.
            if unsafe { libc::kill(-(child.id() as i32), libc::SIGKILL) } != 0 {
                let _ = child.kill();
            }
        }
        let status = match status {
            Some(s) => s,
            None => child.wait()?,
        };
        let result = reader
            .join()
            .map_err(|_| anyhow::anyhow!("Git blob consumer failed"))?;
        let errors = errors
            .join()
            .map_err(|_| anyhow::anyhow!("Git batch error reader failed"))??;
        let written = writer
            .join()
            .map_err(|_| anyhow::anyhow!("Git batch writer failed"))?;
        ensure!(!timed_out, "Git batch exceeded 120 seconds");
        ensure!(
            status.success(),
            "Git batch failed: {}",
            String::from_utf8_lossy(&errors)
        );
        written?;
        result
    })
}

/// Raw blobs for bounded recovery operations. Inspection uses streaming fingerprints.
pub fn batch_blobs(repo: &Path, oids: &[String]) -> Result<BTreeMap<String, Vec<u8>>> {
    let mut result = BTreeMap::new();
    for_each_blob(repo, oids, |oid, bytes| {
        result.insert(oid.to_owned(), bytes);
        Ok(())
    })?;
    Ok(result)
}
