use std::path::Path;

use anyhow::{Context, Result, bail};
use serde_json::{Value, json};

use crate::{
    capsules::Capsules,
    cas::Cas,
    integration,
    model::{Config, Filter, Task, TaskRow},
    pipeline, runtime,
    store::Store,
    workspace,
};

pub struct Service {
    pub config: Config,
    pub store: Store,
    pub cas: Cas,
}

impl Service {
    pub fn open(config: &Path) -> Result<Self> {
        let config = Config::load(config)?;
        let store = Store::open(&config.project.database)?;
        let cas = Cas::open(&config.project.cas_root)?;
        Ok(Self { config, store, cas })
    }

    pub fn call(&mut self, method: &str, input: &Value) -> Result<Value> {
        let capsules = Capsules {
            config: &self.config,
            store: &self.store,
            cas: &self.cas,
        };
        match method {
            "view.list" => Ok(serde_json::to_value(&self.config.views)?),
            "task.ingest" => self.ingest(input),
            "task.query" => self.query(input),
            "task.get" => self.status(&self.store.get(text(input, "task")?)?),
            "task.related" => self.related(input),
            "task.spawn" => self.spawn(input),
            "task.rerun" => self.rerun(input),
            "task.claim" => self.claim(input),
            "task.heartbeat" => self.heartbeat(input),
            "task.cancel" => self.control(input, "cancel"),
            "task.pause" => self.control(input, "pause"),
            "task.resume" => self.control(input, "resume"),
            "task.reprioritize" => self.control(input, "reprioritize"),
            "worktree.prepare" | "workspace.prepare" => self.prepare_workspace(input),
            "workspace.status" => self.workspace_status(input),
            "workspace.diff" => self.workspace_diff(input),
            "workspace.destroy" => self.workspace_destroy(input),
            "workspace.gc" => capsules.workspace_gc(),
            "gate.run" => self.gate(input, false),
            "gate.approve" => self.gate(input, true),
            "step.advance" => self.advance(input),
            "task.block" => self.control(input, "block"),
            "task.retry" => self.control(input, "retry"),
            "task.context" => capsules.task_context(input),
            "artifact.publish" => capsules.artifact_publish(input),
            "artifact.resolve" => capsules.artifact_resolve(input),
            "artifact.materialize" => capsules.artifact_materialize(input),
            "receipt.publish" => capsules.receipt_publish(input),
            "receipt.get" => capsules.receipt_get(input),
            "receipt.resolve_dependencies" => capsules.receipt_resolve_dependencies(input),
            "integration.run" => integration::run(&self.config, &self.store, &self.filter(input)?, input),
            "reconcile" => self.reconcile(),
            _ => bail!("unknown method {method}"),
        }
    }

    fn capsules(&self) -> Capsules<'_> {
        Capsules {
            config: &self.config,
            store: &self.store,
            cas: &self.cas,
        }
    }

    fn ingest(&mut self, input: &Value) -> Result<Value> {
        let tasks: Vec<Task> = serde_json::from_value(input.get("tasks").cloned().unwrap_or_else(|| input.clone()))?;
        Ok(json!({"ingested": self.store.ingest(&tasks)?}))
    }

    fn query(&self, input: &Value) -> Result<Value> {
        let filter = self.filter(input)?;
        let states = strings(input, "states");
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

    fn related(&self, input: &Value) -> Result<Value> {
        let row = self.store.get(text(input, "task")?)?;
        let path = input["path"].as_str().unwrap_or("meta.bundle");
        let dossier = row.value();
        let Some(value) = at_path(&dossier, path).filter(|value| !value.is_null()) else {
            bail!("task {} has no value at {path}; linked objectives require a shared meta path", row.task.uri);
        };
        if matches!(value, Value::String(text) if text.trim().is_empty()) {
            bail!("task {} has an empty value at {path}", row.task.uri);
        }
        let include_self = input["include_self"].as_bool().unwrap_or(true);
        let full = input["full"].as_bool().unwrap_or(false);
        let mut related = Vec::new();
        for other in self.store.all()? {
            if !include_self && other.task.uri == row.task.uri {
                continue;
            }
            let other_value = other.value();
            if at_path(&other_value, path) != Some(value) {
                continue;
            }
            related.push(if full {
                serde_json::to_value(&other)?
            } else {
                json!({
                    "uri": other.task.uri,
                    "title": other.task.title,
                    "role": other.task.meta.get("role"),
                    "tags": other.task.tags,
                    "priority": other.task.priority,
                    "depends_on": other.task.depends_on,
                    "queue_priority": other.queue_priority,
                    "state": other.state,
                    "paused": other.paused,
                    "ready": self.store.ready(&other)?,
                    "owner": other.owner,
                    "error": other.error,
                    "revision": other.revision,
                })
            });
        }
        Ok(json!({
            "task": row.task.uri,
            "path": path,
            "value": value,
            "count": related.len(),
            "related": related,
        }))
    }

    fn spawn(&mut self, input: &Value) -> Result<Value> {
        let task = pipeline::spawn(&self.config, &mut self.store, input)?;
        self.status(&self.store.get(&task.uri)?)
    }

    fn rerun(&mut self, input: &Value) -> Result<Value> {
        let task = pipeline::rerun(&self.config, &mut self.store, input)?;
        self.status(&self.store.get(&task.uri)?)
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

    fn heartbeat(&self, input: &Value) -> Result<Value> {
        let lease = now() + input["lease_seconds"].as_i64().unwrap_or(900).clamp(30, 86_400);
        self.store.heartbeat(text(input, "task")?, text(input, "owner")?, lease)?;
        Ok(json!({"lease_until": lease}))
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

    fn prepare_workspace(&self, input: &Value) -> Result<Value> {
        let row = self.store.get(text(input, "task")?)?;
        if row.paused || (row.state != "running" && row.state != "blocked") {
            bail!("task must be claimed before preparing a workspace");
        }
        if let Some(path) = &row.worktree {
            if Path::new(path).exists() {
                return Ok(json!({
                    "provider": self.config.project.workspace_provider,
                    "branch": row.branch,
                    "worktree": path,
                    "path": path,
                    "reused": true,
                    "merged_dependencies": [],
                    "env": workspace::cache_env(&self.config.project),
                }));
            }
            self.store.workspace(&row.task.uri, &row.state, row.branch.as_deref(), None)?;
        }
        let mut deps = Vec::new();
        for dep_uri in &row.task.depends_on {
            let dep = self.store.get(dep_uri)?;
            if !matches!(dep.state.as_str(), "candidate" | "done") {
                bail!("dependency {dep_uri} is not ready for workspace sync");
            }
            let Some(branch) = dep.branch.clone() else {
                bail!("dependency {dep_uri} has no candidate branch");
            };
            deps.push((dep_uri.clone(), branch));
        }
        let occupied = self.store.live_workspaces()?;
        let prepared = workspace::prepare(
            &self.config.project,
            &self.config.project.repository,
            &row,
            input["base"].as_str(),
            &deps,
            &occupied,
        )?;
        if let Err(error) = self.store.workspace(
            &row.task.uri,
            &row.state,
            prepared["branch"].as_str(),
            prepared["worktree"].as_str().or_else(|| prepared["path"].as_str()),
        ) {
            if let Some(path) = prepared["worktree"].as_str().or_else(|| prepared["path"].as_str()) {
                let _ = workspace::destroy(&self.config.project.repository, Path::new(path), &self.config.project.workspace_provider);
            }
            return Err(error);
        }
        Ok(prepared)
    }

    fn workspace_status(&self, input: &Value) -> Result<Value> {
        workspace::status(&workspace::workspace_path(&self.store.get(text(input, "task")?)?)?)
    }

    fn workspace_diff(&self, input: &Value) -> Result<Value> {
        workspace::diff(&workspace::workspace_path(&self.store.get(text(input, "task")?)?)?)
    }

    fn workspace_destroy(&self, input: &Value) -> Result<Value> {
        let row = self.store.get(text(input, "task")?)?;
        if row.paused {
            bail!("paused tasks cannot destroy workspaces");
        }
        if let Some(path) = &row.worktree {
            workspace::destroy(&self.config.project.repository, Path::new(path), &self.config.project.workspace_provider)?;
            self.store.workspace(&row.task.uri, &row.state, row.branch.as_deref(), None)?;
        }
        Ok(json!({"task": row.task.uri, "destroyed": true}))
    }

    fn gate(&self, input: &Value, approval: bool) -> Result<Value> {
        let row = self.store.get(text(input, "task")?)?;
        if row.paused || row.state != "running" {
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
            json!({"gate": gate.id, "tree": tree, "ok": input["approved"].as_bool().unwrap_or(false), "approved_by": text(input, "by")?, "note": input["note"]})
        } else {
            serde_json::to_value(runtime::run_gate(gate, &row, cwd, &step.id, false)?)?
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
        if row.paused || row.state != "running" || row.owner.as_deref() != Some(text(input, "owner")?) {
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
            workspace::destroy(&self.config.project.repository, Path::new(path), &self.config.project.workspace_provider)?;
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
        let mut response = json!({
            "task": row.task.uri,
            "completed_step": step.id,
            "state": if done { "candidate" } else { "running" },
            "next_step": workflow.steps.get(next).map(|item| &item.id),
        });
        if done && let Some(receipt) = input.get("receipt").filter(|value| !value.is_null()) {
            response["receipt"] = self.capsules().publish_receipt_value(receipt)?;
        }
        Ok(response)
    }

    fn reconcile(&self) -> Result<Value> {
        let expired = self.store.expire(now())?;
        let mut destroyed_workspaces = 0usize;
        for row in self.store.all()? {
            if row.paused || !matches!(row.state.as_str(), "candidate" | "done" | "backlog") {
                continue;
            }
            let Some(path) = &row.worktree else {
                continue;
            };
            let _ = workspace::destroy(&self.config.project.repository, Path::new(path), &self.config.project.workspace_provider);
            self.store.workspace(&row.task.uri, &row.state, row.branch.as_deref(), None)?;
            destroyed_workspaces += 1;
        }
        runtime::git(&self.config.project.repository, &["worktree", "prune"])?;
        let gc = self.capsules().workspace_gc()?;
        Ok(json!({"expired_leases": expired, "destroyed_workspaces": destroyed_workspaces, "gc": gc}))
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
        let workflow = self.config.workflow(row.active_workflow.as_deref().or(row.task.workflow.as_deref()))?;
        let active = pipeline::resolve_active_step(&workflow, row.step, &row.task);
        let tree = row.worktree.as_deref().and_then(|path| runtime::tree(Path::new(path), false).ok());
        let gates = workflow
            .steps
            .get(row.step)
            .map(|step| {
                step.gates
                    .iter()
                    .filter_map(|id| self.config.gates.iter().find(|gate| &gate.id == id))
                    .filter(|gate| gate.when.matches(&row.value()))
                    .map(|gate| {
                        let proof = tree
                            .as_deref()
                            .map(|tree| self.store.proof(&row.task.uri, &step.id, &gate.id, tree))
                            .transpose()?
                            .flatten();
                        Ok(json!({"id":gate.id,"kind":gate.kind,"required":gate.required,"status":match proof {
                            Some(true) => "green",
                            Some(false) => "red",
                            None => "pending",
                        }}))
                    })
                    .collect::<Result<Vec<_>>>()
            })
            .transpose()?
            .unwrap_or_default();
        let receipt = self.store.latest_task_receipt(&row.task.uri)?.map(|(digest, _)| digest);
        Ok(json!({
            "task": row.task,
            "execution": {
                "state": row.state,
                "paused": row.paused,
                "queue_priority": row.queue_priority,
                "workflow": row.active_workflow,
                "step_index": row.step,
                "active_step": active,
                "max_runs": workflow.max_runs,
                "owner": row.owner,
                "lease_until": row.lease_until,
                "branch": row.branch,
                "worktree": row.worktree,
                "error": row.error,
                "revision": row.revision,
                "tree": tree,
                "gates": gates,
                "receipt_digest": receipt,
            }
        }))
    }
}

#[rustfmt::skip] fn text<'a>(value: &'a Value, name: &str) -> Result<&'a str> { value[name].as_str().with_context(|| format!("missing {name}")) }
#[rustfmt::skip] fn strings(value: &Value, name: &str) -> Vec<String> { value[name].as_array().into_iter().flatten().filter_map(Value::as_str).map(str::to_owned).collect() }
#[rustfmt::skip] fn now() -> i64 { std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_secs() as i64 }
#[rustfmt::skip] fn at_path<'a>(root: &'a Value, path: &str) -> Option<&'a Value> { path.split('.').try_fold(root, |value, key| value.get(key)) }
