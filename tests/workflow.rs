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
    let log = run(Path::new(integrated["worktree"].as_str().unwrap()), &["log", "--oneline"]);
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
