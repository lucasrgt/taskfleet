use std::fs;

use anyhow::{Context, Result, bail};
use serde_json::{Value, json};

use crate::{
    model::{Config, Filter},
    runtime,
    store::Store,
    workspace,
};

pub fn run(config: &Config, store: &Store, filter: &Filter, input: &Value) -> Result<Value> {
    let mut pending = store
        .all()?
        .into_iter()
        .filter(|row| row.state == "candidate" && !row.paused && filter.matches(&row.value()))
        .collect::<Vec<_>>();
    if pending.is_empty() {
        bail!("no candidate tasks to integrate");
    }
    let stamp = nonce();
    let branch = input["branch"]
        .as_str()
        .map(str::to_owned)
        .unwrap_or_else(|| format!("integration/taskfleet-{stamp}"));
    let path = config.project.worktree_root.join(format!("integration-{stamp}"));
    fs::create_dir_all(&config.project.worktree_root)?;
    runtime::git(
        &config.project.repository,
        &[
            "worktree",
            "add",
            path.to_str().context("non-utf8 integration path")?,
            "-b",
            &branch,
            input["base"].as_str().unwrap_or("HEAD"),
        ],
    )?;
    let mut merged = Vec::new();
    let mut blocked = Vec::new();
    while !pending.is_empty() {
        let index = pending
            .iter()
            .position(|row| row.task.depends_on.iter().all(|dep| !pending.iter().any(|other| &other.task.uri == dep)))
            .context("candidate dependency cycle")?;
        let row = pending.remove(index);
        let Some(card_branch) = row.branch.as_deref() else {
            store.execution(&row.task.uri, &row.state, "blocked", row.step, None, Some("missing task branch"))?;
            blocked.push(row.task.uri);
            continue;
        };
        if runtime::git(&path, &["merge", "--no-ff", "--no-commit", card_branch]).is_err() {
            let _ = runtime::git(&path, &["merge", "--abort"]);
            store.execution(&row.task.uri, &row.state, "blocked", row.step, None, Some("merge conflict"))?;
            blocked.push(row.task.uri);
            continue;
        }
        let gates = config
            .gates
            .iter()
            .filter(|gate| gate.events.iter().any(|event| event == "integration.merge") && gate.when.matches(&row.value()))
            .collect::<Vec<_>>();
        let mut green = true;
        for gate in gates {
            if gate.kind != "command" {
                green = false;
                break;
            }
            let result = runtime::run_gate(gate, &row, &path, "integration", true)?;
            store.receipt(
                &row.task.uri,
                "integration",
                &gate.id,
                &result.tree,
                result.ok,
                &serde_json::to_string(&result)?,
            )?;
            if gate.required && !result.ok {
                green = false;
                break;
            }
        }
        if green {
            runtime::git(&path, &["commit", "-m", &format!("Integrate {}", row.task.uri)])?;
            store.execution(&row.task.uri, &row.state, "done", row.step, None, None)?;
            merged.push(row.task.uri);
        } else {
            runtime::git(&path, &["merge", "--abort"])?;
            store.execution(&row.task.uri, &row.state, "blocked", row.step, None, Some("integration gate failed"))?;
            blocked.push(row.task.uri);
        }
    }
    let retain = input["retain_worktree"].as_bool().unwrap_or(config.project.retain_integration_worktree);
    let worktree = if retain {
        json!(path)
    } else {
        let _ = workspace::destroy(&config.project.repository, &path, &config.project.workspace_provider);
        Value::Null
    };
    Ok(json!({"branch": branch, "worktree": worktree, "retained": retain, "merged": merged, "blocked": blocked}))
}

fn nonce() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let nanos = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_nanos();
    format!("{nanos:x}")
}
