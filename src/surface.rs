use std::{
    ffi::OsString,
    io::{self, BufRead, Read, Write},
    path::{Path, PathBuf},
};

use anyhow::{Result, bail};
use serde_json::{Value, json};

use crate::{location, service::Service};

#[rustfmt::skip]
const METHODS: &[(&str, &str, &[&str])] = &[
    ("view.list", "List saved views", &[]),
    ("task.ingest", "Idempotently ingest tasks from the tasks array", &["tasks"]),
    ("task.query", "Query compact or full tasks by view, structured filter, state, readiness and limit", &[]),
    ("task.get", "Get one complete task dossier and execution state", &["task"]),
    ("task.claim", "Atomically lease ready tasks selected by view/filter", &["owner"]),
    ("task.heartbeat", "Renew a task lease owned by a worker", &["task", "owner"]),
    ("task.cancel", "Durably cancel a non-completed task", &["task"]),
    ("task.pause", "Hold a non-completed task without losing progress", &["task"]),
    ("task.resume", "Release a paused task for its lifecycle state", &["task"]),
    ("task.reprioritize", "Set the durable operational queue priority", &["task", "priority"]),
    ("worktree.prepare", "Create or reuse the isolated Git branch and worktree for a claimed task", &["task"]),
    ("gate.run", "Run a command gate for the active step and record a tree-bound receipt", &["task", "gate"]),
    ("gate.approve", "Record an explicit human approval gate receipt", &["task", "gate", "by", "approved"]),
    ("step.advance", "Advance only when all required gates are green for the current Git tree", &["task", "owner"]),
    ("task.block", "Block a task with a visible reason", &["task", "reason"]),
    ("task.retry", "Return a blocked task to the claimable backlog", &["task"]),
    ("integration.run", "Merge candidate branches sequentially and re-prove integration gates", &[]),
    ("reconcile", "Expire dead leases and prune stale Git worktree records", &[]),
];

pub fn run_cli_at(args: Vec<OsString>, current: &Path, input: &mut dyn Read, output: &mut dyn Write) -> Result<i32> {
    let mut args = args.into_iter().skip(1).map(|arg| arg.to_string_lossy().into_owned()).collect::<Vec<_>>();
    let explicit = take_option(&mut args, "--config").map(PathBuf::from);
    let home = take_option(&mut args, "--state-home").map(PathBuf::from);
    let inline = take_option(&mut args, "--input");
    let method = args.first().map(String::as_str).unwrap_or("help");
    match method {
        "help" | "--help" | "-h" => writeln!(
            output,
            "taskfleet <method> [--config PATH] [--input JSON]\nmethods: init, locate, external <enable|disable|purge>, mcp, methods, {}",
            METHODS.iter().map(|item| item.0).collect::<Vec<_>>().join(", ")
        )?,
        "--version" | "-V" => writeln!(output, "taskfleet {}", env!("CARGO_PKG_VERSION"))?,
        "init" => {
            let config = explicit.clone().unwrap_or_else(|| current.join("taskfleet.toml"));
            if config.exists() {
                bail!("config already exists: {}", config.display());
            }
            std::fs::write(&config, include_str!("../assets/taskfleet.example.toml"))?;
            writeln!(output, "{}", json!({"config":config,"initialized":true}))?;
        }
        "locate" => write_json(output, &location::locate(current, explicit.as_deref(), home.as_deref())?)?,
        "external" => write_json(
            output,
            &location::manage(current, home.as_deref(), args.get(1).map(String::as_str).unwrap_or("status"))?,
        )?,
        "methods" => write_json(output, &tools())?,
        "mcp" => {
            let location = location::locate(current, explicit.as_deref(), home.as_deref())?;
            if !location.enabled {
                bail!("Taskfleet is not enabled; run `taskfleet external enable`");
            }
            mcp_stream(&location.config, &mut io::BufReader::new(input), output)?;
        }
        method if METHODS.iter().any(|item| item.0 == method) => {
            let value = match inline {
                Some(value) => serde_json::from_str(&value)?,
                None => {
                    let mut text = String::new();
                    input.read_to_string(&mut text)?;
                    if text.trim().is_empty() { json!({}) } else { serde_json::from_str(&text)? }
                }
            };
            let location = location::locate(current, explicit.as_deref(), home.as_deref())?;
            if !location.enabled {
                bail!("Taskfleet is not enabled; run `taskfleet external enable`");
            }
            write_json(output, &Service::open(&location.config)?.call(method, &value)?)?;
        }
        method => bail!("unknown method {method}"),
    }
    Ok(0)
}

pub fn mcp_stream(config: &Path, input: &mut dyn BufRead, output: &mut dyn Write) -> Result<()> {
    let mut service = Service::open(config)?;
    for line in input.lines() {
        let request: Value = serde_json::from_str(&line?)?;
        let Some(id) = request.get("id").cloned() else {
            continue;
        };
        let response = match request["method"].as_str().unwrap_or("") {
            "initialize" => {
                json!({"protocolVersion":"2025-06-18","capabilities":{"tools":{"listChanged":false}},"serverInfo":{"name":"taskfleet","version":env!("CARGO_PKG_VERSION")}})
            }
            "tools/list" => json!({"tools":tools()}),
            "tools/call" => {
                let name = request["params"]["name"].as_str().unwrap_or("");
                let method = name.strip_prefix("taskfleet_").unwrap_or("").replace('_', ".");
                match service.call(&method, &request["params"]["arguments"]) {
                    Ok(value) => json!({"content":[{"type":"text","text":serde_json::to_string(&value)?}]}),
                    Err(error) => json!({"content":[{"type":"text","text":format!("{error:#}")}],"isError":true}),
                }
            }
            method => json!({"code":-32601,"message":format!("unknown method {method}")}),
        };
        let envelope = if response.get("code").is_some() {
            json!({"jsonrpc":"2.0","id":id,"error":response})
        } else {
            json!({"jsonrpc":"2.0","id":id,"result":response})
        };
        writeln!(output, "{}", serde_json::to_string(&envelope)?)?;
    }
    Ok(())
}

pub fn tools() -> Vec<Value> {
    METHODS.iter().map(|(method, description, required)| {
        let name = format!("taskfleet_{}", method.replace('.', "_"));
        let properties = required.iter().chain(optionals(method)).map(|name| ((*name).to_owned(), match *name { "approved" | "ready" | "full" => json!({"type":"boolean"}), "limit" | "lease_seconds" | "priority" => json!({"type":"integer"}), "tasks" => json!({"type":"array","items":{"type":"object"}}), "states" => json!({"type":"array","items":{"type":"string"}}), "filter" => json!({"type":"object"}), _ => json!({"type":"string"}) })).collect::<serde_json::Map<_,_>>();
        json!({"name":name,"description":description,"inputSchema":{"type":"object","properties":properties,"required":required,"additionalProperties":true}})
    }).collect()
}

#[rustfmt::skip] fn optionals(method: &str) -> &'static [&'static str] { match method { "task.query" => &["view","filter","states","ready","full","limit"], "task.claim" => &["view","filter","limit","lease_seconds"], "task.heartbeat" => &["lease_seconds"], "task.cancel" => &["reason"], "worktree.prepare" => &["base"], "gate.approve" => &["note"], "integration.run" => &["view","filter","base","branch"], _ => &[] } }

fn write_json(output: &mut dyn Write, value: &impl serde::Serialize) -> Result<()> {
    writeln!(output, "{}", serde_json::to_string_pretty(value)?)?;
    Ok(())
}

fn take_option(args: &mut Vec<String>, name: &str) -> Option<String> {
    let index = args.iter().position(|arg| arg == name)?;
    if index + 1 >= args.len() {
        return None;
    }
    args.remove(index);
    Some(args.remove(index))
}
