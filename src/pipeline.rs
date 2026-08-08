use anyhow::{Context, Result, bail};
use serde_json::{Map, Value, json};

use crate::{
    model::{Config, Step, Task, Workflow},
    store::Store,
};

pub fn merge_args(workflow: &Value, step: &Value, run: &Value) -> Value {
    let mut out = Map::new();
    for source in [workflow, step, run] {
        if let Some(map) = source.as_object() {
            for (key, value) in map {
                out.insert(key.clone(), value.clone());
            }
        }
    }
    Value::Object(out)
}

pub fn run_args(task: &Task) -> Value {
    task.meta.get("args").cloned().filter(|value| value.is_object()).unwrap_or_else(|| json!({}))
}

pub fn resolve_active_step(workflow: &Workflow, step_index: usize, task: &Task) -> Option<Value> {
    let step = workflow.steps.get(step_index)?;
    Some(resolved_step(workflow, step, task))
}

pub fn resolved_step(workflow: &Workflow, step: &Step, task: &Task) -> Value {
    json!({
        "id": step.id,
        "title": step.title,
        "instruction": step.instruction,
        "gates": step.gates,
        "args": merge_args(&workflow.args, &step.args, &run_args(task)),
    })
}

pub fn spawn(config: &Config, store: &mut Store, input: &Value) -> Result<Task> {
    let workflow_id = text(input, "workflow")?;
    let workflow = config.workflow(Some(workflow_id))?;
    let mut meta = match input.get("meta").cloned() {
        Some(Value::Object(map)) => Value::Object(map),
        Some(_) => bail!("meta must be an object"),
        None => json!({}),
    };
    let args = match input.get("args").cloned() {
        Some(Value::Object(map)) => Value::Object(map),
        Some(_) => bail!("args must be an object"),
        None => meta.get("args").cloned().unwrap_or_else(|| json!({})),
    };
    if !args.is_object() {
        bail!("args must be an object");
    }
    let series = input["series"].as_str();
    let count = count_runs(store, workflow_id, series)?;
    if workflow.max_runs != 0 && count >= workflow.max_runs as usize {
        bail!("pipeline {workflow_id} reached max_runs {}", workflow.max_runs);
    }
    let uri = input["uri"]
        .as_str()
        .map(str::to_owned)
        .unwrap_or_else(|| format!("pipeline://{workflow_id}/{}", nonce()));
    let title = input["title"].as_str().unwrap_or(workflow_id).to_owned();
    let run = input["run"].as_u64().unwrap_or((count as u64).saturating_add(1));
    let meta_map = meta.as_object_mut().context("meta must be an object")?;
    meta_map.insert("pipeline".into(), json!(workflow_id));
    meta_map.insert("run".into(), json!(run));
    meta_map.insert("args".into(), args);
    if let Some(series) = series {
        meta_map.insert("series".into(), json!(series));
    }
    let task = Task {
        uri,
        title,
        description: input["description"].as_str().unwrap_or("").to_owned(),
        tags: input["tags"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .map(str::to_owned)
            .collect(),
        priority: input["priority"].as_str().map(str::to_owned),
        source: input.get("source").cloned().unwrap_or_else(|| json!({"provider":"pipeline"})),
        meta,
        depends_on: input["depends_on"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .map(str::to_owned)
            .collect(),
        workflow: Some(workflow.id),
    };
    store.ingest(std::slice::from_ref(&task))?;
    Ok(task)
}

pub fn rerun(config: &Config, store: &mut Store, input: &Value) -> Result<Task> {
    let source = store.get(text(input, "task")?)?;
    if source.state == "running" {
        bail!("cannot rerun a running task; pause, finish, or cancel it first");
    }
    if let Some(args) = input.get("args")
        && !args.is_object()
    {
        bail!("args must be an object");
    }
    let pipeline_id = source
        .task
        .meta
        .get("pipeline")
        .and_then(Value::as_str)
        .or(source.active_workflow.as_deref())
        .or(source.task.workflow.as_deref())
        .context("task has no pipeline workflow to rerun")?;
    let workflow = config.workflow(Some(pipeline_id))?;
    let series = source.task.meta.get("series").and_then(Value::as_str).or_else(|| input["series"].as_str());
    let count = count_runs(store, pipeline_id, series)?;
    let count = if source.task.meta.get("pipeline").and_then(Value::as_str) == Some(pipeline_id) {
        count
    } else {
        count.saturating_add(1)
    };
    if workflow.max_runs != 0 && count >= workflow.max_runs as usize {
        bail!("pipeline {pipeline_id} reached max_runs {}", workflow.max_runs);
    }
    let mut args = run_args(&source.task);
    if let Some(Value::Object(extra)) = input.get("args").cloned()
        && let Some(map) = args.as_object_mut()
    {
        for (key, value) in extra {
            map.insert(key, value);
        }
    }
    let mut spawn_input = json!({
        "workflow": pipeline_id,
        "args": args,
        "run": count + 1,
        "title": input["title"].as_str().unwrap_or(&source.task.title),
        "description": input["description"].as_str().unwrap_or(&source.task.description),
        "tags": source.task.tags,
        "priority": source.task.priority,
        "source": source.task.source,
        "depends_on": source.task.depends_on,
    });
    if let Some(uri) = input["uri"].as_str() {
        spawn_input["uri"] = json!(uri);
    }
    if let Some(series) = series {
        spawn_input["series"] = json!(series);
    }
    if let Some(meta) = input.get("meta") {
        spawn_input["meta"] = meta.clone();
    }
    spawn(config, store, &spawn_input)
}

pub fn count_runs(store: &Store, pipeline_id: &str, series: Option<&str>) -> Result<usize> {
    let mut count = 0usize;
    for row in store.all()? {
        if row.task.meta.get("pipeline").and_then(Value::as_str) != Some(pipeline_id) {
            continue;
        }
        if let Some(series) = series
            && row.task.meta.get("series").and_then(Value::as_str) != Some(series)
        {
            continue;
        }
        count += 1;
    }
    Ok(count)
}

fn text<'a>(value: &'a Value, name: &str) -> Result<&'a str> {
    value[name].as_str().with_context(|| format!("missing {name}"))
}

fn nonce() -> String {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    format!("{nanos:x}")
}
