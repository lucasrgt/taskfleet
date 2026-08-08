mod common;

use std::path::Path;

use serde_json::json;

use common::{Fixture, commit, ingest, run, task};
use taskfleet::model::{Filter, Gate};

const WORKFLOW: &str = r#"
[[gate]]
id = "clean"
command = ["git", "status", "--porcelain"]
[[gate]]
id = "approval"
kind = "approval"
command = []
[[workflow]]
id = "ship"
[[workflow.step]]
id = "build"
gates = ["clean"]
[[workflow.step]]
id = "review"
gates = ["approval"]
[[route]]
workflow = "ship"
when = { op="true" }
"#;

#[test]
fn every_step_requires_tree_bound_gates_before_advancing() {
    let fixture = Fixture::new(WORKFLOW);
    let mut service = fixture.service();
    ingest(&mut service, vec![task("task://ship", "Ship feature", "website")]);
    service.call("task.claim", &json!({"owner":"agent"})).unwrap();
    let workspace = service.call("worktree.prepare", &json!({"task":"task://ship"})).unwrap();
    let path = Path::new(workspace["worktree"].as_str().unwrap());
    commit(path, "feature.txt", "done\n");
    assert!(service.call("step.advance", &json!({"task":"task://ship","owner":"agent"})).is_err());
    assert!(
        service
            .call("gate.approve", &json!({"task":"task://ship","gate":"clean","by":"human","approved":true}))
            .is_err()
    );
    assert!(
        service.call("gate.run", &json!({"task":"task://ship","gate":"clean"})).unwrap()["ok"]
            .as_bool()
            .unwrap()
    );
    assert_eq!(
        service.call("step.advance", &json!({"task":"task://ship","owner":"agent"})).unwrap()["next_step"],
        "review"
    );
    assert!(service.call("gate.run", &json!({"task":"task://ship","gate":"approval"})).is_err());
    assert!(
        service
            .call("gate.approve", &json!({"task":"task://ship","gate":"approval","by":"lucas","approved":true}))
            .unwrap()["ok"]
            .as_bool()
            .unwrap()
    );
    assert_eq!(
        service.call("step.advance", &json!({"task":"task://ship","owner":"agent"})).unwrap()["state"],
        "candidate"
    );
    assert!(!path.exists());
}

#[test]
fn a_receipt_is_invalidated_when_the_branch_tree_changes() {
    let fixture = Fixture::new(WORKFLOW);
    let mut service = fixture.service();
    ingest(&mut service, vec![task("task://proof", "Proof", "website")]);
    service.call("task.claim", &json!({"owner":"agent"})).unwrap();
    let workspace = service.call("worktree.prepare", &json!({"task":"task://proof"})).unwrap();
    let path = Path::new(workspace["worktree"].as_str().unwrap());
    commit(path, "one.txt", "one\n");
    service.call("gate.run", &json!({"task":"task://proof","gate":"clean"})).unwrap();
    commit(path, "two.txt", "two\n");
    let error = service
        .call("step.advance", &json!({"task":"task://proof","owner":"agent"}))
        .unwrap_err()
        .to_string();
    assert!(error.contains("not green"));
}

#[test]
fn failed_command_gate_blocks_and_can_be_retried() {
    let fixture = Fixture::new(
        r#"
[[gate]]
id = "red"
command = ["git", "diff", "--quiet", "HEAD~1", "HEAD"]
[[workflow]]
id = "ship"
[[workflow.step]]
id = "test"
gates = ["red"]
[[route]]
workflow = "ship"
when = { op="true" }
"#,
    );
    let mut service = fixture.service();
    ingest(&mut service, vec![task("task://red", "Red", "x")]);
    service.call("task.claim", &json!({"owner":"agent"})).unwrap();
    let workspace = service.call("worktree.prepare", &json!({"task":"task://red"})).unwrap();
    commit(Path::new(workspace["worktree"].as_str().unwrap()), "red.txt", "red\n");
    assert!(
        !service.call("gate.run", &json!({"task":"task://red","gate":"red"})).unwrap()["ok"]
            .as_bool()
            .unwrap()
    );
    assert_eq!(
        service.call("task.get", &json!({"task":"task://red"})).unwrap()["execution"]["state"],
        "blocked"
    );
    assert!(service.call("gate.run", &json!({"task":"task://red","gate":"red"})).is_err());
    assert!(service.call("step.advance", &json!({"task":"task://red","owner":"agent"})).is_err());
    service.call("task.retry", &json!({"task":"task://red"})).unwrap();
}

#[test]
fn integration_merges_candidates_sequentially_and_reproves_each_tree() {
    let fixture = Fixture::new(
        r#"
[[gate]]
id = "integration"
events = ["integration.merge"]
command = ["git", "status", "--porcelain"]
"#,
    );
    let mut service = fixture.service();
    ingest(&mut service, vec![task("task://a", "Alpha", "x"), task("task://b", "Beta", "x")]);
    let claimed = service.call("task.claim", &json!({"owner":"fleet","limit":2})).unwrap();
    for (index, row) in claimed.as_array().unwrap().iter().enumerate() {
        let uri = row["task"]["uri"].as_str().unwrap();
        let workspace = service.call("worktree.prepare", &json!({"task":uri})).unwrap();
        commit(Path::new(workspace["worktree"].as_str().unwrap()), &format!("file-{index}.txt"), "green\n");
        service.call("step.advance", &json!({"task":uri,"owner":"fleet"})).unwrap();
    }
    let integrated = service.call("integration.run", &json!({})).unwrap();
    assert_eq!(integrated["merged"].as_array().unwrap().len(), 2);
    assert!(integrated["blocked"].as_array().unwrap().is_empty());
    assert_eq!(service.call("task.query", &json!({"states":["done"]})).unwrap().as_array().unwrap().len(), 2);
    assert!(integrated["worktree"].is_null());
    assert_eq!(integrated["retained"], false);
    let branch = integrated["branch"].as_str().unwrap();
    let log = run(&fixture.repo, &["log", branch, "--oneline"]);
    assert!(log.contains("Integrate task://a") && log.contains("Integrate task://b"));
}

#[test]
fn integration_gate_failure_aborts_only_the_bad_candidate() {
    let fixture = Fixture::new(
        r#"
[[gate]]
id = "integration-red"
events = ["integration.merge"]
command = ["git", "diff", "--cached", "--quiet"]
"#,
    );
    let mut service = fixture.service();
    ingest(&mut service, vec![task("task://bad", "Bad", "x")]);
    service.call("task.claim", &json!({"owner":"agent"})).unwrap();
    let workspace = service.call("worktree.prepare", &json!({"task":"task://bad"})).unwrap();
    commit(Path::new(workspace["worktree"].as_str().unwrap()), "bad.txt", "bad\n");
    service.call("step.advance", &json!({"task":"task://bad","owner":"agent"})).unwrap();
    let result = service.call("integration.run", &json!({})).unwrap();
    assert_eq!(result["blocked"], json!(["task://bad"]));
    assert_eq!(
        service.call("task.get", &json!({"task":"task://bad"})).unwrap()["execution"]["state"],
        "blocked"
    );
}

#[test]
fn worktree_recovery_and_runtime_failures_are_explicit() {
    let fixture = Fixture::new("");
    let mut service = fixture.service();
    ingest(&mut service, vec![task("task://worktree", "A !!! B", "x")]);
    assert!(
        service
            .call("worktree.prepare", &json!({"task":"task://worktree"}))
            .unwrap_err()
            .to_string()
            .contains("claimed")
    );
    service.call("task.claim", &json!({"owner":"agent"})).unwrap();
    let first = service.call("worktree.prepare", &json!({"task":"task://worktree","base":"HEAD"})).unwrap();
    assert!(first["branch"].as_str().unwrap().contains("a-b"));
    assert!(
        service.call("worktree.prepare", &json!({"task":"task://worktree"})).unwrap()["reused"]
            .as_bool()
            .unwrap()
    );
    let path = Path::new(first["worktree"].as_str().unwrap());
    taskfleet::runtime::remove_worktree(&fixture.repo, path).unwrap();
    let recovered = service.call("worktree.prepare", &json!({"task":"task://worktree"})).unwrap();
    assert!(!recovered["reused"].as_bool().unwrap());
    let recovered_path = Path::new(recovered["worktree"].as_str().unwrap());
    std::fs::write(recovered_path.join("dirty.txt"), "dirty").unwrap();
    assert!(taskfleet::runtime::tree(recovered_path, false).unwrap_err().to_string().contains("clean"));
    std::fs::remove_file(recovered_path.join("dirty.txt")).unwrap();
    assert!(taskfleet::runtime::git(&fixture.repo, &["not-a-command"]).is_err());
    taskfleet::runtime::remove_worktree(&fixture.repo, recovered_path).unwrap();
    taskfleet::runtime::remove_worktree(&fixture.repo, recovered_path).unwrap();
    let row = service.store.get("task://worktree").unwrap();
    let empty = Gate {
        id: "empty".into(),
        kind: "command".into(),
        command: vec![],
        events: vec![],
        when: Filter::True,
        timeout_seconds: 1,
        required: true,
    };
    assert!(
        taskfleet::runtime::run_gate(&empty, &row, &fixture.repo, "step", false)
            .unwrap_err()
            .to_string()
            .contains("no command")
    );
    assert!(service.call("step.advance", &json!({"task":"task://worktree","owner":"wrong"})).is_err());
    assert!(service.call("integration.run", &json!({})).unwrap_err().to_string().contains("no candidate"));
}

#[test]
fn query_limits_and_optional_gates_do_not_block_progress() {
    let fixture = Fixture::new(
        r#"
[[gate]]
id = "advisory"
required = false
command = ["git", "diff", "--quiet", "HEAD~1", "HEAD"]
[[workflow]]
id = "ship"
[[workflow.step]]
id = "build"
gates = ["advisory"]
[[route]]
workflow = "ship"
when = { op="true" }
"#,
    );
    let mut service = fixture.service();
    ingest(&mut service, vec![task("task://one", "One", "x"), task("task://two", "Two", "x")]);
    assert_eq!(service.call("task.query", &json!({"limit":1})).unwrap().as_array().unwrap().len(), 1);
    service.call("task.claim", &json!({"owner":"agent"})).unwrap();
    let workspace = service.call("worktree.prepare", &json!({"task":"task://one"})).unwrap();
    commit(Path::new(workspace["worktree"].as_str().unwrap()), "one.txt", "one\n");
    assert!(
        !service.call("gate.run", &json!({"task":"task://one","gate":"advisory"})).unwrap()["ok"]
            .as_bool()
            .unwrap()
    );
    assert_eq!(
        service.call("task.get", &json!({"task":"task://one"})).unwrap()["execution"]["state"],
        "running"
    );
    assert_eq!(
        service.call("step.advance", &json!({"task":"task://one","owner":"agent"})).unwrap()["state"],
        "candidate"
    );
}

#[test]
fn reopened_service_preserves_claim_workspace_and_gate_receipt() {
    let fixture = Fixture::new(WORKFLOW);
    let mut service = fixture.service();
    ingest(&mut service, vec![task("task://restart", "Restart", "x")]);
    service.call("task.claim", &json!({"owner":"agent"})).unwrap();
    let workspace = service.call("worktree.prepare", &json!({"task":"task://restart"})).unwrap();
    commit(Path::new(workspace["worktree"].as_str().unwrap()), "restart.txt", "durable\n");
    service.call("gate.run", &json!({"task":"task://restart","gate":"clean"})).unwrap();
    drop(service);

    let mut service = fixture.service();
    let status = service.call("task.get", &json!({"task":"task://restart"})).unwrap();
    assert_eq!(status["execution"]["owner"], "agent");
    assert_eq!(status["execution"]["branch"], workspace["branch"]);
    assert_eq!(status["execution"]["gates"][0]["status"], "green");
    service.call("step.advance", &json!({"task":"task://restart","owner":"agent"})).unwrap();
}

#[test]
fn merge_conflict_blocks_only_that_candidate_and_later_candidates_continue() {
    let fixture = Fixture::new("");
    let mut service = fixture.service();
    ingest(
        &mut service,
        vec![
            task("task://a", "First", "x"),
            task("task://b", "Conflict", "x"),
            task("task://c", "Later", "x"),
        ],
    );
    let claimed = service.call("task.claim", &json!({"owner":"fleet","limit":3})).unwrap();
    for row in claimed.as_array().unwrap() {
        let uri = row["task"]["uri"].as_str().unwrap();
        let workspace = service.call("worktree.prepare", &json!({"task":uri})).unwrap();
        let path = Path::new(workspace["worktree"].as_str().unwrap());
        match uri {
            "task://a" => commit(path, "seed.txt", "first\n"),
            "task://b" => commit(path, "seed.txt", "conflict\n"),
            _ => commit(path, "later.txt", "later\n"),
        }
        service.call("step.advance", &json!({"task":uri,"owner":"fleet"})).unwrap();
    }
    let integrated = service.call("integration.run", &json!({"retain_worktree": true})).unwrap();
    assert_eq!(integrated["merged"], json!(["task://a", "task://c"]));
    assert_eq!(integrated["blocked"], json!(["task://b"]));
    let path = Path::new(integrated["worktree"].as_str().unwrap());
    assert_eq!(std::fs::read_to_string(path.join("seed.txt")).unwrap(), "first\n");
    assert!(path.join("later.txt").exists());
}

#[test]
fn candidate_pause_and_priority_control_integration_selection() {
    let fixture = Fixture::new("");
    let mut service = fixture.service();
    ingest(
        &mut service,
        vec![
            task("task://low", "Low", "x"),
            task("task://held", "Held", "x"),
            task("task://middle", "Middle", "x"),
        ],
    );
    let claimed = service.call("task.claim", &json!({"owner":"fleet","limit":3})).unwrap();
    for (index, row) in claimed.as_array().unwrap().iter().enumerate() {
        let uri = row["task"]["uri"].as_str().unwrap();
        let workspace = service.call("worktree.prepare", &json!({"task":uri})).unwrap();
        commit(Path::new(workspace["worktree"].as_str().unwrap()), &format!("control-{index}.txt"), uri);
        service.call("step.advance", &json!({"task":uri,"owner":"fleet"})).unwrap();
    }
    service.call("task.reprioritize", &json!({"task":"task://low","priority":1})).unwrap();
    service.call("task.reprioritize", &json!({"task":"task://middle","priority":10})).unwrap();
    service.call("task.reprioritize", &json!({"task":"task://held","priority":50})).unwrap();
    let held = service.call("task.pause", &json!({"task":"task://held"})).unwrap();
    assert_eq!(held["execution"]["state"], "candidate");
    let first = service.call("integration.run", &json!({"branch":"integration/controls-first"})).unwrap();
    assert_eq!(first["merged"], json!(["task://middle", "task://low"]));
    assert_eq!(
        service.call("task.get", &json!({"task":"task://held"})).unwrap()["execution"]["state"],
        "candidate"
    );
    service.call("task.resume", &json!({"task":"task://held"})).unwrap();
    let second = service.call("integration.run", &json!({"branch":"integration/controls-second"})).unwrap();
    assert_eq!(second["merged"], json!(["task://held"]));
}

#[test]
fn pipeline_spawn_merges_args_and_rerun_respects_max_runs() {
    let fixture = Fixture::new(
        r#"
[[workflow]]
id = "pack"
max_runs = 2
args = { language = "en", tone = "neutral" }
[[workflow.step]]
id = "research"
title = "Research"
instruction = "Research using resolved args"
args = { phase = "research", tone = "strict" }
[[workflow.step]]
id = "deliver"
title = "Deliver"
instruction = "Deliver the artifact"
"#,
    );
    let mut service = fixture.service();
    let spawned = service
        .call(
            "task.spawn",
            &json!({
                "workflow":"pack",
                "uri":"pipeline://pack/1",
                "title":"Pack run",
                "series":"alpha",
                "args":{"language":"pt-BR","extra":true}
            }),
        )
        .unwrap();
    assert_eq!(spawned["task"]["uri"], "pipeline://pack/1");
    assert_eq!(spawned["task"]["meta"]["pipeline"], "pack");
    assert_eq!(spawned["task"]["meta"]["run"], 1);
    assert_eq!(spawned["task"]["meta"]["series"], "alpha");
    assert_eq!(spawned["execution"]["active_step"]["id"], "research");
    assert_eq!(spawned["execution"]["active_step"]["instruction"], "Research using resolved args");
    assert_eq!(spawned["execution"]["active_step"]["args"]["language"], "pt-BR");
    assert_eq!(spawned["execution"]["active_step"]["args"]["tone"], "strict");
    assert_eq!(spawned["execution"]["active_step"]["args"]["phase"], "research");
    assert_eq!(spawned["execution"]["active_step"]["args"]["extra"], true);
    assert_eq!(spawned["execution"]["max_runs"], 2);

    service
        .call(
            "task.claim",
            &json!({"owner":"agent","filter":{"op":"eq","path":"uri","value":"pipeline://pack/1"}}),
        )
        .unwrap();
    assert!(
        service
            .call("task.rerun", &json!({"task":"pipeline://pack/1"}))
            .unwrap_err()
            .to_string()
            .contains("running")
    );
    service.call("task.pause", &json!({"task":"pipeline://pack/1"})).unwrap();

    let second = service
        .call(
            "task.rerun",
            &json!({"task":"pipeline://pack/1","uri":"pipeline://pack/2","args":{"tone":"warm"}}),
        )
        .unwrap();
    assert_eq!(second["task"]["uri"], "pipeline://pack/2");
    assert_eq!(second["task"]["meta"]["run"], 2);
    assert_eq!(second["task"]["meta"]["args"]["tone"], "warm");
    assert_eq!(second["task"]["meta"]["args"]["language"], "pt-BR");
    assert_ne!(second["task"]["uri"], spawned["task"]["uri"]);

    assert!(
        service
            .call("task.rerun", &json!({"task":"pipeline://pack/2","uri":"pipeline://pack/3"}))
            .unwrap_err()
            .to_string()
            .contains("max_runs")
    );

    let merged = taskfleet::pipeline::merge_args(&json!({"a":1,"b":1}), &json!({"b":2,"c":2}), &json!({"c":3,"d":3}));
    assert_eq!(merged, json!({"a":1,"b":2,"c":3,"d":3}));

    assert!(
        service
            .call("task.spawn", &json!({"workflow":"pack","meta":[]}))
            .unwrap_err()
            .to_string()
            .contains("meta")
    );
    assert!(
        service
            .call("task.spawn", &json!({"workflow":"pack","args":[]}))
            .unwrap_err()
            .to_string()
            .contains("args")
    );
    assert!(
        service
            .call("task.rerun", &json!({"task":"pipeline://pack/2","args":[]}))
            .unwrap_err()
            .to_string()
            .contains("args")
    );

    let limited = Fixture::new(
        r#"
[[workflow]]
id = "once"
max_runs = 1
[[workflow.step]]
id = "only"
instruction = "once"
"#,
    );
    let mut once = limited.service();
    once.call(
        "task.spawn",
        &json!({
            "workflow":"once",
            "uri":"pipeline://once/1",
            "title":"Once",
            "description":"desc",
            "tags":["t"],
            "priority":"high",
            "depends_on":[]
        }),
    )
    .unwrap();
    assert!(
        once.call("task.spawn", &json!({"workflow":"once","uri":"pipeline://once/2"}))
            .unwrap_err()
            .to_string()
            .contains("max_runs")
    );

    let manual = Fixture::new(
        r#"
[[workflow]]
id = "manual"
max_runs = 2
[[workflow.step]]
id = "go"
"#,
    );
    let mut manual_service = manual.service();
    let mut seeded = task("task://manual-seed", "Manual", "x");
    seeded["workflow"] = json!("manual");
    ingest(&mut manual_service, vec![seeded]);
    let rerun = manual_service
        .call(
            "task.rerun",
            &json!({"task":"task://manual-seed","uri":"pipeline://manual/2","series":"s1","title":"Manual 2"}),
        )
        .unwrap();
    assert_eq!(rerun["task"]["meta"]["pipeline"], "manual");
    assert_eq!(rerun["task"]["meta"]["run"], 2);
    assert_eq!(rerun["task"]["meta"]["series"], "s1");

    let unlimited = Fixture::new(
        r#"
[[workflow]]
id = "loop"
max_runs = 0
[[workflow.step]]
id = "tick"
"#,
    );
    let mut loop_service = unlimited.service();
    loop_service
        .call("task.spawn", &json!({"workflow":"loop","uri":"pipeline://loop/1","series":"batch"}))
        .unwrap();
    let third = loop_service
        .call("task.rerun", &json!({"task":"pipeline://loop/1","uri":"pipeline://loop/2"}))
        .unwrap();
    assert_eq!(third["task"]["meta"]["run"], 2);
    assert!(
        loop_service
            .call("task.rerun", &json!({"task":"task://nope"}))
            .unwrap_err()
            .to_string()
            .contains("not found")
    );
    assert!(
        loop_service
            .call("task.spawn", &json!({"workflow":"missing"}))
            .unwrap_err()
            .to_string()
            .contains("unknown workflow")
    );
    assert!(
        loop_service
            .call("task.rerun", &json!({"task":"pipeline://loop/1","meta":{"note":"x"}}))
            .is_ok()
    );
}
