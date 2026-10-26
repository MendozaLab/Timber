//! Git subprocesses with inherited helpers and networking disabled.
use anyhow::{Context, Result, bail};
use std::io::{Read, Write};
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Command, Output, Stdio};
use std::time::Duration;
use wait_timeout::ChildExt;

pub fn output(repo: &Path, args: &[&str], input: Option<&[u8]>) -> Result<Output> {
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
