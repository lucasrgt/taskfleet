mod common;

use std::io::Cursor;

use assert_cmd::Command;
use serde_json::{Value, json};

use common::{Fixture, task};

fn cli(current: &std::path::Path, args: &[&str], input: &str) -> anyhow::Result<String> {
    let mut output = Vec::new();
    let arguments = std::iter::once("taskfleet").chain(args.iter().copied()).map(Into::into).collect();
    taskfleet::run_cli_at(arguments, current, &mut Cursor::new(input), &mut output)?;
    Ok(String::from_utf8(output).unwrap())
}

#[test]
fn mcp_advertises_and_calls_the_same_core() {
    let fixture = Fixture::new("[[view]]\nid='all'\n");
    let requests = [
        json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}),
        json!({"jsonrpc":"2.0","method":"notifications/initialized"}),
        json!({"jsonrpc":"2.0","id":2,"method":"tools/list"}),
        json!({"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"taskfleet_task_ingest","arguments":{"tasks":[task("task://mcp","MCP","x")]}}}),
        json!({"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"name":"taskfleet_task_query","arguments":{}}}),
        json!({"jsonrpc":"2.0","id":5,"method":"unknown"}),
    ]
    .into_iter()
    .map(|value| value.to_string())
    .collect::<Vec<_>>()
    .join("\n");
    let mut output = Vec::new();
    taskfleet::mcp_stream(&fixture.config, &mut Cursor::new(requests), &mut output).unwrap();
    let responses = String::from_utf8(output)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).unwrap())
        .collect::<Vec<_>>();
    assert_eq!(responses.len(), 5);
    assert_eq!(responses[0]["result"]["serverInfo"]["name"], "taskfleet");
    assert!(responses[1]["result"]["tools"].as_array().unwrap().len() >= 14);
    let query = responses[1]["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .find(|tool| tool["name"] == "taskfleet_task_query")
        .unwrap();
    assert_eq!(query["inputSchema"]["properties"]["filter"]["type"], "object");
    assert_eq!(query["inputSchema"]["properties"]["limit"]["type"], "integer");
    let query: Value = serde_json::from_str(responses[3]["result"]["content"][0]["text"].as_str().unwrap()).unwrap();
    assert_eq!(query[0]["uri"], "task://mcp");
    assert!(responses[3]["result"].get("structuredContent").is_none());
    assert_eq!(responses[4]["error"]["code"], -32601);
}

#[test]
fn packaged_cli_initializes_lists_methods_and_calls_core() {
    let temp = tempfile::tempdir().unwrap();
    let config = temp.path().join("taskfleet.toml");
    Command::cargo_bin("taskfleet").unwrap().current_dir(temp.path()).arg("init").assert().success();
    assert!(config.exists());
    Command::cargo_bin("taskfleet")
        .unwrap()
        .current_dir(temp.path())
        .arg("methods")
        .assert()
        .success();
    let input = json!({"tasks":[task("task://cli","CLI","x")]}).to_string();
    Command::cargo_bin("taskfleet")
        .unwrap()
        .current_dir(temp.path())
        .args(["task.ingest", "--input", &input])
        .assert()
        .success();
    Command::cargo_bin("taskfleet")
        .unwrap()
        .current_dir(temp.path())
        .arg("task.query")
        .write_stdin("{}")
        .assert()
        .success()
        .stdout(predicates::str::contains("task://cli"));
    Command::cargo_bin("taskfleet")
        .unwrap()
        .arg("--version")
        .assert()
        .success()
        .stdout(predicates::str::contains(env!("CARGO_PKG_VERSION")));
}

#[test]
fn in_process_cli_covers_help_config_input_and_failure_surfaces() {
    let temp = tempfile::tempdir().unwrap();
    assert!(cli(temp.path(), &[], "").unwrap().contains("methods:"));
    assert!(cli(temp.path(), &["--version"], "").unwrap().contains(env!("CARGO_PKG_VERSION")));
    assert!(cli(temp.path(), &["init"], "").unwrap().contains("initialized"));
    assert!(cli(temp.path(), &["init"], "").unwrap_err().to_string().contains("already exists"));
    assert!(cli(temp.path(), &["methods"], "").unwrap().contains("taskfleet_task_query"));
    let input = json!({"tasks":[task("task://direct","Direct","x")]}).to_string();
    cli(temp.path(), &["task.ingest", "--input", &input], "").unwrap();
    assert!(cli(temp.path(), &["task.query"], "{}").unwrap().contains("task://direct"));
    assert!(cli(temp.path(), &["task.query"], "").unwrap().contains("task://direct"));
    assert!(cli(temp.path(), &["unknown"], "").unwrap_err().to_string().contains("unknown method"));
    let mcp = json!({"jsonrpc":"2.0","id":1,"method":"tools/list"}).to_string();
    assert!(cli(temp.path(), &["mcp"], &mcp).unwrap().contains("taskfleet_task_query"));
    assert!(cli(temp.path(), &["methods", "--config"], "").unwrap().contains("taskfleet_task_query"));
}

#[test]
fn mcp_tool_failures_are_structured_instead_of_terminating_the_server() {
    let fixture = Fixture::new("");
    let requests = [
        json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"taskfleet_task_get","arguments":{"task":"missing"}}}),
        json!({"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"taskfleet_missing","arguments":{}}}),
    ]
    .into_iter()
    .map(|value| value.to_string())
    .collect::<Vec<_>>()
    .join("\n");
    let mut output = Vec::new();
    taskfleet::mcp_stream(&fixture.config, &mut Cursor::new(requests), &mut output).unwrap();
    for response in String::from_utf8(output).unwrap().lines() {
        assert!(serde_json::from_str::<Value>(response).unwrap()["result"]["isError"].as_bool().unwrap());
    }
}

#[test]
fn control_tools_publish_exact_machine_readable_schemas() {
    let tools = taskfleet::surface::tools();
    for (name, required) in [
        ("taskfleet_task_cancel", vec!["task"]),
        ("taskfleet_task_pause", vec!["task"]),
        ("taskfleet_task_resume", vec!["task"]),
        ("taskfleet_task_reprioritize", vec!["task", "priority"]),
    ] {
        let tool = tools.iter().find(|tool| tool["name"] == name).unwrap();
        assert_eq!(tool["inputSchema"]["required"], json!(required));
        assert_eq!(tool["inputSchema"]["properties"]["task"]["type"], "string");
    }
    let reprioritize = tools.iter().find(|tool| tool["name"] == "taskfleet_task_reprioritize").unwrap();
    assert_eq!(reprioritize["inputSchema"]["properties"]["priority"]["type"], "integer");
    let cancel = tools.iter().find(|tool| tool["name"] == "taskfleet_task_cancel").unwrap();
    assert_eq!(cancel["inputSchema"]["properties"]["reason"]["type"], "string");
}
