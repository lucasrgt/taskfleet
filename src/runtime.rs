use std::{
    fs,
    io::{Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::Duration,
};

use anyhow::{Context, Result, bail};
use serde::Serialize;
use serde_json::{Value, json};
use wait_timeout::ChildExt;

use crate::model::{Gate, TaskRow};

#[derive(Debug, Serialize)]
pub struct GateResult {
    pub gate: String,
    pub tree: String,
    pub ok: bool,
    pub timed_out: bool,
    pub exit_code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
}

pub fn prepare_worktree(repository: &Path, root: &Path, row: &TaskRow, base: Option<&str>) -> Result<(String, PathBuf)> {
    ensure_git(repository)?;
    fs::create_dir_all(root)?;
    git(repository, &["worktree", "prune"])?;
    let name = {
        let value = slug(&row.task.title);
        if value.is_empty() { "task".into() } else { value }
    };
    let branch = row.branch.clone().unwrap_or_else(|| format!("taskfleet/{name}-{:08x}", hash(&row.task.uri)));
    let path = root.join(format!("{name}-{:08x}", hash(&row.task.uri)));
    if path.exists() {
        bail!("worktree path already exists: {}", path.display());
    }
    let exists = Command::new("git")
        .args([
            "-C",
            &repository.display().to_string(),
            "show-ref",
            "--verify",
            "--quiet",
            &format!("refs/heads/{branch}"),
        ])
        .status()?
        .success();
    let mut args = vec!["worktree", "add", path.to_str().context("non-utf8 worktree path")?];
    if exists {
        args.push(&branch);
    } else {
        args.extend(["-b", &branch, base.unwrap_or("HEAD")]);
    }
    git(repository, &args)?;
    Ok((branch, path))
}

pub fn remove_worktree(repository: &Path, path: &Path) -> Result<()> {
    if path.exists() {
        git(repository, &["worktree", "remove", "--force", path.to_str().context("non-utf8 worktree path")?])?;
    }
    git(repository, &["worktree", "prune"])?;
    Ok(())
}

pub fn tree(path: &Path, staged: bool) -> Result<String> {
    if !staged && !git(path, &["status", "--porcelain"])?.is_empty() {
        bail!("worktree must be clean before proving a step");
    }
    git(path, if staged { &["write-tree"] } else { &["rev-parse", "HEAD^{tree}"] })
}

pub fn run_gate(gate: &Gate, row: &TaskRow, cwd: &Path, step: &str, staged: bool) -> Result<GateResult> {
    if gate.kind != "command" {
        bail!("gate {} requires explicit approval", gate.id);
    }
    if gate.command.is_empty() {
        bail!("gate {} has no command", gate.id);
    }
    let before = tree(cwd, staged)?;
    let mut stdout = tempfile::tempfile()?;
    let mut stderr = tempfile::tempfile()?;
    let mut child = Command::new(&gate.command[0])
        .args(&gate.command[1..])
        .current_dir(cwd)
        .stdin(Stdio::piped())
        .stdout(stdout.try_clone()?)
        .stderr(stderr.try_clone()?)
        .spawn()
        .with_context(|| format!("could not start gate {}", gate.id))?;
    let context = json!({"event":if staged {"integration.merge"} else {"step.complete"},"step":step,"task":row,"cwd":cwd});
    child
        .stdin
        .take()
        .context("gate stdin unavailable")?
        .write_all(serde_json::to_string(&context)?.as_bytes())?;
    let status = child.wait_timeout(Duration::from_secs(gate.timeout_seconds))?;
    let timed_out = status.is_none();
    if timed_out {
        child.kill()?;
    }
    let status = match status {
        Some(status) => status,
        None => child.wait()?,
    };
    let stdout = read(&mut stdout)?;
    let stderr = read(&mut stderr)?;
    let dirty = if staged {
        !git(cwd, &["diff", "--name-only"])?.is_empty()
    } else {
        !git(cwd, &["status", "--porcelain"])?.is_empty()
    };
    let after = tree(cwd, staged).unwrap_or_else(|_| before.clone());
    let ok = status.success() && !timed_out && !dirty && before == after;
    Ok(GateResult {
        gate: gate.id.clone(),
        tree: before,
        ok,
        timed_out,
        exit_code: status.code(),
        stdout,
        stderr: if dirty { format!("{stderr}\ngate changed tracked files") } else { stderr },
    })
}

pub fn git(path: &Path, args: &[&str]) -> Result<String> {
    let output = Command::new("git").arg("-C").arg(path).args(args).output()?;
    if !output.status.success() {
        bail!("git {} failed: {}", args.join(" "), String::from_utf8_lossy(&output.stderr).trim());
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned())
}

pub fn ensure_git(path: &Path) -> Result<()> {
    git(path, &["rev-parse", "--git-dir"]).map(|_| ())
}

pub fn hash(text: &str) -> u32 {
    text.bytes().fold(2_166_136_261, |hash, byte| (hash ^ u32::from(byte)).wrapping_mul(16_777_619))
}

fn slug(text: &str) -> String {
    let mut out = text
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c.to_ascii_lowercase() } else { '-' })
        .collect::<String>();
    while out.contains("--") {
        out = out.replace("--", "-");
    }
    out.trim_matches('-').chars().take(32).collect::<String>().trim_matches('-').to_owned()
}

fn read(file: &mut fs::File) -> Result<String> {
    file.seek(SeekFrom::Start(0))?;
    let mut text = String::new();
    file.take(65_536).read_to_string(&mut text)?;
    Ok(text)
}

pub fn gate_output(result: &GateResult) -> Value {
    serde_json::to_value(result).unwrap_or(Value::Null)
}
