use anyhow::{Context, Result, ensure};
use clap::{Parser, Subcommand};
use serde::Serialize;
use serde_json::{Value, json};
use std::os::unix::fs::OpenOptionsExt;
use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
};
use timber_worktrees::{
    capture,
    convention::{self, Convention, Overridden},
    fsx, inspect,
    model::{ImportMap, Layer, Plan, Selection},
    planning, preview, report, restore,
    store::Store,
};

#[derive(Parser)]
#[command(
    name = "timber",
    version,
    about = "Untangle dirty worktrees. Keep the useful work."
)]
struct Cli {
    /// Checkpoint store outside source repositories. Wins over the repository's
    /// timber.toml `store`, then TIMBER_STORE, then the OS local data directory.
    #[arg(long, global = true)]
    store: Option<PathBuf>,
    /// Emit machine-readable results.
    #[arg(long, global = true)]
    json: bool,
    /// Save the complete result (or editable consolidation plan) to a new JSON file.
    #[arg(long, global = true)]
    out: Option<PathBuf>,
    /// Limit in seconds for each single Git operation (1 to 86400; default 120). Wins
    /// over the repository's timber.toml `git_timeout_seconds`. A timeout leaves the
    /// source untouched and the result partial, naming the operation.
    #[arg(
        long,
        global = true,
        value_name = "SECONDS",
        value_parser = clap::value_parser!(u64).range(1..=86_400)
    )]
    git_timeout: Option<u64>,
    #[command(subcommand)]
    command: Commands,
}
#[derive(Subcommand)]
enum Commands {
    /// Write a commented timber.toml template at the worktree root. Never overwrites.
    Init {
        #[arg(default_value = ".")]
        worktree: PathBuf,
    },
    /// Inspect explicitly supplied repository and preservation roots. No source writes.
    Inspect {
        #[arg(long)]
        repo: PathBuf,
        #[arg(long)]
        folder: Vec<PathBuf>,
        /// Inspect all registered worktrees deeply; default inspects the selected repository only.
        #[arg(long)]
        all_worktrees: bool,
        /// Additional registered worktrees to inspect deeply.
        #[arg(long)]
        worktree: Vec<PathBuf>,
        /// Compare saved payload bytes and Git-backed tracked versions against the selected target.
        #[arg(long)]
        compare_folders: bool,
        /// List every ignored path; default reports count, bytes and top-level paths only.
        #[arg(long)]
        list_ignored: bool,
    },
    /// Capture supported Git and filesystem state. Does not stage or commit the source.
    Checkpoint {
        worktree: PathBuf,
        #[arg(long)]
        include_ignored: Vec<String>,
        /// Skip a disposable untracked or ignored path for this run only (repeatable,
        /// relative to the worktree). Recorded as excluded_by_operator; never remembered.
        /// Adds to the timber.toml `exclude` list and overrides any entry it covers.
        #[arg(long)]
        exclude: Vec<String>,
        #[arg(long)]
        writers_paused: bool,
    },
    /// Capture files present in an existing recovery folder; historical completeness stays unknown.
    Import {
        folder: PathBuf,
        #[arg(long)]
        map: Option<PathBuf>,
        #[arg(long)]
        writers_paused: bool,
        /// Explicit existing repository from which to preserve declared Git dependencies.
        #[arg(long, requires = "map")]
        dependency_repo: Option<PathBuf>,
    },
    Checkpoints {
        #[command(subcommand)]
        command: CheckpointCommands,
    },
    /// Independently restore and compare a checkpoint without needing the original repository.
    Verify { id: String },
    Restore {
        id: String,
        #[arg(long)]
        to: PathBuf,
    },
    /// Reconstruct one captured Git dependency in a fresh independent repository.
    RestoreGit {
        id: String,
        #[arg(long)]
        group: String,
        #[arg(long)]
        to: PathBuf,
    },
    /// Compare verified snapshots. Source order is preserved; edit layers and extras in plan JSON.
    Plan {
        #[arg(long)]
        target: String,
        #[arg(long, required = true)]
        source: Vec<String>,
        #[arg(long,default_value="working",value_parser=["working","index","committed"])]
        layer: String,
    },
    /// Build a consolidation preview in a fresh independent repository. Stops on conflict.
    Preview {
        plan: PathBuf,
        #[arg(long)]
        to: PathBuf,
    },
    /// Recheck current sources and explain blockers. Does not authorize deletion.
    Assess { plan: PathBuf },
    /// Render saved results as a self-contained local HTML report.
    Report {
        result: PathBuf,
        #[arg(long)]
        to: PathBuf,
    },
}
#[derive(Subcommand)]
enum CheckpointCommands {
    List,
    Show { id: String },
}

fn default_store() -> Result<(PathBuf, &'static str)> {
    if let Some(value) = std::env::var_os("TIMBER_STORE") {
        return Ok((value.into(), "environment"));
    }
    let home = std::env::var_os("HOME").context("HOME is not configured; pass --store")?;
    #[cfg(target_os = "macos")]
    {
        Ok((
            PathBuf::from(home).join("Library/Application Support/Timber"),
            "default",
        ))
    }
    #[cfg(not(target_os = "macos"))]
    {
        Ok((
            std::env::var_os("XDG_STATE_HOME")
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from(home).join(".local/state"))
                .join("timber"),
            "default",
        ))
    }
}
/// Store precedence: `--store`, then the convention file's `store`, then
/// TIMBER_STORE, then the OS local data directory. Returns where it came from.
fn store_for(cli: &Cli, found: Option<&Convention>) -> Result<(PathBuf, &'static str)> {
    if let Some(store) = &cli.store {
        return Ok((store.clone(), "command_line"));
    }
    if let Some(raw) = found.and_then(|c| c.values.store.as_deref()) {
        return Ok((convention::resolve_store(raw)?, "convention_file"));
    }
    default_store()
}
/// Git timeout precedence: `--git-timeout`, then the convention file, then 120.
fn timeout_for(cli: &Cli, found: &Convention) -> (u64, &'static str) {
    match (cli.git_timeout, found.values.git_timeout_seconds) {
        (Some(seconds), _) => (seconds, "command_line"),
        (None, Some(seconds)) => (seconds, "convention_file"),
        (None, None) => (timber_worktrees::git::DEFAULT_TIMEOUT_SECONDS, "default"),
    }
}
/// Reads the repository's convention file and says so on standard error before any
/// further work, so even a later refusal shows whether the file applied.
fn read_convention(path: &Path) -> Result<Convention> {
    let found = convention::load(&convention::root(path)?)?;
    if found.found {
        eprintln!("Timber: read convention file {}", found.file.display());
    } else {
        eprintln!(
            "Timber: no convention file found at {}",
            found.file.display()
        );
    }
    Ok(found)
}
fn convention_report(
    found: &Convention,
    timeout: (u64, &str),
    store: Option<&(PathBuf, &str)>,
    exclusions: Option<(&[String], &[Overridden])>,
) -> Value {
    let mut report = json!({
        "file": found.file,
        "status": if found.found { "read" } else { "not_found" },
        "git_timeout_seconds": {"value": timeout.0, "from": timeout.1},
    });
    let mut notes = Vec::new();
    if found.found {
        report["values"] = json!(found.values);
        if found.values.schedule.is_some() {
            notes.push("schedule is a note for people; Timber never schedules anything.");
        }
    } else {
        notes.push("No timber.toml at the repository root; command-line flags and defaults apply.");
    }
    if let Some((path, from)) = store {
        report["store"] = json!({"path": path, "from": from});
    }
    if let Some((applied, overridden)) = exclusions {
        report["exclusions_applied"] = json!(applied);
        report["exclusions_overridden"] = json!(overridden);
    }
    report["notes"] = json!(notes);
    report
}
fn layer(value: &str) -> Layer {
    match value {
        "committed" => Layer::Committed,
        "index" => Layer::Index,
        _ => Layer::Working,
    }
}
fn read_json<T: serde::de::DeserializeOwned>(path: &Path) -> Result<T> {
    serde_json::from_slice(&fs::read(path)?).with_context(|| format!("read {}", path.display()))
}
fn write_new(path: &Path, bytes: &[u8]) -> Result<()> {
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
        .with_context(|| {
            format!(
                "create new output {} (existing files are not overwritten)",
                path.display()
            )
        })?;
    file.write_all(bytes)?;
    file.sync_all()?;
    if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
        fs::File::open(parent)?.sync_all()?;
    }
    Ok(())
}
fn value<T: Serialize>(data: T) -> Result<Value> {
    Ok(serde_json::to_value(data)?)
}
fn load_plan(path: &Path) -> Result<Plan> {
    let v: Value = read_json(path)?;
    serde_json::from_value(v.get("result").cloned().unwrap_or(v))
        .context("invalid consolidation plan")
}
fn source_roots(store: &Store, plan: &Plan) -> Result<Vec<PathBuf>> {
    let mut roots = Vec::new();
    for selection in std::iter::once(&plan.target).chain(plan.sources.iter()) {
        roots.extend(manifest_roots(&store.load(&selection.checkpoint)?));
    }
    Ok(roots)
}
fn manifest_roots(manifest: &timber_worktrees::model::Manifest) -> Vec<PathBuf> {
    let mut roots = vec![PathBuf::from(&manifest.source_path)];
    if let Some(state) = &manifest.git {
        roots.extend(state.protected_roots.iter().map(PathBuf::from));
    }
    for captured in &manifest.captured_dependencies {
        roots.extend(captured.protected_roots.iter().map(PathBuf::from));
    }
    roots
}
fn protect_output(output: Option<&Path>, roots: &[PathBuf]) -> Result<()> {
    if let Some(output) = output {
        for root in roots {
            fsx::ensure_outside(output, root)?;
        }
    }
    Ok(())
}

fn execute(cli: &Cli) -> Result<(Value, i32)> {
    timber_worktrees::git::set_timeout_seconds(
        cli.git_timeout
            .unwrap_or(timber_worktrees::git::DEFAULT_TIMEOUT_SECONDS),
    );
    // Commands without a repository argument do not read a convention file.
    let flag_store = || store_for(cli, None).map(|(path, _)| path);
    let mut code = 0;
    let (command, result) = match &cli.command {
        Commands::Init { worktree } => {
            let root = convention::root(worktree)?;
            ensure!(
                root.canonicalize()? == worktree.canonicalize()?,
                "init must select the worktree root: {}",
                root.display()
            );
            protect_output(
                cli.out.as_deref(),
                &timber_worktrees::git::protected_roots(&root)?,
            )?;
            let file = convention::init(&root)?;
            (
                "init",
                json!({"status":"written","file":file,"notes":[
                    "Commented template: every key is commented out, so nothing changes until you edit it.",
                    "Review and commit timber.toml so every person and agent on this repository reads the same convention.",
                    "Timber never schedules anything; schedule is a note for people."
                ]}),
            )
        }
        Commands::Inspect {
            repo,
            folder,
            all_worktrees,
            worktree,
            compare_folders,
            list_ignored,
        } => {
            let options = inspect::InspectOptions {
                all_worktrees: *all_worktrees,
                worktrees: worktree.clone(),
                compare_folders: *compare_folders,
                list_ignored: *list_ignored,
            };
            let found = read_convention(repo)?;
            let timeout = timeout_for(cli, &found);
            timber_worktrees::git::set_timeout_seconds(timeout.0);
            let mut result = inspect::inspect_with_options(repo, folder, &options)?;
            result["convention"] = convention_report(&found, timeout, None, None);
            if result["status"] == "partial" {
                code = 1;
            }
            let mut roots = vec![repo.clone()];
            roots.extend(folder.clone());
            if let Some(trees) = result["worktrees"].as_array() {
                for tree in trees {
                    if let Some(p) = tree["path"].as_str() {
                        roots.push(p.into());
                    }
                }
            }
            protect_output(cli.out.as_deref(), &roots)?;
            ("inspect", result)
        }
        Commands::Checkpoint {
            worktree,
            include_ignored,
            exclude,
            writers_paused,
        } => {
            let found = read_convention(worktree)?;
            let timeout = timeout_for(cli, &found);
            timber_worktrees::git::set_timeout_seconds(timeout.0);
            let chosen_store = store_for(cli, Some(&found))?;
            let (applied, overridden) =
                convention::merge_exclusions(&found.values.exclude, exclude, include_ignored)?;
            let roots = timber_worktrees::git::protected_roots(worktree)?;
            protect_output(Some(&chosen_store.0), &roots)?;
            protect_output(cli.out.as_deref(), &roots)?;
            let store = Store::open(&chosen_store.0)?;
            let manifest = capture::checkpoint_with_convention(
                &store,
                worktree,
                include_ignored,
                exclude,
                &applied,
                *writers_paused,
            )?;
            let verification = restore::verify(&store, &manifest.id)?;
            let report = convention_report(
                &found,
                timeout,
                Some(&chosen_store),
                Some((&applied, &overridden)),
            );
            (
                "checkpoint",
                json!({"checkpoint":manifest,"verification":verification,"convention":report}),
            )
        }
        Commands::Import {
            folder,
            map,
            writers_paused,
            dependency_repo,
        } => {
            let store_path = flag_store()?;
            fsx::ensure_outside(&store_path, folder)?;
            protect_output(cli.out.as_deref(), std::slice::from_ref(folder))?;
            if let Some(repo) = dependency_repo {
                let roots = timber_worktrees::git::protected_roots(repo)?;
                protect_output(Some(&store_path), &roots)?;
                protect_output(cli.out.as_deref(), &roots)?;
            }
            let map: Option<ImportMap> = map.as_deref().map(read_json).transpose()?;
            let store = Store::open(&store_path)?;
            let manifest = capture::import_folder_with_dependencies(
                &store,
                folder,
                map,
                *writers_paused,
                dependency_repo.as_deref(),
            )?;
            let verification = restore::verify(&store, &manifest.id)?;
            (
                "import",
                json!({"checkpoint":manifest,"verification":verification}),
            )
        }
        Commands::Checkpoints { command } => {
            let store = Store::existing(&flag_store()?)?;
            match command {
                CheckpointCommands::List => {
                    let manifests = store.list()?;
                    let roots = manifests
                        .iter()
                        .flat_map(manifest_roots)
                        .collect::<Vec<_>>();
                    protect_output(cli.out.as_deref(), &roots)?;
                    ("checkpoints_list", value(manifests)?)
                }
                CheckpointCommands::Show { id } => {
                    let manifest = store.load(id)?;
                    protect_output(cli.out.as_deref(), &manifest_roots(&manifest))?;
                    ("checkpoints_show", value(manifest)?)
                }
            }
        }
        Commands::Verify { id } => {
            let store = Store::existing(&flag_store()?)?;
            let m = store.load(id)?;
            protect_output(cli.out.as_deref(), &manifest_roots(&m))?;
            ("verify", value(restore::verify(&store, id)?)?)
        }
        Commands::Restore { id, to } => {
            let store = Store::existing(&flag_store()?)?;
            let m = store.load(id)?;
            protect_output(cli.out.as_deref(), &manifest_roots(&m))?;
            ("restore", value(restore::restore(&store, id, to)?)?)
        }
        Commands::RestoreGit { id, group, to } => {
            let store = Store::existing(&flag_store()?)?;
            let manifest = store.load(id)?;
            let roots = manifest_roots(&manifest);
            protect_output(Some(to), &roots)?;
            protect_output(cli.out.as_deref(), &roots)?;
            (
                "restore_git",
                value(restore::restore_dependency(&store, id, group, to)?)?,
            )
        }
        Commands::Plan {
            target,
            source,
            layer: l,
        } => {
            let store = Store::existing(&flag_store()?)?;
            let target = Selection {
                checkpoint: target.clone(),
                layer: layer(l),
                extra_paths: Vec::new(),
            };
            let sources = source
                .iter()
                .map(|id| Selection {
                    checkpoint: id.clone(),
                    layer: layer(l),
                    extra_paths: Vec::new(),
                })
                .collect();
            let plan = planning::create(&store, target, sources)?;
            protect_output(cli.out.as_deref(), &source_roots(&store, &plan)?)?;
            ("plan", value(plan)?)
        }
        Commands::Preview { plan, to } => {
            let store = Store::existing(&flag_store()?)?;
            let plan = load_plan(plan)?;
            let roots = source_roots(&store, &plan)?;
            protect_output(Some(to), &roots)?;
            protect_output(cli.out.as_deref(), &roots)?;
            let result = preview::build(&store, &plan, to)?;
            if result.status != "complete" {
                code = 1;
            }
            ("preview", value(result)?)
        }
        Commands::Assess { plan } => {
            let store = Store::existing(&flag_store()?)?;
            let plan = load_plan(plan)?;
            protect_output(cli.out.as_deref(), &source_roots(&store, &plan)?)?;
            let result = planning::assess(&store, &plan)?;
            code = 1; // An assessment never grants permission to clean up a source.
            ("assess", result)
        }
        Commands::Report { result, to } => {
            let result: Value = read_json(result)?;
            fn protect_report(value: &Value, to: &Path) -> Result<()> {
                if let Some(map) = value.as_object() {
                    for (key, v) in map {
                        if key == "protected_roots" {
                            if let Some(roots) = v.as_array() {
                                for p in roots {
                                    if let Some(p) = p.as_str() {
                                        fsx::ensure_outside(to, Path::new(p))?;
                                    }
                                }
                            }
                        }
                        if ["source_path", "repo", "path"].contains(&key.as_str()) {
                            if let Some(p) = v.as_str() {
                                if Path::new(p).is_absolute() {
                                    fsx::ensure_outside(to, Path::new(p))?;
                                }
                            }
                        }
                        protect_report(v, to)?;
                    }
                } else if let Some(items) = value.as_array() {
                    for v in items {
                        protect_report(v, to)?;
                    }
                }
                Ok(())
            }
            protect_report(&result, to)?;
            if let Some(out) = cli.out.as_deref() {
                protect_report(&result, out)?;
            }
            // Plans and preview receipts refer to checkpoint identities rather
            // than original paths. Resolve those roots whenever they are present.
            fn checkpoint_ids(value: &Value, ids: &mut std::collections::BTreeSet<String>) {
                if let Some(map) = value.as_object() {
                    for (key, v) in map {
                        if key == "checkpoint" {
                            if let Some(id) = v.as_str() {
                                if timber_worktrees::store::valid_digest(id) {
                                    ids.insert(id.to_owned());
                                }
                            }
                        }
                        checkpoint_ids(v, ids);
                    }
                } else if let Some(items) = value.as_array() {
                    for v in items {
                        checkpoint_ids(v, ids);
                    }
                }
            }
            let mut ids = std::collections::BTreeSet::new();
            checkpoint_ids(&result, &mut ids);
            if !ids.is_empty() {
                let store = Store::existing(&flag_store()?).context(
                    "source-bound reports require the checkpoint store for output confinement",
                )?;
                let roots = ids
                    .iter()
                    .map(|id| Ok(manifest_roots(&store.load(id)?)))
                    .collect::<Result<Vec<_>>>()?
                    .into_iter()
                    .flatten()
                    .collect::<Vec<_>>();
                protect_output(Some(to), &roots)?;
                protect_output(cli.out.as_deref(), &roots)?;
            }
            ensure!(
                cli.out.as_ref() != Some(to),
                "HTML and JSON outputs must differ"
            );
            write_new(to, report::render(&result).as_bytes())?;
            (
                "report",
                json!({"status":"written","destination":to,"notes":["Local report only; not published or accepted."]}),
            )
        }
    };
    let envelope = json!({"schema":1,"command":command,"result":result});
    if let Some(out) = &cli.out {
        // A consolidation plan is directly editable; other results keep their command envelope.
        let saved = if command == "plan" {
            &envelope["result"]
        } else {
            &envelope
        };
        let bytes = serde_json::to_vec_pretty(saved)?;
        write_new(out, &bytes)?;
    }
    Ok((envelope, code))
}
fn main() {
    let cli = Cli::parse();
    let code = match execute(&cli) {
        Ok((value, code)) => {
            if cli.json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&value).expect("serializable result")
                );
            } else {
                println!("{}", report::terminal(&value));
            }
            code
        }
        Err(error) => {
            let code = if matches!(
                cli.command,
                Commands::Verify { .. } | Commands::Restore { .. } | Commands::RestoreGit { .. }
            ) {
                4
            } else {
                3
            };
            if cli.json {
                eprintln!(
                    "{}",
                    json!({"schema":1,"error":{"code":"OPERATION_FAILED","message":format!("{error:#}")}})
                );
            } else {
                eprintln!("Timber stopped: {error:#}");
            }
            code
        }
    };
    if code != 0 {
        std::process::exit(code);
    }
}
