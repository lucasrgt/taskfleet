use std::{
    fs,
    path::{Path, PathBuf},
};

use anyhow::{Result, bail};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

#[rustfmt::skip]
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Config { #[serde(default = "schema")] pub schema: u32, #[serde(default)] pub project: Project, #[serde(default, rename = "view")] pub views: Vec<View>, #[serde(default, rename = "workflow")] pub workflows: Vec<Workflow>, #[serde(default, rename = "gate")] pub gates: Vec<Gate>, #[serde(default, rename = "route")] pub routes: Vec<Route> }

#[rustfmt::skip]
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Project {
    #[serde(default = "dot")] pub repository: PathBuf,
    #[serde(default = "database")] pub database: PathBuf,
    #[serde(default = "worktrees")] pub worktree_root: PathBuf,
    #[serde(default = "cas_root")] pub cas_root: PathBuf,
    #[serde(default = "workspace_provider")] pub workspace_provider: String,
    #[serde(default)] pub shared_caches: std::collections::BTreeMap<String, PathBuf>,
    #[serde(default = "cas_retention")] pub cas_retention_seconds: i64,
    pub cas_max_bytes: Option<u64>,
    #[serde(default)] pub agentfs_bin: Option<PathBuf>,
    #[serde(default)] pub max_parallel_workspaces: u32,
    #[serde(default = "retain_integration")] pub retain_integration_worktree: bool,
    pub default_workflow: Option<String>,
}

#[rustfmt::skip]
impl Default for Project {
    fn default() -> Self {
        Self {
            repository: dot(), database: database(), worktree_root: worktrees(), cas_root: cas_root(),
            workspace_provider: workspace_provider(), shared_caches: Default::default(),
            cas_retention_seconds: cas_retention(), cas_max_bytes: None, agentfs_bin: None,
            max_parallel_workspaces: 0, retain_integration_worktree: retain_integration(), default_workflow: None,
        }
    }
}

#[rustfmt::skip]
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct View { pub id: String, #[serde(default)] pub filter: Filter }

#[rustfmt::skip]
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Workflow {
    pub id: String,
    #[serde(default, rename = "step")]
    pub steps: Vec<Step>,
    #[serde(default = "object")]
    pub args: Value,
    #[serde(default = "max_runs")]
    pub max_runs: u32,
}

#[rustfmt::skip]
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Step {
    pub id: String,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub gates: Vec<String>,
    #[serde(default)]
    pub instruction: String,
    #[serde(default = "object")]
    pub args: Value,
}

#[rustfmt::skip]
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Gate { pub id: String, #[serde(default = "command_kind")] pub kind: String, #[serde(default)] pub command: Vec<String>, #[serde(default = "gate_events")] pub events: Vec<String>, #[serde(default)] pub when: Filter, #[serde(default = "timeout")] pub timeout_seconds: u64, #[serde(default = "required")] pub required: bool }

#[rustfmt::skip]
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Route { pub workflow: String, #[serde(default)] pub when: Filter }

#[rustfmt::skip]
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum Filter {
    #[default] True,
    Eq { path: String, value: Value }, Ne { path: String, value: Value }, Gt { path: String, value: Value }, Gte { path: String, value: Value },
    Lt { path: String, value: Value }, Lte { path: String, value: Value }, Contains { path: String, value: Value }, In { path: String, values: Vec<Value> },
    Exists { path: String }, And { args: Vec<Filter> }, Or { args: Vec<Filter> }, Not { arg: Box<Filter> },
}

#[rustfmt::skip]
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Task { pub uri: String, pub title: String, #[serde(default)] pub description: String, #[serde(default)] pub tags: Vec<String>, pub priority: Option<String>, #[serde(default = "object")] pub source: Value, #[serde(default = "object")] pub meta: Value, #[serde(default)] pub depends_on: Vec<String>, pub workflow: Option<String> }

#[rustfmt::skip]
#[derive(Clone, Debug, Serialize)]
pub struct TaskRow { #[serde(flatten)] pub task: Task, pub state: String, pub paused: bool, pub queue_priority: i64, pub active_workflow: Option<String>, pub step: usize, pub owner: Option<String>, pub lease_until: Option<i64>, pub branch: Option<String>, pub worktree: Option<String>, pub error: Option<String>, pub revision: u64 }

impl Config {
    pub fn load(path: &Path) -> Result<Self> {
        let mut value: Self = toml::from_str(&fs::read_to_string(path)?)?;
        if value.schema != 1 {
            bail!("unsupported config schema {}", value.schema);
        }
        let base = path.parent().unwrap_or_else(|| Path::new("."));
        value.project.repository = absolute(base, &value.project.repository);
        value.project.database = absolute(base, &value.project.database);
        value.project.worktree_root = absolute(base, &value.project.worktree_root);
        value.project.cas_root = absolute(base, &value.project.cas_root);
        value.project.shared_caches = value
            .project
            .shared_caches
            .into_iter()
            .map(|(key, path)| (key, absolute(base, &path)))
            .collect();
        if let Some(path) = &value.project.agentfs_bin {
            value.project.agentfs_bin = Some(absolute(base, path));
        }
        if !matches!(value.project.workspace_provider.as_str(), "git-worktree" | "agentfs" | "reflink") {
            bail!("unsupported workspace_provider {}", value.project.workspace_provider);
        }
        unique("view", value.views.iter().map(|item| item.id.as_str()))?;
        unique("workflow", value.workflows.iter().map(|item| item.id.as_str()))?;
        unique("gate", value.gates.iter().map(|item| item.id.as_str()))?;
        for gate in &value.gates {
            if !matches!(gate.kind.as_str(), "command" | "approval") {
                bail!("unsupported gate kind {}", gate.kind);
            }
            if gate.kind == "command" && gate.command.is_empty() {
                bail!("command gate {} has no command", gate.id);
            }
        }
        for workflow in &value.workflows {
            if workflow.steps.is_empty() {
                bail!("workflow {} has no steps", workflow.id);
            }
            unique("step", workflow.steps.iter().map(|item| item.id.as_str()))?;
            for step in &workflow.steps {
                for gate in &step.gates {
                    if !value.gates.iter().any(|item| &item.id == gate) {
                        bail!("unknown gate {gate}");
                    }
                }
            }
        }
        for route in &value.routes {
            if !value.workflows.iter().any(|item| item.id == route.workflow) {
                bail!("route references unknown workflow {}", route.workflow);
            }
        }
        if let Some(id) = &value.project.default_workflow
            && !value.workflows.iter().any(|item| &item.id == id)
        {
            bail!("unknown default workflow {id}");
        }
        Ok(value)
    }

    pub fn workflow(&self, id: Option<&str>) -> Result<Workflow> {
        match id {
            Some(id) => self
                .workflows
                .iter()
                .find(|item| item.id == id)
                .cloned()
                .ok_or_else(|| anyhow::anyhow!("unknown workflow {id}")),
            None => Ok(implicit_workflow()),
        }
    }

    pub fn route(&self, task: &Value, explicit: Option<&str>) -> Option<String> {
        explicit
            .map(str::to_owned)
            .or_else(|| self.routes.iter().find(|route| route.when.matches(task)).map(|route| route.workflow.clone()))
            .or_else(|| self.project.default_workflow.clone())
    }
}

impl Filter {
    pub fn matches(&self, root: &Value) -> bool {
        let at = |path: &str| path.split('.').try_fold(root, |value, key| value.get(key));
        let cmp = |path: &str, expected: &Value, op: fn(std::cmp::Ordering) -> bool| at(path).and_then(|value| compare(value, expected)).is_some_and(op);
        match self {
            Self::True => true,
            Self::Eq { path, value } => at(path) == Some(value),
            Self::Ne { path, value } => at(path).is_some_and(|actual| actual != value),
            Self::Gt { path, value } => cmp(path, value, |o| o.is_gt()),
            Self::Gte { path, value } => cmp(path, value, |o| o.is_ge()),
            Self::Lt { path, value } => cmp(path, value, |o| o.is_lt()),
            Self::Lte { path, value } => cmp(path, value, |o| o.is_le()),
            Self::Contains { path, value } => at(path).is_some_and(|actual| match actual {
                Value::Array(items) => items.contains(value),
                Value::String(text) => value.as_str().is_some_and(|needle| text.contains(needle)),
                Value::Object(map) => value.as_str().is_some_and(|key| map.contains_key(key)),
                _ => false,
            }),
            Self::In { path, values } => at(path).is_some_and(|actual| values.contains(actual)),
            Self::Exists { path } => at(path).is_some(),
            Self::And { args } => args.iter().all(|arg| arg.matches(root)),
            Self::Or { args } => args.iter().any(|arg| arg.matches(root)),
            Self::Not { arg } => !arg.matches(root),
        }
    }
}

impl TaskRow {
    pub fn value(&self) -> Value {
        let mut value = serde_json::to_value(&self.task).unwrap_or(Value::Null);
        if let Some(map) = value.as_object_mut() {
            map.insert("execution".into(), json!({"state":self.state,"paused":self.paused,"queue_priority":self.queue_priority,"workflow":self.active_workflow,"step":self.step,"owner":self.owner,"lease_until":self.lease_until,"branch":self.branch,"worktree":self.worktree,"error":self.error,"revision":self.revision}));
        }
        value
    }
}

fn compare(left: &Value, right: &Value) -> Option<std::cmp::Ordering> {
    match (left, right) {
        (Value::Number(a), Value::Number(b)) => a.as_f64()?.partial_cmp(&b.as_f64()?),
        (Value::String(a), Value::String(b)) => Some(a.cmp(b)),
        _ => None,
    }
}

#[rustfmt::skip] fn implicit_workflow() -> Workflow {
    Workflow {
        id: "implicit".into(),
        steps: vec![Step {
            id: "execute".into(),
            title: "Execute".into(),
            gates: vec![],
            instruction: String::new(),
            args: object(),
        }],
        args: object(),
        max_runs: 1,
    }
}
#[rustfmt::skip] fn absolute(base: &Path, path: &Path) -> PathBuf { if path.is_absolute() { path.into() } else { base.join(path) } }
#[rustfmt::skip] fn schema() -> u32 { 1 }
#[rustfmt::skip] fn dot() -> PathBuf { ".".into() }
#[rustfmt::skip] fn database() -> PathBuf { ".taskfleet/state.sqlite".into() }
#[rustfmt::skip] fn worktrees() -> PathBuf { "../.taskfleet-worktrees".into() }
#[rustfmt::skip] fn cas_root() -> PathBuf { ".taskfleet/cas".into() }
#[rustfmt::skip] fn workspace_provider() -> String { "git-worktree".into() }
#[rustfmt::skip] fn cas_retention() -> i64 { 604_800 }
#[rustfmt::skip] fn retain_integration() -> bool { false }
#[rustfmt::skip] fn command_kind() -> String { "command".into() }
#[rustfmt::skip] fn gate_events() -> Vec<String> { vec!["step.complete".into()] }
#[rustfmt::skip] fn timeout() -> u64 { 900 }
#[rustfmt::skip] fn required() -> bool { true }
#[rustfmt::skip] fn object() -> Value { json!({}) }
#[rustfmt::skip] fn max_runs() -> u32 { 1 }
#[rustfmt::skip] fn unique<'a>(kind: &str, values: impl Iterator<Item = &'a str>) -> Result<()> { let mut seen = std::collections::HashSet::new(); for value in values { if !seen.insert(value) { bail!("duplicate {kind} id {value}"); } } Ok(()) }
