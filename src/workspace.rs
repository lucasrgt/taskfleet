use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    process::Command,
};

use anyhow::{Context, Result, bail};
use serde_json::{Value, json};

use crate::{
    model::{Project, TaskRow},
    runtime,
};

pub fn prepare(
    project: &Project,
    repository: &Path,
    row: &TaskRow,
    base: Option<&str>,
    dependency_branches: &[(String, String)],
    occupied: &[String],
) -> Result<Value> {
    assert_capacity(project, occupied.len())?;
    ensure_cache_dirs(project)?;
    match project.workspace_provider.as_str() {
        "git-worktree" => prepare_git(project, repository, row, base, dependency_branches, occupied, "git-worktree"),
        "agentfs" => {
            if !agentfs_available(project) {
                bail!("workspace_provider agentfs requires the agentfs CLI on PATH");
            }
            let mut value = prepare_git(project, repository, row, base, dependency_branches, occupied, "agentfs")?;
            value["note"] = json!("agentfs CLI detected; workspace remains a git worktree until FUSE overlay is configured by the harness");
            Ok(value)
        }
        "reflink" => {
            let mut value = prepare_git(project, repository, row, base, dependency_branches, occupied, "reflink")?;
            value["cow"] = json!(cow_supported());
            Ok(value)
        }
        other => bail!("unsupported workspace_provider {other}"),
    }
}

pub fn assert_capacity(project: &Project, live: usize) -> Result<()> {
    let max = project.max_parallel_workspaces;
    if max > 0 && live >= max as usize {
        bail!("workspace pool exhausted: {live} live, max {max}");
    }
    Ok(())
}

pub fn ensure_cache_dirs(project: &Project) -> Result<()> {
    for path in project.shared_caches.values() {
        fs::create_dir_all(path)?;
    }
    Ok(())
}

pub fn allocate_path(project: &Project, occupied: &[String], row: &TaskRow) -> Result<PathBuf> {
    fs::create_dir_all(&project.worktree_root)?;
    if project.max_parallel_workspaces == 0 {
        let name = {
            let value = runtime::slug(&row.task.title);
            if value.is_empty() { "task".into() } else { value }
        };
        return Ok(project.worktree_root.join(format!("{name}-{:08x}", runtime::hash(&row.task.uri))));
    }
    for slot in 0..project.max_parallel_workspaces {
        let path = project.worktree_root.join(format!("pool-{slot}"));
        if occupied.iter().any(|item| Path::new(item) == path) {
            continue;
        }
        return Ok(path);
    }
    bail!("workspace pool exhausted: max {}", project.max_parallel_workspaces)
}

pub fn status(path: &Path) -> Result<Value> {
    if !path.exists() {
        bail!("workspace does not exist: {}", path.display());
    }
    let dirty = !runtime::git(path, &["status", "--porcelain"])?.is_empty();
    let branch = runtime::git(path, &["rev-parse", "--abbrev-ref", "HEAD"]).unwrap_or_else(|_| "DETACHED".into());
    let tree = runtime::git(path, &["rev-parse", "HEAD^{tree}"]).ok();
    Ok(json!({"path": path, "exists": true, "dirty": dirty, "branch": branch, "tree": tree}))
}

pub fn diff(path: &Path) -> Result<Value> {
    if !path.exists() {
        bail!("workspace does not exist: {}", path.display());
    }
    let porcelain = runtime::git(path, &["status", "--porcelain"])?;
    let patch = runtime::git(path, &["diff", "HEAD"]).unwrap_or_default();
    Ok(json!({"path": path, "porcelain": porcelain, "patch": patch}))
}

pub fn destroy(repository: &Path, path: &Path, provider: &str) -> Result<()> {
    match provider {
        "git-worktree" | "agentfs" | "reflink" => runtime::remove_worktree(repository, path),
        other => bail!("unsupported workspace_provider {other}"),
    }
}

pub fn cache_env(project: &Project) -> BTreeMap<String, String> {
    let mut env = BTreeMap::new();
    for (key, path) in &project.shared_caches {
        let var = match key.as_str() {
            "pnpm_store" => "PNPM_STORE_PATH",
            "nuget_packages" => "NUGET_PACKAGES",
            "cargo_home" => "CARGO_HOME",
            "sccache_dir" => "SCCACHE_DIR",
            other => other,
        };
        env.insert(var.to_owned(), path.display().to_string());
    }
    env
}

pub fn copy_prefer_cow(src: &Path, dst: &Path) -> Result<()> {
    crate::cas::copy_prefer_cow(src, dst)
}

fn prepare_git(
    project: &Project,
    repository: &Path,
    row: &TaskRow,
    base: Option<&str>,
    dependency_branches: &[(String, String)],
    occupied: &[String],
    provider: &str,
) -> Result<Value> {
    let path = allocate_path(project, occupied, row)?;
    if path.exists() {
        let _ = runtime::remove_worktree(repository, &path);
    }
    let (branch, path) = runtime::prepare_worktree(repository, &project.worktree_root, row, base, Some(path))?;
    let mut merged = Vec::new();
    for (dep_uri, dep_branch) in dependency_branches {
        runtime::git(&path, &["merge", "--no-ff", "--no-edit", dep_branch])
            .with_context(|| format!("failed merging dependency {dep_uri} branch {dep_branch}"))?;
        merged.push(json!({"task": dep_uri, "branch": dep_branch}));
    }
    Ok(json!({
        "provider": provider,
        "branch": branch,
        "worktree": path,
        "path": path,
        "reused": false,
        "pool_slot": path.file_name().and_then(|name| name.to_str()).and_then(|name| name.strip_prefix("pool-")),
        "merged_dependencies": merged,
        "env": cache_env(project),
    }))
}

fn agentfs_available(project: &Project) -> bool {
    match &project.agentfs_bin {
        Some(path) => path.exists(),
        None => Command::new("agentfs")
            .arg("--version")
            .output()
            .map(|output| output.status.success())
            .unwrap_or(false),
    }
}

fn cow_supported() -> bool {
    cfg!(any(target_os = "linux", target_os = "macos"))
}

pub fn ensure_parent(path: &Path) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    Ok(())
}

pub fn workspace_path(row: &TaskRow) -> Result<PathBuf> {
    row.worktree.as_deref().map(PathBuf::from).context("task has no workspace")
}
