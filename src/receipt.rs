use anyhow::{Context, Result, bail};
use serde_json::{Value, json};

use crate::cas::{digest_str, is_digest};

pub fn validate(receipt: &Value) -> Result<()> {
    require_object(receipt)?;
    if receipt["schema_version"].as_u64() != Some(1) {
        bail!("receipt schema_version must be 1");
    }
    for key in ["task_uri", "step_id", "created_at"] {
        if text(receipt, key)?.is_empty() {
            bail!("receipt {key} is required");
        }
    }
    let producer = object(receipt, "producer")?;
    text(producer, "agent_id")?;
    text(producer, "harness")?;
    let source = object(receipt, "source")?;
    text(source, "base_tree")?;
    text(source, "result_tree")?;
    let provider = text(source, "workspace_provider")?;
    if !matches!(provider, "git-worktree" | "agentfs" | "overlayfs" | "reflink" | "jj-workspace") {
        bail!("unsupported workspace_provider {provider}");
    }
    let changes = object(receipt, "changes")?;
    if !changes["paths"].is_array() {
        bail!("changes.paths must be an array");
    }
    digest_field(changes, "patch_digest")?;
    let exports = object(receipt, "context_exports")?;
    text(exports, "summary")?;
    for key in ["decisions", "assumptions", "symbols", "contracts", "followups"] {
        if !exports[key].is_array() {
            bail!("context_exports.{key} must be an array");
        }
    }
    let fingerprint = object(receipt, "fingerprint")?;
    digest_field(fingerprint, "input_digest")?;
    if !fingerprint["lockfiles"].is_object() || !fingerprint["toolchain"].is_object() {
        bail!("fingerprint.lockfiles and toolchain must be objects");
    }
    for dep in array(receipt, "dependencies")? {
        text(dep, "task_uri")?;
        digest_field(dep, "receipt_digest")?;
    }
    for artifact in array(receipt, "artifacts")? {
        text(artifact, "name")?;
        digest_field(artifact, "digest")?;
        text(artifact, "media_type")?;
        if artifact["size"].as_u64().is_none() {
            bail!("artifact size must be an integer");
        }
        let role = text(artifact, "role")?;
        if !matches!(role, "input" | "output" | "proof" | "context" | "log") {
            bail!("unsupported artifact role {role}");
        }
    }
    for proof in array(receipt, "proofs")? {
        text(proof, "gate")?;
        let status = text(proof, "status")?;
        if !matches!(status, "green" | "red" | "approved" | "rejected") {
            bail!("unsupported proof status {status}");
        }
        text(proof, "tree")?;
        digest_field(proof, "command_digest")?;
        for key in ["stdout_digest", "stderr_digest"] {
            if !proof[key].is_null() && proof.get(key).is_some() {
                digest_field(proof, key)?;
            }
        }
    }
    Ok(())
}

pub fn digest(receipt: &Value) -> Result<String> {
    Ok(digest_str(&serde_json::to_string(receipt)?))
}

pub fn context_for(task_uri: &str, dependencies: &[(String, String, Value)], include: &[String], budget_bytes: u64) -> Result<Value> {
    let mut used = 0u64;
    let mut items = Vec::new();
    for (dep_uri, receipt_digest, receipt) in dependencies {
        let mut item = json!({
            "task_uri": dep_uri,
            "receipt_digest": receipt_digest,
        });
        let want_exports = include.is_empty() || include.iter().any(|rule| rule == "dependency.receipts.context_exports" || rule == "*");
        let want_paths = include.is_empty() || include.iter().any(|rule| rule == "dependency.changes.paths" || rule == "*");
        let want_proofs = include.iter().any(|rule| rule.starts_with("dependency.artifacts[role=proof]") || rule == "*");
        let want_all_artifacts = include.is_empty() || include.iter().any(|rule| rule == "dependency.artifacts" || rule == "*");
        if want_exports {
            item["context_exports"] = receipt["context_exports"].clone();
        }
        if want_paths {
            item["changes"] = json!({"paths": receipt["changes"]["paths"].clone()});
        }
        if want_all_artifacts || want_proofs {
            let artifacts = receipt["artifacts"]
                .as_array()
                .into_iter()
                .flatten()
                .filter(|artifact| want_all_artifacts || artifact["role"].as_str() == Some("proof"))
                .cloned()
                .collect::<Vec<_>>();
            item["artifacts"] = Value::Array(artifacts);
        }
        if include.is_empty() || include.iter().any(|rule| rule == "dependency.proofs" || rule == "*") {
            item["proofs"] = receipt["proofs"].clone();
        }
        let encoded = serde_json::to_vec(&item)?;
        used = used.saturating_add(encoded.len() as u64);
        if used > budget_bytes {
            bail!("task.context exceeded budget_bytes {budget_bytes}");
        }
        items.push(item);
    }
    Ok(json!({"task": task_uri, "dependencies": items, "bytes": used}))
}

fn require_object(value: &Value) -> Result<()> {
    if !value.is_object() {
        bail!("receipt must be an object");
    }
    Ok(())
}

fn object<'a>(value: &'a Value, name: &str) -> Result<&'a Value> {
    value
        .get(name)
        .filter(|item| item.is_object())
        .with_context(|| format!("missing object {name}"))
}

fn array<'a>(value: &'a Value, name: &str) -> Result<&'a Vec<Value>> {
    value[name].as_array().with_context(|| format!("missing array {name}"))
}

fn text<'a>(value: &'a Value, name: &str) -> Result<&'a str> {
    value[name].as_str().with_context(|| format!("missing {name}"))
}

fn digest_field(value: &Value, name: &str) -> Result<()> {
    let digest = text(value, name)?;
    if !is_digest(digest) {
        bail!("{name} must be sha256 digest");
    }
    Ok(())
}
