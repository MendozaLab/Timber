//! Team convention file: `timber.toml` at the repository root.
//!
//! The file is committed so every person and agent on a repository uses the same
//! store, exclusion list and Git timeout. Only four keys exist: `store`, `exclude`,
//! `git_timeout_seconds` and `schedule`. Any other key is refused, naming the key.
//! `schedule` is a note for people; Timber never schedules anything.
//!
//! No TOML library is a dependency of Timber, so this module reads the small subset
//! of TOML the four keys need: `#` comments, bare keys, basic and literal
//! single-line strings, decimal integers and arrays of strings (which may span
//! lines). Tables, dotted keys, multi-line strings and every other value type are
//! refused with the line named, never guessed.
use crate::{fsx, git};
use anyhow::{Context, Result, bail, ensure};
use serde::Serialize;
use std::{
    fs,
    io::Write,
    os::unix::fs::OpenOptionsExt,
    path::{Path, PathBuf},
};

/// File name of the convention file at the repository root.
pub const FILE_NAME: &str = "timber.toml";
/// The only keys a convention file may hold.
pub const KEYS: [&str; 4] = ["store", "exclude", "git_timeout_seconds", "schedule"];
/// Largest convention file Timber reads. A team convention is a few lines.
const MAX_BYTES: u64 = 64 * 1024;

/// Commented template written by `timber init`. Every key is commented out, so the
/// template on its own changes nothing.
pub const TEMPLATE: &str = r#"# timber.toml: team convention for Timber on this repository.
# Commit this file so every person and agent uses the same settings.
# Every key is optional. Unknown keys are refused, naming the key.
# Command-line flags add to or override these values for one run.
# `timber inspect` and `timber checkpoint` print whether this file was read.

# Checkpoint store, outside every source repository and preview. An absolute
# path, or one that starts with an environment-variable reference such as
# "$HOME/timber-store" or "${TEAM_TIMBER_STORE}". --store wins when given.
# store = "$HOME/timber-store"

# Untracked or ignored paths, relative to the repository root, that every
# checkpoint of this repository skips as if passed with --exclude. Each one is
# recorded in the manifest as excluded_by_convention_file. Tracked paths, paths
# that match nothing and paths reached through a symbolic link are refused.
# exclude = [
#     "test-output",
#     "build/tmp",
# ]

# Limit in seconds for each single Git operation (1 to 86400; default 120).
# --git-timeout wins when given.
# git_timeout_seconds = 300

# Free-text note on when the team checkpoints. Timber never schedules anything.
# schedule = "before each merge to main"
"#;

/// Values read from a convention file, exactly as written.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct Values {
    pub store: Option<String>,
    pub exclude: Vec<String>,
    pub git_timeout_seconds: Option<u64>,
    pub schedule: Option<String>,
}

/// The result of looking for a convention file. `found` is false when no file
/// exists at `file`; the values are then empty.
#[derive(Debug, Clone, Serialize)]
pub struct Convention {
    pub file: PathBuf,
    pub found: bool,
    pub values: Values,
}

/// A convention exclusion dropped for one run because the command line overrode it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Overridden {
    pub path: String,
    pub by: String,
}

/// Repository root that holds the convention file for `path`.
pub fn root(path: &Path) -> Result<PathBuf> {
    let top = git::text(path, &["rev-parse", "--show-toplevel"])
        .with_context(|| format!("not a Git worktree: {}", path.display()))?;
    Ok(PathBuf::from(top))
}

/// Reads `timber.toml` at `root`. A missing file is not an error; a symbolic link,
/// a non-regular file, an oversized or non-UTF-8 file, or any content outside the
/// supported subset is refused, naming the file.
pub fn load(root: &Path) -> Result<Convention> {
    let file = root.join(FILE_NAME);
    let metadata = match fs::symlink_metadata(&file) {
        Ok(metadata) => metadata,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Ok(Convention {
                file,
                found: false,
                values: Values::default(),
            });
        }
        Err(e) => {
            return Err(e)
                .with_context(|| format!("convention file unreadable: {}", file.display()));
        }
    };
    ensure!(
        !metadata.file_type().is_symlink(),
        "convention file is a symbolic link; refused: {}",
        file.display()
    );
    ensure!(
        metadata.is_file() && !fsx::is_placeholder(&metadata),
        "convention file is not a regular local file; refused: {}",
        file.display()
    );
    ensure!(
        metadata.len() <= MAX_BYTES,
        "convention file is larger than {MAX_BYTES} bytes; refused: {}",
        file.display()
    );
    let bytes = fs::read(&file)
        .with_context(|| format!("convention file unreadable: {}", file.display()))?;
    let text = String::from_utf8(bytes)
        .with_context(|| format!("convention file is not UTF-8: {}", file.display()))?;
    let values =
        parse(&text).with_context(|| format!("convention file refused: {}", file.display()))?;
    Ok(Convention {
        file,
        found: true,
        values,
    })
}

/// Writes the commented template to `root/timber.toml`. Never overwrites: an existing
/// file, symbolic link or other entry at that name is refused, naming it.
pub fn init(root: &Path) -> Result<PathBuf> {
    let file = root.join(FILE_NAME);
    let mut handle = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o644)
        .open(&file)
        .map_err(|e| {
            if e.kind() == std::io::ErrorKind::AlreadyExists {
                anyhow::anyhow!(
                    "convention file already exists; init never overwrites it: {}",
                    file.display()
                )
            } else {
                anyhow::Error::new(e).context(format!("create convention file {}", file.display()))
            }
        })?;
    handle.write_all(TEMPLATE.as_bytes())?;
    handle.sync_all()?;
    fs::File::open(root)?.sync_all()?;
    Ok(file)
}

/// Resolves the `store` value: an absolute path, or one that starts with a single
/// environment-variable reference (`$NAME` or `${NAME}`). An unset or empty variable,
/// a relative result, or a `$` anywhere but the start is refused.
pub fn resolve_store(raw: &str) -> Result<PathBuf> {
    let (head, rest) = if let Some(after) = raw.strip_prefix("${") {
        let end = after
            .find('}')
            .with_context(|| format!("timber.toml store has an unclosed ${{ reference: {raw}"))?;
        (Some(&after[..end]), &after[end + 1..])
    } else if let Some(after) = raw.strip_prefix('$') {
        let end = after
            .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
            .unwrap_or(after.len());
        (Some(&after[..end]), &after[end..])
    } else {
        (None, raw)
    };
    ensure!(
        !rest.contains('$'),
        "timber.toml store may hold one environment-variable reference, only at the start: {raw}"
    );
    let path = match head {
        Some(name) => {
            ensure!(
                !name.is_empty() && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_'),
                "timber.toml store has an invalid environment-variable name: {raw}"
            );
            let value = std::env::var_os(name)
                .filter(|v| !v.is_empty())
                .with_context(|| {
                    format!(
                        "timber.toml store refers to environment variable {name}, which is not set"
                    )
                })?;
            ensure!(
                rest.is_empty() || rest.starts_with('/'),
                "timber.toml store must continue with '/' after the environment-variable reference: {raw}"
            );
            let mut joined = value;
            joined.push(rest);
            PathBuf::from(joined)
        }
        None => PathBuf::from(rest),
    };
    ensure!(
        path.is_absolute(),
        "timber.toml store must resolve to an absolute path: {raw} resolved to {}",
        path.display()
    );
    Ok(path)
}

/// True when `child` is `parent` or lies below it.
fn covers(parent: &str, child: &str) -> bool {
    child == parent
        || child
            .strip_prefix(parent)
            .is_some_and(|rest| rest.starts_with('/'))
}

/// Removes convention exclusions that the command line overrides for this run. A
/// convention exclusion yields when a command-line `--exclude` names the same path
/// or a parent of it, or when an `--include-ignored` selection overlaps it in either
/// direction. A command-line `--exclude` strictly inside a convention exclusion is
/// refused, naming both: the convention already skips it, and narrowing a team
/// convention is an edit to the file.
pub fn merge_exclusions(
    convention: &[String],
    operator: &[String],
    include_ignored: &[String],
) -> Result<(Vec<String>, Vec<Overridden>)> {
    let norm = |s: &String| s.trim_end_matches('/').to_owned();
    let mut kept = Vec::new();
    let mut overridden = Vec::new();
    'entries: for entry in convention {
        let entry = norm(entry);
        for flag in operator.iter().map(norm) {
            if covers(&flag, &entry) {
                overridden.push(Overridden {
                    path: entry.clone(),
                    by: format!("--exclude {flag}"),
                });
                continue 'entries;
            }
            ensure!(
                !covers(&entry, &flag),
                "--exclude {flag} lies inside the timber.toml exclusion {entry}, which already skips it; edit timber.toml to narrow the convention"
            );
        }
        for selection in include_ignored.iter().map(norm) {
            if covers(&selection, &entry) || covers(&entry, &selection) {
                overridden.push(Overridden {
                    path: entry.clone(),
                    by: format!("--include-ignored {selection}"),
                });
                continue 'entries;
            }
        }
        kept.push(entry);
    }
    Ok((kept, overridden))
}

/// Parses the supported subset of TOML. Errors name the line and, for an unknown
/// key, the key itself.
pub fn parse(text: &str) -> Result<Values> {
    let mut p = Parser {
        text,
        pos: 0,
        values: Values::default(),
        seen: Vec::new(),
    };
    p.document()?;
    validate(&p.values)?;
    Ok(p.values)
}

fn validate(values: &Values) -> Result<()> {
    if let Some(store) = &values.store {
        ensure!(!store.is_empty(), "store must not be empty");
    }
    let mut seen: Vec<String> = Vec::new();
    for raw in &values.exclude {
        let path = raw.trim_end_matches('/').to_owned();
        fsx::validate_relative(&path).with_context(|| format!("invalid exclude path: {raw:?}"))?;
        for earlier in &seen {
            ensure!(
                !covers(earlier, &path) && !covers(&path, earlier),
                "exclude paths repeat or overlap: {earlier} and {path}"
            );
        }
        seen.push(path);
    }
    if let Some(seconds) = values.git_timeout_seconds {
        ensure!(
            (1..=86_400).contains(&seconds),
            "git_timeout_seconds must be from 1 to 86400, found {seconds}"
        );
    }
    Ok(())
}

struct Parser<'a> {
    text: &'a str,
    pos: usize,
    values: Values,
    seen: Vec<String>,
}

impl Parser<'_> {
    fn line(&self) -> usize {
        self.text[..self.pos].matches('\n').count() + 1
    }
    fn peek(&self) -> Option<char> {
        self.text[self.pos..].chars().next()
    }
    fn bump(&mut self) -> Option<char> {
        let c = self.peek()?;
        self.pos += c.len_utf8();
        Some(c)
    }
    fn at_crlf(&self) -> bool {
        self.text[self.pos..].starts_with("\r\n")
    }
    fn skip_spaces(&mut self) {
        while matches!(self.peek(), Some(' ' | '\t')) {
            self.pos += 1;
        }
    }
    fn skip_comment(&mut self) {
        if self.peek() == Some('#') {
            while !matches!(self.peek(), None | Some('\n')) && !self.at_crlf() {
                self.bump();
            }
        }
    }
    /// Skips whitespace, newlines and comments between items.
    fn skip_blank(&mut self) {
        loop {
            self.skip_spaces();
            self.skip_comment();
            if self.peek() == Some('\n') {
                self.pos += 1;
            } else if self.at_crlf() {
                self.pos += 2;
            } else {
                return;
            }
        }
    }
    fn end_of_line(&mut self) -> Result<()> {
        self.skip_spaces();
        self.skip_comment();
        match self.peek() {
            None | Some('\n') => Ok(()),
            Some(_) if self.at_crlf() => Ok(()),
            Some(c) => bail!("line {}: unexpected {c:?} after the value", self.line()),
        }
    }
    fn document(&mut self) -> Result<()> {
        loop {
            self.skip_blank();
            let Some(first) = self.peek() else {
                return Ok(());
            };
            let line = self.line();
            ensure!(
                first != '[',
                "line {line}: tables are not supported; timber.toml holds only the top-level keys {}",
                KEYS.join(", ")
            );
            let start = self.pos;
            while !matches!(
                self.peek(),
                None | Some(' ' | '\t' | '=' | '\n' | '\r' | '#')
            ) {
                self.bump();
            }
            let key = self.text[start..self.pos].to_owned();
            ensure!(!key.is_empty(), "line {line}: expected a key");
            ensure!(
                KEYS.contains(&key.as_str()),
                "unknown key in timber.toml: {key} (line {line}); supported keys are {}",
                KEYS.join(", ")
            );
            ensure!(
                !self.seen.contains(&key),
                "line {line}: key {key} is given more than once"
            );
            self.seen.push(key.clone());
            self.skip_spaces();
            ensure!(
                self.bump() == Some('='),
                "line {line}: expected '=' after {key}"
            );
            self.skip_spaces();
            match key.as_str() {
                "store" => self.values.store = Some(self.string(&key)?),
                "schedule" => self.values.schedule = Some(self.string(&key)?),
                "git_timeout_seconds" => self.values.git_timeout_seconds = Some(self.integer()?),
                "exclude" => self.values.exclude = self.array(&key)?,
                _ => unreachable!("key checked against KEYS"),
            }
            self.end_of_line()?;
        }
    }
    fn string(&mut self, key: &str) -> Result<String> {
        let line = self.line();
        let rest = &self.text[self.pos..];
        ensure!(
            !rest.starts_with("\"\"\"") && !rest.starts_with("'''"),
            "line {line}: multi-line strings are not supported"
        );
        let quote = match self.bump() {
            Some(q @ ('"' | '\'')) => q,
            _ => bail!("line {line}: {key} must be a quoted string"),
        };
        let mut out = String::new();
        loop {
            match self.bump() {
                None | Some('\n' | '\r') => bail!("line {line}: unterminated string for {key}"),
                Some(c) if c == quote => return Ok(out),
                Some('\\') if quote == '"' => out.push(self.escape(line)?),
                Some(c) if c.is_control() && c != '\t' => {
                    bail!("line {line}: control character in string for {key}")
                }
                Some(c) => out.push(c),
            }
        }
    }
    fn escape(&mut self, line: usize) -> Result<char> {
        Ok(match self.bump() {
            Some('"') => '"',
            Some('\\') => '\\',
            Some('n') => '\n',
            Some('t') => '\t',
            Some('r') => '\r',
            Some(kind @ ('u' | 'U')) => {
                let width = if kind == 'u' { 4 } else { 8 };
                let digits = self
                    .text
                    .get(self.pos..self.pos + width)
                    .filter(|d| d.chars().all(|c| c.is_ascii_hexdigit()))
                    .with_context(|| format!("line {line}: invalid unicode escape"))?;
                self.pos += width;
                u32::from_str_radix(digits, 16)
                    .ok()
                    .and_then(char::from_u32)
                    .with_context(|| format!("line {line}: invalid unicode escape"))?
            }
            _ => bail!("line {line}: unsupported escape sequence"),
        })
    }
    fn integer(&mut self) -> Result<u64> {
        let line = self.line();
        let start = self.pos;
        while matches!(self.peek(), Some('0'..='9' | '_')) {
            self.pos += 1;
        }
        let raw = &self.text[start..self.pos];
        ensure!(
            !raw.is_empty()
                && !raw.starts_with('_')
                && !raw.ends_with('_')
                && !raw.contains("__")
                && !matches!(self.peek(), Some('.' | 'e' | 'E')),
            "line {line}: git_timeout_seconds must be a whole number of seconds"
        );
        raw.replace('_', "")
            .parse()
            .with_context(|| format!("line {line}: git_timeout_seconds is out of range"))
    }
    fn array(&mut self, key: &str) -> Result<Vec<String>> {
        let line = self.line();
        ensure!(
            self.bump() == Some('['),
            "line {line}: {key} must be an array of quoted paths"
        );
        let mut items = Vec::new();
        loop {
            self.skip_blank();
            if self.peek() == Some(']') {
                self.pos += 1;
                return Ok(items);
            }
            ensure!(
                self.peek().is_some(),
                "line {line}: unterminated array for {key}"
            );
            items.push(self.string(key)?);
            self.skip_blank();
            match self.bump() {
                Some(',') => {}
                Some(']') => return Ok(items),
                None => bail!("line {line}: unterminated array for {key}"),
                _ => bail!(
                    "line {}: expected ',' or ']' in the {key} array",
                    self.line()
                ),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn template_parses_to_no_settings() {
        assert_eq!(parse(TEMPLATE).unwrap(), Values::default());
    }

    #[test]
    fn all_keys_parse() {
        let values = parse(
            "# team\nstore = \"/srv/timber\" # shared\nexclude = [\n  \"a\", # first\n  'b/c/',\n]\ngit_timeout_seconds = 1_200\nschedule = \"daily \\\"ish\\\"\"\n",
        )
        .unwrap();
        assert_eq!(values.store.as_deref(), Some("/srv/timber"));
        assert_eq!(values.exclude, ["a", "b/c/"]);
        assert_eq!(values.git_timeout_seconds, Some(1200));
        assert_eq!(values.schedule.as_deref(), Some("daily \"ish\""));
        let crlf = parse("exclude = [\"a\",\r\n \"b\"]\r\nschedule = 'x' # c\r\n").unwrap();
        assert_eq!(crlf.exclude, ["a", "b"]);
    }

    #[test]
    fn refusals_name_the_problem() {
        let cases = [
            ("color = \"blue\"\n", "unknown key in timber.toml: color"),
            (
                "store.path = \"/x\"\n",
                "unknown key in timber.toml: store.path",
            ),
            ("[store]\n", "tables are not supported"),
            ("store = \"/a\"\nstore = \"/b\"\n", "more than once"),
            ("store = 5\n", "must be a quoted string"),
            ("git_timeout_seconds = 0\n", "from 1 to 86400"),
            ("git_timeout_seconds = 1.5\n", "whole number"),
            ("git_timeout_seconds = \"9\"\n", "whole number"),
            ("exclude = \"a\"\n", "array of quoted paths"),
            (
                "exclude = [\"a\", \"a/b\"]\n",
                "repeat or overlap: a and a/b",
            ),
            ("exclude = [\"../x\"]\n", "invalid exclude path"),
            ("exclude = [\"/abs\"]\n", "invalid exclude path"),
            ("exclude = [\"a\"\n", "unterminated array"),
            ("schedule = \"\"\"x\"\"\"\n", "multi-line strings"),
            ("store = \"/a\" extra\n", "after the value"),
        ];
        for (text, expected) in cases {
            let message = format!("{:#}", parse(text).unwrap_err());
            assert!(message.contains(expected), "{text:?}: {message}");
        }
    }

    #[test]
    fn store_references_resolve_or_refuse() {
        assert_eq!(resolve_store("/srv/t").unwrap(), PathBuf::from("/srv/t"));
        let home = std::env::var("HOME").unwrap();
        assert_eq!(
            resolve_store("$HOME/t").unwrap(),
            PathBuf::from(format!("{home}/t"))
        );
        assert_eq!(
            resolve_store("${HOME}/t").unwrap(),
            PathBuf::from(format!("{home}/t"))
        );
        for (raw, expected) in [
            ("relative/store", "absolute path"),
            (
                "$TIMBER_CONVENTION_TEST_UNSET_VARIABLE/x",
                "which is not set",
            ),
            ("/a/$HOME", "only at the start"),
            ("$HOMEx", "not set"),
            ("${HOME", "unclosed"),
        ] {
            let message = format!("{:#}", resolve_store(raw).unwrap_err());
            assert!(message.contains(expected), "{raw}: {message}");
        }
    }

    #[test]
    fn command_line_overrides_convention_exclusions() {
        let file = vec!["a".to_owned(), "b/c".to_owned(), "d".to_owned()];
        let (kept, overridden) =
            merge_exclusions(&file, &["b".to_owned()], &["d/e".to_owned()]).unwrap();
        assert_eq!(kept, ["a"]);
        assert_eq!(
            overridden,
            [
                Overridden {
                    path: "b/c".into(),
                    by: "--exclude b".into()
                },
                Overridden {
                    path: "d".into(),
                    by: "--include-ignored d/e".into()
                },
            ]
        );
        let error = merge_exclusions(&file, &["a/x".to_owned()], &[]).unwrap_err();
        assert!(format!("{error:#}").contains("inside the timber.toml exclusion a"));
    }
}
