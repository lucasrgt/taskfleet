mod common;

use std::path::Path;

use serde_json::{Value, json};

use common::{Fixture, commit, ingest, run, task};

fn empty_digest() -> String {
    taskfleet::cas::digest_bytes(b"")
}

fn sample_receipt(task_uri: &str, step_id: &str, summary: &str) -> Value {
    let digest = empty_digest();
    json!({
        "schema_version": 1,
        "task_uri": task_uri,
        "step_id": step_id,
        "producer": {"agent_id": "tester", "harness": "test", "model": null},
        "source": {
            "base_tree": "base",
            "result_tree": "result",
            "branch": "taskfleet/test",
            "commit": null,
            "workspace_provider": "git-worktree",
            "workspace_id": null
        },
        "dependencies": [],
        "changes": {"paths": ["feature.txt"], "patch_digest": digest},
        "artifacts": [{
            "name": "proof.log",
            "digest": digest,
            "media_type": "text/plain",
            "size": 0,
            "role": "proof",
            "logical_path": null
        }],
        "proofs": [{
            "gate": "none",
            "status": "green",
            "tree": "result",
            "command_digest": digest,
            "stdout_digest": null,
            "stderr_digest": null
        }],
        "context_exports": {
            "summary": summary,
            "decisions": ["ship the seam"],
            "assumptions": [],
            "symbols": ["Feature"],
            "contracts": ["feature.txt exports Feature"],
            "followups": []
        },
        "fingerprint": {
            "input_digest": digest,
            "lockfiles": {},
            "toolchain": {"rustc": "1"}
        },
        "created_at": "2026-08-07T00:00:00Z"
    })
}

#[test]
fn dependencies_become_ready_at_candidate_and_context_recovers_receipts() {
    let fixture = Fixture::new("");
    let mut service = fixture.service();
    let blocker = task("task://up", "Up", "x");
    let mut down = task("task://down", "Down", "x");
    down["depends_on"] = json!(["task://up"]);
    ingest(&mut service, vec![blocker, down]);

    assert_eq!(
        service.call("task.claim", &json!({"owner":"a","limit":2})).unwrap().as_array().unwrap().len(),
        1
    );
    let workspace = service.call("workspace.prepare", &json!({"task":"task://up"})).unwrap();
    commit(Path::new(workspace["worktree"].as_str().unwrap()), "feature.txt", "from-up\n");
    service.call("step.advance", &json!({"task":"task://up","owner":"a"})).unwrap();
    assert_eq!(service.store.get("task://up").unwrap().state, "candidate");
    assert!(service.store.ready("task://down").unwrap());

    let missing = service.call("task.context", &json!({"task":"task://down"}));
    assert!(missing.unwrap_err().to_string().contains("no published receipt"));

    let published = service
        .call("receipt.publish", &json!({"receipt": sample_receipt("task://up", "execute", "upstream ready")}))
        .unwrap();
    assert!(published["digest"].as_str().unwrap().starts_with("sha256:"));

    let context = service
        .call(
            "task.context",
            &json!({
                "task":"task://down",
                "include":["dependency.receipts.context_exports","dependency.artifacts[role=proof]","dependency.changes.paths"]
            }),
        )
        .unwrap();
    assert_eq!(context["dependencies"][0]["context_exports"]["summary"], "upstream ready");
    assert_eq!(context["dependencies"][0]["changes"]["paths"][0], "feature.txt");
    assert_eq!(context["dependencies"][0]["artifacts"][0]["role"], "proof");
}

#[test]
fn workspace_prepare_merges_dependency_candidate_branches() {
    let fixture = Fixture::new("");
    let mut service = fixture.service();
    let blocker = task("task://up", "Up", "x");
    let mut down = task("task://down", "Down", "x");
    down["depends_on"] = json!(["task://up"]);
    ingest(&mut service, vec![blocker, down]);

    service.call("task.claim", &json!({"owner":"a"})).unwrap();
    let up = service.call("workspace.prepare", &json!({"task":"task://up"})).unwrap();
    commit(Path::new(up["worktree"].as_str().unwrap()), "feature.txt", "from-up\n");
    service.call("step.advance", &json!({"task":"task://up","owner":"a"})).unwrap();

    service
        .call("task.claim", &json!({"owner":"b","filter":{"op":"eq","path":"uri","value":"task://down"}}))
        .unwrap();
    let prepared = service.call("workspace.prepare", &json!({"task":"task://down"})).unwrap();
    assert_eq!(prepared["merged_dependencies"][0]["task"], "task://up");
    let path = Path::new(prepared["worktree"].as_str().unwrap());
    assert_eq!(std::fs::read_to_string(path.join("feature.txt")).unwrap().replace("\r\n", "\n"), "from-up\n");
    let status = service.call("workspace.status", &json!({"task":"task://down"})).unwrap();
    assert_eq!(status["exists"], true);
    let diff = service.call("workspace.diff", &json!({"task":"task://down"})).unwrap();
    assert!(diff["porcelain"].as_str().unwrap().is_empty());
}

#[test]
fn artifacts_cas_and_gc_preserve_pinned_and_reachable_blobs() {
    let fixture = Fixture::new("cas_retention_seconds = 0\n");
    let mut service = fixture.service();
    ingest(&mut service, vec![task("task://solo", "Solo", "x")]);
    service.call("task.claim", &json!({"owner":"a"})).unwrap();
    service.call("workspace.prepare", &json!({"task":"task://solo"})).unwrap();
    service.call("step.advance", &json!({"task":"task://solo","owner":"a"})).unwrap();

    let published = service
        .call(
            "artifact.publish",
            &json!({"bytes":"reachable","media_type":"text/plain","pin":true,"note":"keep"}),
        )
        .unwrap();
    let orphan = service.call("artifact.publish", &json!({"bytes":"orphan","media_type":"text/plain"})).unwrap();
    let digest = published["digest"].as_str().unwrap().to_owned();
    let orphan_digest = orphan["digest"].as_str().unwrap().to_owned();
    assert!(service.cas.resolve(&digest).is_ok());
    assert!(service.cas.resolve(&orphan_digest).is_ok());

    let receipt = sample_receipt("task://solo", "execute", "solo");
    service.call("receipt.publish", &json!({"receipt": receipt})).unwrap();

    let resolved = service.call("artifact.resolve", &json!({"digest": digest})).unwrap();
    assert!(Path::new(resolved["path"].as_str().unwrap()).exists());
    let dest = fixture.temp.path().join("out.txt");
    service.call("artifact.materialize", &json!({"digest": digest, "path": dest})).unwrap();
    assert_eq!(std::fs::read_to_string(&dest).unwrap(), "reachable");

    let gc = service.call("workspace.gc", &json!({})).unwrap();
    assert!(gc["removed_files"].as_u64().unwrap() >= 1 || gc["removed_rows"].as_u64().unwrap() >= 1);
    assert!(service.cas.resolve(&digest).is_ok());
}

#[test]
fn worktree_prepare_alias_and_destroy_preserve_receipts() {
    let fixture = Fixture::new("");
    let mut service = fixture.service();
    ingest(&mut service, vec![task("task://alias", "Alias", "x")]);
    service.call("task.claim", &json!({"owner":"a"})).unwrap();
    let prepared = service.call("worktree.prepare", &json!({"task":"task://alias"})).unwrap();
    assert_eq!(prepared["reused"], false);
    let reused = service.call("workspace.prepare", &json!({"task":"task://alias"})).unwrap();
    assert_eq!(reused["reused"], true);
    commit(Path::new(prepared["worktree"].as_str().unwrap()), "alias.txt", "ok\n");
    let advanced = service
        .call(
            "step.advance",
            &json!({
                "task":"task://alias",
                "owner":"a",
                "receipt": sample_receipt("task://alias", "execute", "alias done")
            }),
        )
        .unwrap();
    assert!(advanced["receipt"]["digest"].as_str().unwrap().starts_with("sha256:"));
    let got = service.call("receipt.get", &json!({"task":"task://alias"})).unwrap();
    assert_eq!(got["receipt"]["context_exports"]["summary"], "alias done");
    let by_digest = service.call("receipt.get", &json!({"digest": got["digest"]})).unwrap();
    assert_eq!(by_digest["receipt"]["task_uri"], "task://alias");
    let deps = service.call("receipt.resolve_dependencies", &json!({"digest": got["digest"]})).unwrap();
    assert!(deps["dependencies"].as_array().unwrap().is_empty());
    assert!(service.call("workspace.destroy", &json!({"task":"task://alias"})).is_ok());
    assert!(service.store.get("task://alias").unwrap().worktree.is_none());
}

#[test]
fn surface_lists_capsule_methods() {
    let fixture = Fixture::new("");
    let tools = taskfleet::surface::tools();
    let names = tools.iter().filter_map(|tool| tool["name"].as_str()).collect::<Vec<_>>();
    assert!(names.contains(&"taskfleet_workspace_prepare"));
    assert!(names.contains(&"taskfleet_task_context"));
    assert!(names.contains(&"taskfleet_receipt_publish"));
    assert!(names.contains(&"taskfleet_artifact_publish"));
    let _ = run(&fixture.repo, &["status", "--porcelain"]);
}

#[test]
fn shared_caches_agentfs_errors_and_cas_helpers_are_exercised() {
    let fixture = Fixture::new(
        r#"workspace_provider = "git-worktree"
cas_max_bytes = 1
cas_retention_seconds = 0
[project.shared_caches]
pnpm_store = "../.caches/pnpm"
nuget_packages = "../.caches/nuget"
cargo_home = "../.caches/cargo"
sccache_dir = "../.caches/sccache"
custom_cache = "../.caches/custom"
"#,
    );
    let mut service = fixture.service();
    let env = taskfleet::workspace::cache_env(&service.config.project);
    assert!(env.get("PNPM_STORE_PATH").unwrap().contains("pnpm"));
    assert!(env.get("NUGET_PACKAGES").unwrap().contains("nuget"));
    assert!(env.get("CARGO_HOME").unwrap().contains("cargo"));
    assert!(env.get("SCCACHE_DIR").unwrap().contains("sccache"));
    assert!(env.get("custom_cache").unwrap().contains("custom"));
    ingest(&mut service, vec![task("task://agent", "Agent", "x")]);
    service.call("task.claim", &json!({"owner":"a"})).unwrap();
    let prepared = service.call("workspace.prepare", &json!({"task":"task://agent"})).unwrap();
    assert_eq!(prepared["provider"], "git-worktree");
    assert!(prepared["env"]["PNPM_STORE_PATH"].as_str().unwrap().contains("pnpm"));
    for key in ["pnpm", "nuget", "cargo", "sccache", "custom"] {
        let path = fixture.temp.path().join(".caches").join(key);
        assert!(path.is_dir(), "missing shared cache {}", path.display());
    }

    let missing = fixture.temp.path().join("no-workspace");
    assert!(taskfleet::workspace::status(&missing).is_err());
    assert!(taskfleet::workspace::diff(&missing).is_err());
    assert!(taskfleet::workspace::destroy(&fixture.repo, &missing, "mystery").is_err());
    assert!(taskfleet::workspace::destroy(&fixture.repo, &missing, "git-worktree").is_ok());

    let source = fixture.temp.path().join("blob.txt");
    std::fs::write(&source, b"from-path").unwrap();
    let published = service.call("artifact.publish", &json!({"path": source, "media_type":"text/plain"})).unwrap();
    let digest = published["digest"].as_str().unwrap();
    assert_eq!(service.cas.get_bytes(digest).unwrap(), b"from-path");
    assert!(!service.store.is_pinned(digest).unwrap());
    service.store.pin_blob(digest, Some("pin")).unwrap();
    assert!(service.store.is_pinned(digest).unwrap());
    let _ = service.cas.put_bytes(b"tiny", "text/plain").unwrap();
    let _ = service.call("workspace.gc", &json!({})).unwrap();
    let empty = taskfleet::cas::Cas::open(&fixture.temp.path().join("empty-cas")).unwrap();
    assert_eq!(empty.sweep(&Default::default()).unwrap(), 0);
    assert!(
        empty
            .resolve("sha256:0000000000000000000000000000000000000000000000000000000000000000")
            .is_err()
    );
    assert!(!taskfleet::cas::is_digest("nope"));
    taskfleet::workspace::ensure_parent(&fixture.temp.path().join("nested").join("file.txt")).unwrap();
}

#[test]
fn agentfs_rejects_when_cli_is_absent() {
    let fixture = Fixture::new(
        r#"workspace_provider = "agentfs"
agentfs_bin = "./missing-agentfs"
"#,
    );
    let mut service = fixture.service();
    ingest(&mut service, vec![task("task://agent", "Agent", "x")]);
    service.call("task.claim", &json!({"owner":"a"})).unwrap();
    assert!(
        service
            .call("workspace.prepare", &json!({"task":"task://agent"}))
            .unwrap_err()
            .to_string()
            .contains("agentfs")
    );
}

#[test]
fn receipt_validation_rejects_invalid_contracts_and_context_budget() {
    let fixture = Fixture::new("");
    let mut service = fixture.service();
    ingest(&mut service, vec![task("task://r", "R", "x")]);
    service.call("task.claim", &json!({"owner":"a"})).unwrap();
    service.call("workspace.prepare", &json!({"task":"task://r"})).unwrap();
    service.call("step.advance", &json!({"task":"task://r","owner":"a"})).unwrap();
    assert!(service.call("receipt.publish", &json!({"receipt":{"schema_version":2}})).is_err());
    let mut empty_uri = sample_receipt("task://r", "execute", "bad");
    empty_uri["task_uri"] = json!("");
    assert!(service.call("receipt.publish", &json!({"receipt": empty_uri})).is_err());
    let mut bad_paths = sample_receipt("task://r", "execute", "bad");
    bad_paths["changes"]["paths"] = json!("nope");
    assert!(service.call("receipt.publish", &json!({"receipt": bad_paths})).is_err());
    let mut bad_exports = sample_receipt("task://r", "execute", "bad");
    bad_exports["context_exports"]["decisions"] = json!("nope");
    assert!(service.call("receipt.publish", &json!({"receipt": bad_exports})).is_err());
    let mut bad_fp = sample_receipt("task://r", "execute", "bad");
    bad_fp["fingerprint"]["lockfiles"] = json!([]);
    assert!(service.call("receipt.publish", &json!({"receipt": bad_fp})).is_err());
    let mut bad_size = sample_receipt("task://r", "execute", "bad");
    bad_size["artifacts"][0]["size"] = json!("nope");
    assert!(service.call("receipt.publish", &json!({"receipt": bad_size})).is_err());
    let mut bad = sample_receipt("task://r", "execute", "bad");
    bad["source"]["workspace_provider"] = json!("nope");
    assert!(service.call("receipt.publish", &json!({"receipt": bad})).is_err());
    let mut bad_role = sample_receipt("task://r", "execute", "bad");
    bad_role["artifacts"][0]["role"] = json!("mystery");
    assert!(service.call("receipt.publish", &json!({"receipt": bad_role})).is_err());
    let mut bad_proof = sample_receipt("task://r", "execute", "bad");
    bad_proof["proofs"][0]["status"] = json!("maybe");
    assert!(service.call("receipt.publish", &json!({"receipt": bad_proof})).is_err());
    assert!(service.call("artifact.publish", &json!({})).is_err());

    let up = task("task://budget-up", "Up", "x");
    let mut down = task("task://budget-down", "Down", "x");
    down["depends_on"] = json!(["task://budget-up"]);
    ingest(&mut service, vec![up, down]);
    service
        .call("task.claim", &json!({"owner":"b","filter":{"op":"eq","path":"uri","value":"task://budget-up"}}))
        .unwrap();
    service.call("workspace.prepare", &json!({"task":"task://budget-up"})).unwrap();
    service.call("step.advance", &json!({"task":"task://budget-up","owner":"b"})).unwrap();
    let summary = "huge context ".repeat(200);
    service
        .call("receipt.publish", &json!({"receipt": sample_receipt("task://budget-up", "execute", &summary)}))
        .unwrap();
    assert!(
        service
            .call("task.context", &json!({"task":"task://budget-down","budget_bytes":32}))
            .unwrap_err()
            .to_string()
            .contains("budget_bytes")
    );
    let full = service.call("task.context", &json!({"task":"task://budget-down","include":["*"]})).unwrap();
    assert_eq!(full["dependencies"].as_array().unwrap().len(), 1);
    assert!(service.call("reconcile", &json!({})).is_ok());
}

#[test]
fn unsupported_provider_and_missing_workspace_paths_fail_closed() {
    let project = taskfleet::model::Project {
        workspace_provider: "mystery".into(),
        ..Default::default()
    };
    let fixture = Fixture::new("");
    let row = taskfleet::model::TaskRow {
        task: serde_json::from_value(task("task://x", "X", "x")).unwrap(),
        state: "running".into(),
        active_workflow: None,
        step: 0,
        owner: Some("a".into()),
        lease_until: None,
        branch: None,
        worktree: None,
        error: None,
        revision: 1,
    };
    assert!(taskfleet::workspace::prepare(&project, &fixture.repo, &row, None, &[], &[]).is_err());
    assert!(taskfleet::workspace::workspace_path(&row).is_err());
    let mut with_path = row.clone();
    with_path.worktree = Some(fixture.temp.path().join("gone").display().to_string());
    assert!(taskfleet::workspace::workspace_path(&with_path).is_ok());
    let mut service = fixture.service();
    ingest(&mut service, vec![task("task://status", "Status", "x")]);
    service.call("task.claim", &json!({"owner":"a"})).unwrap();
    assert!(service.call("workspace.status", &json!({"task":"task://status"})).is_err());
    assert!(service.call("workspace.diff", &json!({"task":"task://status"})).is_err());
    let prepared = service.call("workspace.prepare", &json!({"task":"task://status"})).unwrap();
    std::fs::write(Path::new(prepared["worktree"].as_str().unwrap()).join("dirty.txt"), "x").unwrap();
    let status = service.call("workspace.status", &json!({"task":"task://status"})).unwrap();
    assert_eq!(status["dirty"], true);
    let diff = service.call("workspace.diff", &json!({"task":"task://status"})).unwrap();
    assert!(!diff["porcelain"].as_str().unwrap().is_empty());
}

#[test]
fn agentfs_succeeds_with_fake_cli_and_byte_cap_gc_trims_keep_set() {
    let bin = tempfile::tempdir().unwrap();
    let agentfs = bin.path().join("agentfs-fake");
    std::fs::write(&agentfs, "fake").unwrap();
    let fixture = Fixture::new(&format!(
        r#"workspace_provider = "agentfs"
agentfs_bin = "{}"
cas_max_bytes = 8
cas_retention_seconds = 604800
"#,
        agentfs.display().to_string().replace('\\', "/")
    ));
    let mut service = fixture.service();
    ingest(&mut service, vec![task("task://afs", "Afs", "x")]);
    service.call("task.claim", &json!({"owner":"a"})).unwrap();
    let prepared = service.call("workspace.prepare", &json!({"task":"task://afs"})).unwrap();
    assert_eq!(prepared["provider"], "agentfs");
    assert!(prepared["env"].is_object());
    service.call("step.advance", &json!({"task":"task://afs","owner":"a"})).unwrap();

    let big = "0123456789abcdef".repeat(4);
    service.call("artifact.publish", &json!({"bytes": big, "media_type":"text/plain"})).unwrap();
    service
        .call("artifact.publish", &json!({"bytes":"tiny","media_type":"text/plain","pin":true}))
        .unwrap();
    let gc = service.call("workspace.gc", &json!({})).unwrap();
    assert!(gc["kept"].as_u64().is_some());

    let upstream = sample_receipt("task://afs", "execute", "self");
    let published = service.call("receipt.publish", &json!({"receipt": upstream})).unwrap();
    let mut receipt = sample_receipt("task://afs", "execute", "with deps");
    receipt["dependencies"] = json!([{"task_uri":"task://afs","receipt_digest": published["digest"]}]);
    receipt["proofs"][0]["stdout_digest"] = json!(empty_digest());
    receipt["proofs"][0]["stderr_digest"] = json!(empty_digest());
    let linked = service.call("receipt.publish", &json!({"receipt": receipt})).unwrap();
    let resolved = service.call("receipt.resolve_dependencies", &json!({"digest": linked["digest"]})).unwrap();
    assert_eq!(resolved["dependencies"][0]["task_uri"], "task://afs");
}

#[test]
fn dependency_sync_requires_ready_branch_and_stale_workspace_is_rebuilt() {
    let fixture = Fixture::new("");
    let mut service = fixture.service();
    let blocker = task("task://up", "Up", "x");
    let mut down = task("task://down", "Down", "x");
    down["depends_on"] = json!(["task://up"]);
    ingest(&mut service, vec![blocker, down]);
    service.call("task.claim", &json!({"owner":"a"})).unwrap();
    service.call("workspace.prepare", &json!({"task":"task://up"})).unwrap();
    service.store.execution("task://up", "candidate", 1, None, None).unwrap();
    service.store.workspace("task://up", None, None).unwrap();
    service
        .call("task.claim", &json!({"owner":"b","filter":{"op":"eq","path":"uri","value":"task://down"}}))
        .unwrap();
    assert!(
        service
            .call("workspace.prepare", &json!({"task":"task://down"}))
            .unwrap_err()
            .to_string()
            .contains("no candidate branch")
    );

    let solo = Fixture::new("");
    let mut service = solo.service();
    ingest(&mut service, vec![task("task://stale", "Stale", "x")]);
    service.call("task.claim", &json!({"owner":"a"})).unwrap();
    let prepared = service.call("workspace.prepare", &json!({"task":"task://stale"})).unwrap();
    let path = Path::new(prepared["worktree"].as_str().unwrap());
    taskfleet::runtime::remove_worktree(&solo.repo, path).unwrap();
    let recovered = service.call("workspace.prepare", &json!({"task":"task://stale"})).unwrap();
    assert_eq!(recovered["reused"], false);
    assert!(Path::new(recovered["worktree"].as_str().unwrap()).exists());
    assert!(service.call("artifact.publish", &json!({"bytes":"x"})).is_ok());
    assert!(taskfleet::cas::digest_str("hello").starts_with("sha256:"));
    let cas = taskfleet::cas::Cas::open(&solo.temp.path().join("cas2")).unwrap();
    let (digest, _, _) = cas.put_bytes(b"once", "text/plain").unwrap();
    let (again, _, _) = cas.put_bytes(b"once", "text/plain").unwrap();
    assert_eq!(digest, again);
    assert!(cas.get_bytes("sha256:dead").is_err());
    assert!(!taskfleet::cas::is_digest("sha256:gg"));
    assert!(!taskfleet::cas::is_digest("sha256:not64"));
    let mut keep = std::collections::HashSet::new();
    keep.insert(digest.clone());
    assert_eq!(cas.sweep(&keep).unwrap(), 0);
    let (orphan, _, path) = cas.put_bytes(b"gone", "text/plain").unwrap();
    assert!(path.exists());
    assert_eq!(cas.sweep(&keep).unwrap(), 1);
    assert!(cas.resolve(&orphan).is_err());
}

#[test]
fn more_fail_closed_and_destroy_paths_raise_coverage() {
    let fixture = Fixture::new(r#"workspace_provider = "agentfs""#);
    let mut service = fixture.service();
    ingest(&mut service, vec![task("task://cov", "Cov", "x")]);
    service.call("task.claim", &json!({"owner":"a"})).unwrap();
    assert!(service.call("workspace.prepare", &json!({"task":"task://cov"})).is_err());

    let fixture = Fixture::new("");
    let mut service = fixture.service();
    let _ = service.cas.root();
    let blocker = task("task://up", "Up", "x");
    let mut down = task("task://down", "Down", "x");
    down["depends_on"] = json!(["task://up"]);
    ingest(&mut service, vec![blocker, down, task("task://live", "Live", "x")]);
    service
        .call("task.claim", &json!({"owner":"a","filter":{"op":"eq","path":"uri","value":"task://up"}}))
        .unwrap();
    service.call("workspace.prepare", &json!({"task":"task://up"})).unwrap();
    service.store.execution("task://down", "running", 0, Some("b"), None).unwrap();
    assert!(
        service
            .call("workspace.prepare", &json!({"task":"task://down"}))
            .unwrap_err()
            .to_string()
            .contains("not ready")
    );

    service
        .call("task.claim", &json!({"owner":"c","filter":{"op":"eq","path":"uri","value":"task://live"}}))
        .unwrap();
    let live = service.call("workspace.prepare", &json!({"task":"task://live"})).unwrap();
    assert!(service.call("workspace.destroy", &json!({"task":"task://live"})).is_ok());
    assert!(!Path::new(live["worktree"].as_str().unwrap()).exists());

    service
        .call(
            "receipt.publish",
            &json!({"receipt": sample_receipt("task://live", "execute", "while running")}),
        )
        .unwrap();
    service.store.execution("task://live", "backlog", 0, None, None).unwrap();
    assert!(
        service
            .call("receipt.publish", &json!({"receipt": sample_receipt("task://live", "execute", "backlog")}))
            .unwrap_err()
            .to_string()
            .contains("requires")
    );

    let dest = fixture.temp.path().join("nested").join("out.bin");
    let published = service.call("artifact.publish", &json!({"bytes":"materialize-me"})).unwrap();
    service
        .call("artifact.materialize", &json!({"digest": published["digest"], "path": dest}))
        .unwrap();
    assert!(dest.exists());
}

#[test]
fn workspace_pool_caps_reuses_slots_and_reconcile_cleans_candidates() {
    let fixture = Fixture::new(
        r#"max_parallel_workspaces = 1
workspace_provider = "reflink"
"#,
    );
    let mut service = fixture.service();
    ingest(&mut service, vec![task("task://one", "One", "x"), task("task://two", "Two", "x")]);
    service.call("task.claim", &json!({"owner":"pool","limit":2})).unwrap();
    let first = service.call("workspace.prepare", &json!({"task":"task://one"})).unwrap();
    assert_eq!(first["provider"], "reflink");
    assert_eq!(first["pool_slot"], "0");
    assert!(
        service
            .call("workspace.prepare", &json!({"task":"task://two"}))
            .unwrap_err()
            .to_string()
            .contains("pool exhausted")
    );
    service.call("workspace.destroy", &json!({"task":"task://one"})).unwrap();
    let second = service.call("workspace.prepare", &json!({"task":"task://two"})).unwrap();
    assert_eq!(second["pool_slot"], "0");
    assert_eq!(Path::new(first["worktree"].as_str().unwrap()), Path::new(second["worktree"].as_str().unwrap()));
    commit(Path::new(second["worktree"].as_str().unwrap()), "two.txt", "two\n");
    service.call("step.advance", &json!({"task":"task://two","owner":"pool"})).unwrap();
    assert_eq!(service.store.get("task://two").unwrap().state, "candidate");
    // Force a leftover path onto a candidate to prove reconcile cleanup.
    let leftover = fixture.temp.path().join("worktrees").join("leftover-candidate");
    std::fs::create_dir_all(&leftover).unwrap();
    service
        .store
        .workspace(
            "task://two",
            service.store.get("task://two").unwrap().branch.as_deref(),
            Some(leftover.to_str().unwrap()),
        )
        .unwrap();
    let cleaned = service.call("reconcile", &json!({})).unwrap();
    assert_eq!(cleaned["destroyed_workspaces"], 1);
    assert!(service.store.get("task://two").unwrap().worktree.is_none());
}
