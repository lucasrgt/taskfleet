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
    assert!(responses[1]["result"]["tools"].as_array().unwrap().len() >= 20);
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

#[cfg(target_os = "linux")]
#[test]
#[ignore]
fn external_manage_lock_probe() {
    let Some(repository) = std::env::var_os("TASKFLEET_LOCK_PROBE_REPOSITORY") else {
        return;
    };
    let home = std::env::var_os("TASKFLEET_LOCK_PROBE_HOME").unwrap();
    let ready = std::env::var_os("TASKFLEET_LOCK_PROBE_READY").unwrap();
    let acquired = std::env::var_os("TASKFLEET_LOCK_PROBE_ACQUIRED").unwrap();
    std::fs::write(ready, b"ready").unwrap();
    taskfleet::location::manage(std::path::Path::new(&repository), Some(std::path::Path::new(&home)), "disable").unwrap();
    std::fs::write(acquired, b"acquired").unwrap();
}

#[test]
#[ignore]
fn external_state_environment_probe() {
    let Some(repository) = std::env::var_os("TASKFLEET_ENV_PROBE_REPOSITORY") else {
        return;
    };
    let root = std::path::PathBuf::from(std::env::var_os("TASKFLEET_ENV_PROBE_ROOT").unwrap())
        .canonicalize()
        .unwrap();
    let variables = ["TASKFLEET_STATE_HOME", "XDG_STATE_HOME", "LOCALAPPDATA", "HOME", "USERPROFILE"];
    for name in variables {
        unsafe {
            std::env::remove_var(name);
        }
    }
    for (index, name) in variables.into_iter().enumerate() {
        let expected = root.join(format!("state-{index}"));
        unsafe {
            std::env::set_var(name, &expected);
        }
        let located = taskfleet::location::locate(std::path::Path::new(&repository), None, None).unwrap();
        assert!(!located.enabled && located.state.unwrap().starts_with(&expected));
        unsafe {
            std::env::remove_var(name);
        }
    }
    assert!(taskfleet::location::locate(std::path::Path::new(&repository), None, None).is_err());
}

#[test]
fn external_mode_is_opt_in_reversible_and_leaves_the_repository_clean() {
    let fixture = Fixture::new("");
    std::fs::remove_file(&fixture.config).unwrap();
    let home = fixture.temp.path().join("external-state");
    let home = home.to_str().unwrap();
    assert_eq!(common::run(&fixture.repo, &["status", "--porcelain"]), "");
    let inside = fixture.repo.join("hidden");
    let error = cli(&fixture.repo, &["locate", "--state-home", inside.to_str().unwrap()], "").unwrap_err();
    assert!(error.to_string().contains("outside the repository"));
    #[cfg(unix)]
    {
        let link = fixture.temp.path().join("state-link");
        std::os::unix::fs::symlink(&fixture.repo, &link).unwrap();
        let error = cli(&fixture.repo, &["locate", "--state-home", link.to_str().unwrap()], "").unwrap_err();
        assert!(error.to_string().contains("outside the repository"));
    }

    let initial: Value = serde_json::from_str(&cli(&fixture.repo, &["locate", "--state-home", home], "").unwrap()).unwrap();
    assert_eq!(initial["mode"], "external");
    assert_eq!(initial["enabled"], false);
    assert!(!initial["config"].as_str().is_some_and(|path| std::path::Path::new(path).exists()));
    #[cfg(target_os = "linux")]
    {
        let lock_path = std::path::Path::new(initial["state"].as_str().unwrap()).with_extension("lock");
        std::fs::create_dir_all(lock_path.parent().unwrap()).unwrap();
        let held = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(lock_path)
            .unwrap();
        held.lock().unwrap();
        let ready = fixture.temp.path().join("lock-probe-ready");
        let acquired = fixture.temp.path().join("lock-probe-acquired");
        let mut waiting = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--ignored", "--exact", "external_manage_lock_probe"])
            .env("TASKFLEET_LOCK_PROBE_REPOSITORY", &fixture.repo)
            .env("TASKFLEET_LOCK_PROBE_HOME", home)
            .env("TASKFLEET_LOCK_PROBE_READY", &ready)
            .env("TASKFLEET_LOCK_PROBE_ACQUIRED", &acquired)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .unwrap();
        for _ in 0..500 {
            if ready.exists() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert!(ready.exists(), "external lifecycle lock probe did not start");
        std::thread::sleep(std::time::Duration::from_millis(25));
        assert!(!acquired.exists(), "external lifecycle lock was not exclusive");
        drop(held);
        assert!(waiting.wait().unwrap().success());
        assert!(acquired.exists());
    }

    let enabled: Value = serde_json::from_str(&cli(&fixture.repo, &["external", "enable", "--state-home", home], "").unwrap()).unwrap();
    assert_eq!(enabled["enabled"], true);
    assert_eq!(enabled["changed"], true);
    assert!(std::path::Path::new(enabled["config"].as_str().unwrap()).exists());
    let nested = fixture.repo.join("nested");
    std::fs::create_dir(&nested).unwrap();
    let nested_location: Value = serde_json::from_str(&cli(&nested, &["locate", "--state-home", home], "").unwrap()).unwrap();
    assert_eq!(nested_location["config"], enabled["config"]);
    assert_eq!(common::run(&fixture.repo, &["status", "--porcelain"]), "");

    let input = json!({"tasks":[task("task://external", "External", "x")]}).to_string();
    cli(&fixture.repo, &["task.ingest", "--state-home", home, "--input", &input], "").unwrap();
    let query: Value = serde_json::from_str(&cli(&fixture.repo, &["task.query", "--state-home", home], "").unwrap()).unwrap();
    assert_eq!(query[0]["uri"], "task://external");

    let disabled: Value = serde_json::from_str(&cli(&fixture.repo, &["external", "disable", "--state-home", home], "").unwrap()).unwrap();
    assert_eq!(disabled["enabled"], false);
    assert!(cli(&fixture.repo, &["task.query", "--state-home", home], "").is_err());
    cli(&fixture.repo, &["external", "enable", "--state-home", home], "").unwrap();
    let restored: Value = serde_json::from_str(&cli(&fixture.repo, &["task.query", "--state-home", home], "").unwrap()).unwrap();
    assert_eq!(restored[0]["uri"], "task://external");

    cli(&fixture.repo, &["init"], "").unwrap();
    let local: Value = serde_json::from_str(&cli(&fixture.repo, &["locate", "--state-home", home], "").unwrap()).unwrap();
    assert_eq!(local["mode"], "local");
    std::fs::remove_file(&fixture.config).unwrap();
    assert!(cli(&fixture.repo, &["external", "purge", "--state-home", home], "").is_err());
    cli(&fixture.repo, &["external", "disable", "--state-home", home], "").unwrap();
    let purged: Value = serde_json::from_str(&cli(&fixture.repo, &["external", "purge", "--state-home", home], "").unwrap()).unwrap();
    assert_eq!(purged["purged"], true);
    assert!(!std::path::Path::new(purged["state"].as_str().unwrap()).exists());
    assert_eq!(common::run(&fixture.repo, &["status", "--porcelain"]), "");
}

#[test]
fn external_location_precedence_and_error_paths_are_explicit() {
    let outside = tempfile::tempdir().unwrap();
    let none = taskfleet::location::locate(outside.path(), None, None).unwrap();
    assert_eq!((none.mode, none.enabled), ("none", false));
    let fixture = Fixture::new("");
    let nested = fixture.repo.join("nested");
    std::fs::create_dir(&nested).unwrap();
    let explicit = taskfleet::location::locate(&nested, Some(std::path::Path::new("../taskfleet.toml")), None).unwrap();
    assert_eq!(explicit.mode, "local");
    assert!(cli(&fixture.repo, &["help"], "").unwrap().contains("external <enable|disable|purge>"));

    std::fs::remove_file(&fixture.config).unwrap();
    let environment = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--ignored", "--exact", "external_state_environment_probe"])
        .env("TASKFLEET_ENV_PROBE_REPOSITORY", &fixture.repo)
        .env("TASKFLEET_ENV_PROBE_ROOT", fixture.temp.path())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .unwrap();
    assert!(environment.success());

    let home = fixture.temp.path().join("state");
    let first = taskfleet::location::manage(&fixture.repo, Some(&home), "enable").unwrap();
    let second = taskfleet::location::manage(&fixture.repo, Some(&home), "enable").unwrap();
    assert_eq!((first.changed, second.changed), (Some(true), Some(false)));
    assert!(taskfleet::location::manage(&fixture.repo, Some(&home), "unknown").is_err());
    taskfleet::location::manage(&fixture.repo, Some(&home), "disable").unwrap();
    assert_eq!(taskfleet::location::manage(&fixture.repo, Some(&home), "disable").unwrap().changed, Some(false));
    taskfleet::location::manage(&fixture.repo, Some(&home), "purge").unwrap();
    assert_eq!(taskfleet::location::manage(&fixture.repo, Some(&home), "purge").unwrap().purged, Some(false));
    assert!(cli(&fixture.repo, &["mcp", "--state-home", home.to_str().unwrap()], "").is_err());
}
