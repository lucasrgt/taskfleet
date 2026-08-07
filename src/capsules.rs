use std::path::Path;

use anyhow::{Context, Result, bail};
use serde_json::{Value, json};

use crate::{cas::Cas, model::Config, receipt, store::Store, workspace};

pub struct Capsules<'a> {
    pub config: &'a Config,
    pub store: &'a Store,
    pub cas: &'a Cas,
}

impl Capsules<'_> {
    pub fn task_context(&self, input: &Value) -> Result<Value> {
        let row = self.store.get(text(input, "task")?)?;
        let include = strings(input, "include");
        let budget = input["budget_bytes"].as_u64().unwrap_or(1_048_576);
        let mut deps = Vec::new();
        for dep_uri in &row.task.depends_on {
            let (digest, receipt) = self
                .store
                .latest_task_receipt(dep_uri)?
                .with_context(|| format!("dependency {dep_uri} has no published receipt"))?;
            deps.push((dep_uri.clone(), digest, receipt));
        }
        receipt::context_for(&row.task.uri, &deps, &include, budget)
    }

    pub fn artifact_publish(&self, input: &Value) -> Result<Value> {
        let media_type = input["media_type"].as_str().unwrap_or("application/octet-stream");
        let (digest, size, path) = if let Some(bytes) = input["bytes"].as_str() {
            self.cas.put_bytes(bytes.as_bytes(), media_type)?
        } else if let Some(path) = input["path"].as_str() {
            self.cas.put_path(Path::new(path), media_type)?
        } else {
            bail!("artifact.publish requires bytes or path");
        };
        self.store.record_blob(&digest, size, media_type, &path.display().to_string())?;
        if input["pin"].as_bool().unwrap_or(false) {
            self.store.pin_blob(&digest, input["note"].as_str())?;
        }
        Ok(json!({"digest": digest, "size": size, "media_type": media_type, "path": path}))
    }

    pub fn artifact_resolve(&self, input: &Value) -> Result<Value> {
        let digest = text(input, "digest")?;
        let path = self.cas.resolve(digest)?;
        let meta = self.store.blob(digest)?;
        Ok(json!({"digest": digest, "path": path, "meta": meta}))
    }

    pub fn artifact_materialize(&self, input: &Value) -> Result<Value> {
        let digest = text(input, "digest")?;
        let destination = Path::new(text(input, "path")?);
        workspace::ensure_parent(destination)?;
        let path = self.cas.materialize(digest, destination)?;
        Ok(json!({"digest": digest, "path": path}))
    }

    pub fn receipt_publish(&self, input: &Value) -> Result<Value> {
        self.publish_receipt_value(input.get("receipt").unwrap_or(input))
    }

    pub fn publish_receipt_value(&self, receipt: &Value) -> Result<Value> {
        receipt::validate(receipt)?;
        let task_uri = text(receipt, "task_uri")?;
        let row = self.store.get(task_uri)?;
        if !matches!(row.state.as_str(), "candidate" | "done" | "running") {
            bail!("task receipt requires running, candidate, or done task");
        }
        let digest = receipt::digest(receipt)?;
        let (blob_digest, size, path) = self.cas.put_bytes(serde_json::to_string(receipt)?.as_bytes(), "application/json")?;
        self.store.record_blob(&blob_digest, size, "application/json", &path.display().to_string())?;
        self.store.publish_task_receipt(&digest, receipt)?;
        Ok(json!({"digest": digest, "blob_digest": blob_digest, "task": task_uri}))
    }

    pub fn receipt_get(&self, input: &Value) -> Result<Value> {
        if let Some(digest) = input["digest"].as_str() {
            return Ok(json!({"digest": digest, "receipt": self.store.task_receipt(digest)?}));
        }
        let task = text(input, "task")?;
        let (digest, receipt) = self.store.latest_task_receipt(task)?.with_context(|| format!("no receipt for {task}"))?;
        Ok(json!({"digest": digest, "receipt": receipt}))
    }

    pub fn receipt_resolve_dependencies(&self, input: &Value) -> Result<Value> {
        let digest = text(input, "digest")?;
        let deps = self
            .store
            .receipt_dependencies(digest)?
            .into_iter()
            .map(|(task_uri, receipt_digest)| {
                Ok(json!({
                    "task_uri": task_uri,
                    "receipt_digest": receipt_digest,
                    "receipt": self.store.task_receipt(&receipt_digest)?,
                }))
            })
            .collect::<Result<Vec<_>>>()?;
        Ok(json!({"digest": digest, "dependencies": deps}))
    }

    pub fn workspace_gc(&self) -> Result<Value> {
        let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_secs() as i64;
        let mut keep = self.store.reachable_digests(self.config.project.cas_retention_seconds, now)?;
        if let Some(max_bytes) = self.config.project.cas_max_bytes {
            let mut entries = Vec::new();
            let mut total = 0u64;
            for digest in keep.iter() {
                if let Ok(path) = self.cas.resolve(digest) {
                    let size = std::fs::metadata(path)?.len();
                    total = total.saturating_add(size);
                    entries.push((digest.clone(), size));
                }
            }
            if total > max_bytes {
                entries.sort_by_key(|entry| entry.1);
                for (digest, size) in entries {
                    if total <= max_bytes {
                        break;
                    }
                    if self.store.is_pinned(&digest)? {
                        continue;
                    }
                    keep.remove(&digest);
                    total = total.saturating_sub(size);
                }
            }
        }
        let removed_files = self.cas.sweep(&keep)?;
        let removed_rows = self.store.delete_blob_rows(&keep)?;
        Ok(json!({"removed_files": removed_files, "removed_rows": removed_rows, "kept": keep.len()}))
    }
}

fn text<'a>(value: &'a Value, name: &str) -> Result<&'a str> {
    value[name].as_str().with_context(|| format!("missing {name}"))
}

fn strings(value: &Value, name: &str) -> Vec<String> {
    value[name]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .map(str::to_owned)
        .collect()
}
