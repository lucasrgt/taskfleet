use std::{
    env, fs,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, bail};
use serde::Serialize;

use crate::{model::Config, runtime};

#[rustfmt::skip]
#[derive(Debug, Serialize)]
pub struct Location { pub mode: &'static str, pub repository: PathBuf, pub config: PathBuf, pub state: Option<PathBuf>, pub enabled: bool, #[serde(skip_serializing_if = "Option::is_none")] pub changed: Option<bool>, #[serde(skip_serializing_if = "Option::is_none")] pub purged: Option<bool> }

pub fn locate(current: &Path, explicit: Option<&Path>, home: Option<&Path>) -> Result<Location> {
    if let Some(path) = explicit {
        return local(if path.is_absolute() { path.into() } else { current.join(path) });
    }
    let mut directory = current.canonicalize()?;
    loop {
        let candidate = directory.join("taskfleet.toml");
        if candidate.exists() {
            return local(candidate);
        }
        let Some(parent) = directory.parent() else { break };
        directory = parent.into();
    }
    if runtime::git(current, &["rev-parse", "--show-toplevel"]).is_err() {
        return none(current);
    }
    let (repository, state) = entry(current, home)?;
    let config = state.join("taskfleet.toml");
    let enabled = state.join(".enabled").exists();
    if enabled {
        validate_external(&repository, &config)?;
    }
    Ok(Location {
        mode: "external",
        repository,
        config,
        state: Some(state),
        enabled,
        changed: None,
        purged: None,
    })
}

pub fn manage(current: &Path, home: Option<&Path>, action: &str) -> Result<Location> {
    let (repository, state) = entry(current, home)?;
    fs::create_dir_all(state.parent().context("external state has no parent")?)?;
    let _lock = lifecycle_lock(&state)?;
    let config = state.join("taskfleet.toml");
    let marker = state.join(".enabled");
    if config.exists() {
        validate_external(&repository, &config)?;
    }
    let (enabled, changed, purged) = match action {
        "enable" => {
            fs::create_dir_all(&state)?;
            if !config.exists() {
                create_config(&repository, &state, &config)?;
            }
            let changed = match fs::OpenOptions::new().write(true).create_new(true).open(&marker) {
                Ok(_) => true,
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => false,
                Err(error) => return Err(error.into()),
            };
            (true, Some(changed), None)
        }
        "disable" => {
            let changed = marker.exists();
            if changed {
                fs::remove_file(&marker)?;
            }
            (false, Some(changed), None)
        }
        "purge" => {
            if marker.exists() {
                bail!("disable external mode before purging it");
            }
            let purged = state.exists();
            if purged {
                fs::remove_dir_all(&state)?;
                let _ = runtime::git(&repository, &["worktree", "prune"]);
            }
            (false, None, Some(purged))
        }
        _ => bail!("unknown external action {action}"),
    };
    Ok(Location {
        mode: "external",
        repository,
        config,
        state: Some(state),
        enabled,
        changed,
        purged,
    })
}

#[rustfmt::skip]
fn none(current: &Path) -> Result<Location> { Ok(Location { mode: "none", repository: current.canonicalize()?, config: PathBuf::new(), state: None, enabled: false, changed: None, purged: None }) }

#[rustfmt::skip]
fn local(config: PathBuf) -> Result<Location> { let loaded = Config::load(&config)?; Ok(Location { mode: "local", repository: loaded.project.repository, config: config.canonicalize()?, state: None, enabled: true, changed: None, purged: None }) }

#[rustfmt::skip]
fn entry(current: &Path, home: Option<&Path>) -> Result<(PathBuf, PathBuf)> {
    let repository = PathBuf::from(runtime::git(current, &["rev-parse", "--show-toplevel"]).context("external mode requires a Git repository")?).canonicalize()?;
    let root = match home.map(Path::to_path_buf) { Some(path) => path, None => state_root()? };
    if root.components().any(|item| matches!(item, std::path::Component::ParentDir)) { bail!("external state home may not contain parent traversal"); }
    let root = if root.is_absolute() { root } else { current.join(root) };
    let root = resolved(&root)?;
    if root.starts_with(&repository) { bail!("external state home must be outside the repository"); }
    let key = repository.to_string_lossy().bytes().fold(14_695_981_039_346_656_037_u64, |hash, byte| (hash ^ u64::from(byte)).wrapping_mul(1_099_511_628_211));
    Ok((repository, root.join("projects").join(format!("{key:016x}"))))
}

#[rustfmt::skip]
fn lifecycle_lock(state: &Path) -> Result<fs::File> { let lock = fs::OpenOptions::new().read(true).write(true).create(true).truncate(false).open(state.with_extension("lock"))?; lock.lock()?; Ok(lock) }

#[rustfmt::skip]
fn resolved(path: &Path) -> Result<PathBuf> {
    let (mut base, mut suffix) = (path.to_path_buf(), PathBuf::new());
    while fs::symlink_metadata(&base).is_err() { suffix = PathBuf::from(base.file_name().context("external state has no existing ancestor")?).join(&suffix); base = base.parent().context("external state has no existing ancestor")?.into(); }
    Ok(base.canonicalize()?.join(suffix))
}

#[rustfmt::skip]
fn state_root() -> Result<PathBuf> {
    env::var_os("TASKFLEET_STATE_HOME").map(PathBuf::from)
        .or_else(|| env::var_os("XDG_STATE_HOME").map(|path| PathBuf::from(path).join("taskfleet"))).or_else(|| env::var_os("LOCALAPPDATA").map(|path| PathBuf::from(path).join("Taskfleet")))
        .or_else(|| env::var_os("HOME").map(|path| PathBuf::from(path).join(".local/state/taskfleet"))).or_else(|| env::var_os("USERPROFILE").map(|path| PathBuf::from(path).join(".local/state/taskfleet")))
        .context("cannot determine Taskfleet state home; set TASKFLEET_STATE_HOME")
}

fn create_config(repository: &Path, state: &Path, config: &Path) -> Result<()> {
    let repository = toml::Value::String(repository.to_string_lossy().into_owned());
    let contents = format!(
        "schema = 1\n\n[project]\nrepository = {repository}\ndatabase = \"state.sqlite\"\nworktree_root = \"worktrees\"\n\n[[view]]\nid = \"all\"\nfilter = {{ op = \"true\" }}\n"
    );
    let temporary = tempfile::NamedTempFile::new_in(state)?;
    fs::write(temporary.path(), contents)?;
    if let Err(error) = temporary.persist_noclobber(config)
        && !config.exists()
    {
        return Err(error.error.into());
    }
    Ok(())
}

#[rustfmt::skip]
fn validate_external(repository: &Path, config: &Path) -> Result<()> { if Config::load(config)?.project.repository.canonicalize()? != repository { bail!("external state belongs to another repository"); } Ok(()) }
