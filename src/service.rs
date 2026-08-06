use std::{fs, path::Path};

use anyhow::{Context, Result, bail};
use serde_json::{Value, json};

use crate::{
    model::{Config, Filter, Task, TaskRow},
    runtime,
    store::Store,
};

pub struct Service {
    pub config: Config,
    pub store: Store,
}

impl Service {
    pub fn open(config: &Path) -> Result<Self> {
        let config = Config::load(config)?;
        let store = Store::open(&config.project.database)?;
        Ok(Self { config, store })
    }

    pub fn call(&mut self, method: &str, input: &Value) -> Result<Value> {
        match method {
            "view.list" => Ok(serde_json::to_value(&self.config.views)?),
            "task.ingest" => self.ingest(input),
            "task.query" => self.query(input),
            "task.get" => self.status(&self.store.get(text(input, "task")?)?),
            "task.claim" => self.claim(input),
            "task.heartbeat" => {
                let lease = now() + input["lease_seconds"].as_i64().unwrap_or(900).clamp(30, 86_400);
                self.store.heartbeat(text(input, "task")?, text(input, "owner")?, lease)?;
                Ok(json!({"lease_until":lease}))
            }
            "task.cancel" => self.control(input, "cancel"),
            "task.pause" => self.control(input, "pause"),
            "task.resume" => self.control(input, "resume"),
            "task.reprioritize" => self.control(input, "reprioritize"),
            "worktree.prepare" => self.worktree(input),
            "gate.run" => self.gate(input, false),
            "gate.approve" => self.gate(input, true),
            "step.advance" => self.advance(input),
            "task.block" => self.control(input, "block"),
            "task.retry" => self.control(input, "retry"),
            "integration.run" => self.integrate(input),
            "reconcile" => self.reconcile(),
            _ => bail!("unknown method {method}"),
        }
    }

    fn ingest(&mut self, input: &Value) -> Result<Value> {
        let tasks: Vec<Task> = serde_json::from_value(input.get("tasks").cloned().unwrap_or_else(|| input.clone()))?;
        Ok(json!({"ingested":self.store.ingest(&tasks)?}))
    }

    fn query(&self, input: &Value) -> Result<Value> {
        let filter = self.filter(input)?;
        let states = input["states"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .map(str::to_owned)
            .collect::<Vec<_>>();
        let ready = input["ready"].as_bool().unwrap_or(false);
        let full = input["full"].as_bool().unwrap_or(false);
        let limit = input["limit"].as_u64().unwrap_or(50).clamp(1, 500) as usize;
        let mut result = Vec::new();
        for row in self.store.all()? {
            if (!states.is_empty() && !states.contains(&row.state)) || !filter.matches(&row.value()) || (ready && !self.store.ready(&row)?) {
                continue;
            }
            result.push(if full {
                serde_json::to_value(&row)?
            } else {
                json!({"uri":row.task.uri,"title":row.task.title,"tags":row.task.tags,"priority":row.task.priority,"depends_on":row.task.depends_on,"queue_priority":row.queue_priority,"state":row.state,"paused":row.paused,"workflow":row.active_workflow,"step":row.step,"owner":row.owner,"lease_until":row.lease_until,"error":row.error,"revision":row.revision,"ready":self.store.ready(&row)?})
            });
            if result.len() == limit {
                break;
            }
        }
        Ok(Value::Array(result))
    }

    fn claim(&mut self, input: &Value) -> Result<Value> {
        let owner = text(input, "owner")?;
        let filter = self.filter(input)?;
        let limit = input["limit"].as_u64().unwrap_or(1).clamp(1, 32) as usize;
        let mut selected = Vec::new();
        let mut workflows = Vec::new();
        for row in self.store.all()? {
            if !self.store.ready(&row)? || !filter.matches(&row.value()) {
                continue;
            }
            let workflow = self.config.route(&row.value(), row.task.workflow.as_deref());
            self.config.workflow(workflow.as_deref())?;
            selected.push(row.task.uri);
            workflows.push(workflow);
            if selected.len() == limit {
                break;
            }
        }
        let lease = now() + input["lease_seconds"].as_i64().unwrap_or(900).clamp(30, 86_400);
        Ok(Value::Array(
            self.store
                .claim(&selected, owner, lease, &workflows)?
                .iter()
                .map(|row| self.status(row))
                .collect::<Result<_>>()?,
        ))
    }

    fn control(&self, input: &Value, action: &str) -> Result<Value> {
        let uri = text(input, "task")?;
        let number = if action == "reprioritize" {
            Some(input["priority"].as_i64().context("missing priority")?)
        } else {
            None
        };
        self.store.control(uri, action, number, input["reason"].as_str())?;
        if matches!(action, "block" | "retry") {
            Ok(json!({"task":uri,"state":if action == "block" {"blocked"} else {"backlog"}}))
        } else {
            self.status(&self.store.get(uri)?)
        }
    }

    fn worktree(&self, input: &Value) -> Result<Value> {
        let row = self.store.get(text(input, "task")?)?;
        if row.paused || (row.state != "running" && row.state != "blocked") {
            bail!("task must be claimed before preparing a worktree");
        }
        if let Some(path) = &row.worktree {
            if Path::new(path).exists() {
                return Ok(json!({"branch":row.branch,"worktree":path,"reused":true}));
            }
            self.store.workspace(&row.task.uri, &row.state, row.branch.as_deref(), None)?;
        }
        let (branch, path) = runtime::prepare_worktree(
            &self.config.project.repository,
            &self.config.project.worktree_root,
            &row,
            input["base"].as_str(),
        )?;
        if let Err(error) = self.store.workspace(&row.task.uri, &row.state, Some(&branch), path.to_str()) {
            let _ = runtime::remove_worktree(&self.config.project.repository, &path);
            return Err(error);
        }
        Ok(json!({"branch":branch,"worktree":path,"reused":false}))
    }

    fn gate(&self, input: &Value, approval: bool) -> Result<Value> {
        let row = self.store.get(text(input, "task")?)?;
        if row.state != "running" {
            bail!("only running tasks can execute gates");
        }
        let workflow = self.config.workflow(row.active_workflow.as_deref())?;
        let step = workflow.steps.get(row.step).context("task has no active step")?;
        let gate_id = text(input, "gate")?;
        let gate = self
            .config
            .gates
            .iter()
            .find(|gate| gate.id == gate_id && step.gates.iter().any(|item| item == gate_id) && gate.when.matches(&row.value()))
            .with_context(|| format!("gate {gate_id} does not apply to step {}", step.id))?;
        let cwd = row.worktree.as_deref().map(Path::new).unwrap_or(&self.config.project.repository);
        let tree = runtime::tree(cwd, false)?;
        let output = if approval {
            if gate.kind != "approval" {
                bail!("gate {} is not an approval gate", gate.id);
            }
            json!({"gate":gate.id,"tree":tree,"ok":input["approved"].as_bool().unwrap_or(false),"approved_by":text(input,"by")?,"note":input["note"]})
        } else {
            runtime::gate_output(&runtime::run_gate(gate, &row, cwd, &step.id, false)?)
        };
        let ok = output["ok"].as_bool().unwrap_or(false);
        self.store
            .receipt(&row.task.uri, &step.id, &gate.id, &tree, ok, &serde_json::to_string(&output)?)?;
        self.store.execution(
            &row.task.uri,
            &row.state,
            if ok || !gate.required { "running" } else { "blocked" },
            row.step,
            row.owner.as_deref(),
            if ok || !gate.required { None } else { Some("gate failed") },
        )?;
        Ok(output)
    }

    fn advance(&self, input: &Value) -> Result<Value> {
        let row = self.store.get(text(input, "task")?)?;
        if row.state != "running" || row.owner.as_deref() != Some(text(input, "owner")?) {
            bail!("task is not running or owned by this worker");
        }
        let workflow = self.config.workflow(row.active_workflow.as_deref())?;
        let step = workflow.steps.get(row.step).context("task has no active step")?;
        let cwd = row.worktree.as_deref().map(Path::new).unwrap_or(&self.config.project.repository);
        let tree = runtime::tree(cwd, false)?;
        for gate_id in &step.gates {
            let gate = self
                .config
                .gates
                .iter()
                .find(|gate| &gate.id == gate_id)
                .context("configured gate disappeared")?;
            if gate.when.matches(&row.value()) && gate.required && self.store.proof(&row.task.uri, &step.id, gate_id, &tree)? != Some(true) {
                bail!("required gate {gate_id} is not green for tree {tree}");
            }
        }
        let next = row.step + 1;
        let done = next == workflow.steps.len();
        if done && let Some(path) = &row.worktree {
            runtime::remove_worktree(&self.config.project.repository, Path::new(path))?;
            self.store.workspace(&row.task.uri, &row.state, row.branch.as_deref(), None)?;
        }
        self.store.execution(
            &row.task.uri,
            &row.state,
            if done { "candidate" } else { "running" },
            next,
            if done { None } else { row.owner.as_deref() },
            None,
        )?;
        Ok(
            json!({"task":row.task.uri,"completed_step":step.id,"state":if done {"candidate"} else {"running"},"next_step":workflow.steps.get(next).map(|item|&item.id)}),
        )
    }

    fn reconcile(&self) -> Result<Value> {
        let expired = self.store.expire(now())?;
        runtime::git(&self.config.project.repository, &["worktree", "prune"])?;
        Ok(json!({"expired_leases":expired}))
    }

    fn integrate(&self, input: &Value) -> Result<Value> {
        let filter = self.filter(input)?;
        let mut pending = self
            .store
            .all()?
            .into_iter()
            .filter(|row| row.state == "candidate" && !row.paused && filter.matches(&row.value()))
            .collect::<Vec<_>>();
        if pending.is_empty() {
            bail!("no candidate tasks to integrate");
        }
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis();
        let branch = input["branch"]
            .as_str()
            .map(str::to_owned)
            .unwrap_or_else(|| format!("integration/taskfleet-{stamp}"));
        let path = self.config.project.worktree_root.join(format!("integration-{stamp}"));
        fs::create_dir_all(&self.config.project.worktree_root)?;
        runtime::git(
            &self.config.project.repository,
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
                self.store
                    .execution(&row.task.uri, &row.state, "blocked", row.step, None, Some("missing task branch"))?;
                blocked.push(row.task.uri);
                continue;
            };
            if runtime::git(&path, &["merge", "--no-ff", "--no-commit", card_branch]).is_err() {
                let _ = runtime::git(&path, &["merge", "--abort"]);
                self.store
                    .execution(&row.task.uri, &row.state, "blocked", row.step, None, Some("merge conflict"))?;
                blocked.push(row.task.uri);
                continue;
            }
            let mut green = true;
            for gate in self
                .config
                .gates
                .iter()
                .filter(|gate| gate.events.iter().any(|event| event == "integration.merge") && gate.when.matches(&row.value()))
            {
                if gate.kind != "command" {
                    green = false;
                    break;
                }
                let result = runtime::run_gate(gate, &row, &path, "integration", true)?;
                self.store.receipt(
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
                self.store.execution(&row.task.uri, &row.state, "done", row.step, None, None)?;
                merged.push(row.task.uri);
            } else {
                runtime::git(&path, &["merge", "--abort"])?;
                self.store
                    .execution(&row.task.uri, &row.state, "blocked", row.step, None, Some("integration gate failed"))?;
                blocked.push(row.task.uri);
            }
        }
        Ok(json!({"branch":branch,"worktree":path,"merged":merged,"blocked":blocked}))
    }

    fn filter(&self, input: &Value) -> Result<Filter> {
        let view = match input["view"].as_str() {
            Some(id) => self
                .config
                .views
                .iter()
                .find(|view| view.id == id)
                .with_context(|| format!("unknown view {id}"))?
                .filter
                .clone(),
            None => Filter::default(),
        };
        let extra = match input.get("filter").filter(|value| !value.is_null()) {
            Some(value) => serde_json::from_value(value.clone())?,
            None => Filter::default(),
        };
        Ok(Filter::And { args: vec![view, extra] })
    }

    fn status(&self, row: &TaskRow) -> Result<Value> {
        let workflow = self.config.workflow(row.active_workflow.as_deref())?;
        let active = workflow.steps.get(row.step);
        let tree = row.worktree.as_deref().and_then(|path| runtime::tree(Path::new(path), false).ok());
        let gates = active.map(|step| step.gates.iter().filter_map(|id| self.config.gates.iter().find(|gate| &gate.id == id)).filter(|gate| gate.when.matches(&row.value())).map(|gate| {
            let proof = tree.as_deref().map(|tree| self.store.proof(&row.task.uri, &step.id, &gate.id, tree)).transpose()?.flatten();
            Ok(json!({"id":gate.id,"kind":gate.kind,"required":gate.required,"status":match proof {Some(true)=>"green",Some(false)=>"red",None=>"pending"}}))
        }).collect::<Result<Vec<_>>>()).transpose()?.unwrap_or_default();
        Ok(
            json!({"task":row.task,"execution":{"state":row.state,"paused":row.paused,"queue_priority":row.queue_priority,"workflow":row.active_workflow,"step_index":row.step,"active_step":active,"owner":row.owner,"lease_until":row.lease_until,"branch":row.branch,"worktree":row.worktree,"error":row.error,"revision":row.revision,"tree":tree,"gates":gates}}),
        )
    }
}

fn text<'a>(value: &'a Value, name: &str) -> Result<&'a str> {
    value[name].as_str().with_context(|| format!("missing {name}"))
}
fn now() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_secs() as i64
}
pub fn init(path: &Path) -> Result<()> {
    if path.exists() {
        bail!("config already exists: {}", path.display());
    }
    fs::write(path, include_str!("../assets/taskfleet.example.toml"))?;
    Ok(())
}
