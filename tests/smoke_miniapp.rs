//! Production-shaped smoke tests against `examples/miniapp`.
//!
//! Materializes the miniapp into a temporary git checkout, compiles its verify
//! gate, then runs a multi-agent delivery loop with receipts and integration.

mod common;

use std::{
    env, fs,
    path::{Path, PathBuf},
    process::Command,
};

use serde_json::{Value, json};

use common::{ingest, run, task};

struct MiniappSmoke {
    #[allow(dead_code)]
    temp: tempfile::TempDir,
    repo: PathBuf,
    config: PathBuf,
    verify: PathBuf,
}

impl MiniappSmoke {
    fn boot() -> Self {
        let source = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("examples").join("miniapp");
        let temp = tempfile::tempdir().unwrap();
        let repo = temp.path().join("miniapp");
        copy_dir(&source, &repo);
        // auth/health already present; remove auth so the first failing gate proves fail-closed,
        // then agents restore it as product work.
        let _ = fs::remove_file(repo.join("app").join("features").join("auth.txt"));

        run(&repo, &["init", "-b", "main"]);
        run(&repo, &["config", "user.name", "Miniapp Smoke"]);
        run(&repo, &["config", "user.email", "smoke@taskfleet.test"]);
        run(&repo, &["add", "."]);
        run(&repo, &["commit", "-m", "seed miniapp"]);

        let verify = compile_verify(&repo);
        let config = write_config(&repo, &verify);
        Self { temp, repo, config, verify }
    }

    fn service(&self) -> taskfleet::Service {
        taskfleet::Service::open(&self.config).unwrap()
    }
}

fn copy_dir(from: &Path, to: &Path) {
    fs::create_dir_all(to).unwrap();
    for entry in fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let target = to.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_dir(&entry.path(), &target);
        } else {
            fs::copy(entry.path(), target).unwrap();
        }
    }
}

fn compile_verify(repo: &Path) -> PathBuf {
    let source = repo.join("scripts").join("verify.rs");
    let output = if cfg!(windows) {
        repo.join("scripts").join("verify.exe")
    } else {
        repo.join("scripts").join("verify")
    };
    let status = Command::new("rustc")
        .args([source.to_str().unwrap(), "-O", "-o", output.to_str().unwrap()])
        .status()
        .expect("rustc must be available to compile miniapp verify");
    assert!(status.success(), "rustc failed to compile miniapp verify");
    output
}

fn write_config(repo: &Path, verify: &Path) -> PathBuf {
    let worktrees = repo.parent().unwrap().join("worktrees");
    let config = repo.join("taskfleet.toml");
    let verify_cmd = verify.display().to_string().replace('\\', "/");
    fs::write(
        &config,
        format!(
            r#"schema = 1
[project]
repository = "."
database = ".taskfleet/state.sqlite"
worktree_root = "{worktrees}"
cas_root = ".taskfleet/cas"
workspace_provider = "git-worktree"
default_workflow = "delivery"

[[view]]
id = "all"
filter = {{ op = "true" }}

[[gate]]
id = "miniapp-verify"
kind = "command"
command = ["{verify_cmd}"]
events = ["step.complete", "integration.merge"]
timeout_seconds = 120
required = true

[[workflow]]
id = "delivery"
[[workflow.step]]
id = "implement"
gates = ["miniapp-verify"]
[[route]]
workflow = "delivery"
when = {{ op = "true" }}
"#,
            worktrees = worktrees.display().to_string().replace('\\', "/")
        ),
    )
    .unwrap();
    config
}

fn commit_if_dirty(worktree: &Path, message: &str) {
    let dirty = run(worktree, &["status", "--porcelain"]);
    if dirty.is_empty() {
        return;
    }
    run(worktree, &["add", "."]);
    run(worktree, &["commit", "-m", message]);
}

fn empty_digest() -> String {
    taskfleet::cas::digest_bytes(b"")
}

fn receipt(task_uri: &str, summary: &str, paths: &[&str], deps: &[(String, String)]) -> Value {
    let digest = empty_digest();
    json!({
        "schema_version": 1,
        "task_uri": task_uri,
        "step_id": "implement",
        "producer": {"agent_id": "smoke-agent", "harness": "miniapp-smoke", "model": null},
        "source": {
            "base_tree": "base",
            "result_tree": "result",
            "branch": null,
            "commit": null,
            "workspace_provider": "git-worktree",
            "workspace_id": null
        },
        "dependencies": deps.iter().map(|(uri, dig)| json!({"task_uri": uri, "receipt_digest": dig})).collect::<Vec<_>>(),
        "changes": {"paths": paths, "patch_digest": digest},
        "artifacts": [{
            "name": "verify.log",
            "digest": digest,
            "media_type": "text/plain",
            "size": 0,
            "role": "proof",
            "logical_path": null
        }],
        "proofs": [{
            "gate": "miniapp-verify",
            "status": "green",
            "tree": "result",
            "command_digest": digest,
            "stdout_digest": null,
            "stderr_digest": null
        }],
        "context_exports": {
            "summary": summary,
            "decisions": [format!("shipped {task_uri}")],
            "assumptions": [],
            "symbols": [],
            "contracts": paths.iter().map(|path| format!("{path} is public")).collect::<Vec<_>>(),
            "followups": []
        },
        "fingerprint": {
            "input_digest": digest,
            "lockfiles": {},
            "toolchain": {"verify": "rustc"}
        },
        "created_at": "2026-08-07T15:00:00Z"
    })
}

#[test]
fn miniapp_prod_smoke_runs_fail_closed_gate_then_full_delivery() {
    let smoke = MiniappSmoke::boot();
    let mut service = smoke.service();

    // Seed without auth: verify gate must fail closed.
    let status = Command::new(&smoke.verify).current_dir(&smoke.repo).output().unwrap();
    assert!(!status.status.success(), "verify must fail before auth exists");

    let mut health = task("miniapp://health", "Harden health surface", "backend");
    health["meta"]["surface"] = json!("health");
    let mut auth = task("miniapp://auth", "Add auth surface", "backend");
    auth["meta"]["surface"] = json!("auth");
    let mut ship = task("miniapp://ship", "Ship miniapp release notes", "backend");
    ship["depends_on"] = json!(["miniapp://health", "miniapp://auth"]);
    ship["meta"]["surface"] = json!("ship");
    ingest(&mut service, vec![health, auth, ship]);

    // Agent A: health improvement alone still fails the product gate (auth missing).
    service
        .call(
            "task.claim",
            &json!({"owner":"agent-health","filter":{"op":"eq","path":"uri","value":"miniapp://health"}}),
        )
        .unwrap();
    let health_ws = service.call("workspace.prepare", &json!({"task":"miniapp://health"})).unwrap();
    let health_path = Path::new(health_ws["worktree"].as_str().unwrap());
    fs::write(
        health_path.join("app").join("features").join("health.txt"),
        "# health\nstatus endpoint returns ok\nlatency budget 100ms\n",
    )
    .unwrap();
    commit_if_dirty(health_path, "harden health");
    let red = service.call("gate.run", &json!({"task":"miniapp://health","gate":"miniapp-verify"})).unwrap();
    assert_eq!(red["ok"], false);
    assert_eq!(service.store.get("miniapp://health").unwrap().state, "blocked");
    service.call("task.retry", &json!({"task":"miniapp://health"})).unwrap();

    // Agent B: add auth surface first so the product gate can pass.
    service
        .call(
            "task.claim",
            &json!({"owner":"agent-auth","filter":{"op":"eq","path":"uri","value":"miniapp://auth"}}),
        )
        .unwrap();
    let auth_ws = service.call("workspace.prepare", &json!({"task":"miniapp://auth"})).unwrap();
    let auth_path = Path::new(auth_ws["worktree"].as_str().unwrap());
    fs::create_dir_all(auth_path.join("app").join("features")).unwrap();
    fs::write(auth_path.join("app").join("features").join("auth.txt"), "# auth\nlogin + session cookie\n").unwrap();
    commit_if_dirty(auth_path, "add auth");
    let green = service.call("gate.run", &json!({"task":"miniapp://auth","gate":"miniapp-verify"})).unwrap();
    assert_eq!(green["ok"], true);
    let auth_advance = service
        .call(
            "step.advance",
            &json!({
                "task":"miniapp://auth",
                "owner":"agent-auth",
                "receipt": receipt("miniapp://auth", "auth surface live", &["app/features/auth.txt"], &[])
            }),
        )
        .unwrap();
    assert_eq!(auth_advance["state"], "candidate");

    // Agent A retries health on top of auth candidate via dependency? health doesn't depend on auth
    // in the task graph, but the product gate needs auth in the tree. Merge auth branch manually
    // by making health depend on auth for workspace sync — re-ingest with dependency.
    let mut health = task("miniapp://health", "Harden health surface", "backend");
    health["depends_on"] = json!(["miniapp://auth"]);
    health["meta"]["surface"] = json!("health");
    ingest(&mut service, vec![health]);
    assert!(service.store.ready(&service.store.get("miniapp://health").unwrap()).unwrap());
    service
        .call(
            "task.claim",
            &json!({"owner":"agent-health","filter":{"op":"eq","path":"uri","value":"miniapp://health"}}),
        )
        .unwrap();
    // Clear stale worktree path from the blocked attempt if present.
    if let Some(path) = service.store.get("miniapp://health").unwrap().worktree {
        let _ = taskfleet::runtime::remove_worktree(&smoke.repo, Path::new(&path));
        let health = service.store.get("miniapp://health").unwrap();
        service.store.workspace("miniapp://health", &health.state, None, None).unwrap();
    }
    let health_ws = service.call("workspace.prepare", &json!({"task":"miniapp://health"})).unwrap();
    assert_eq!(health_ws["merged_dependencies"][0]["task"], "miniapp://auth");
    let health_path = Path::new(health_ws["worktree"].as_str().unwrap());
    assert!(health_path.join("app").join("features").join("auth.txt").exists());
    fs::write(
        health_path.join("app").join("features").join("health.txt"),
        "# health\nstatus endpoint returns ok\nlatency budget 100ms\nreadyz probes\n",
    )
    .unwrap();
    commit_if_dirty(health_path, "harden health with auth present");
    let green = service.call("gate.run", &json!({"task":"miniapp://health","gate":"miniapp-verify"})).unwrap();
    assert_eq!(green["ok"], true);
    let auth_digest = service.call("receipt.get", &json!({"task":"miniapp://auth"})).unwrap()["digest"]
        .as_str()
        .unwrap()
        .to_owned();
    service
        .call(
            "step.advance",
            &json!({
                "task":"miniapp://health",
                "owner":"agent-health",
                "receipt": receipt(
                    "miniapp://health",
                    "health hardened",
                    &["app/features/health.txt"],
                    &[("miniapp://auth".into(), auth_digest.clone())]
                )
            }),
        )
        .unwrap();

    // Ship agent recovers both receipts, merges both branches, writes release notes, integrates.
    service
        .call(
            "task.claim",
            &json!({"owner":"agent-ship","filter":{"op":"eq","path":"uri","value":"miniapp://ship"}}),
        )
        .unwrap();
    let ctx = service
        .call(
            "task.context",
            &json!({
                "task":"miniapp://ship",
                "include":["dependency.receipts.context_exports","dependency.changes.paths"]
            }),
        )
        .unwrap();
    assert_eq!(ctx["dependencies"].as_array().unwrap().len(), 2);

    let ship_ws = service.call("workspace.prepare", &json!({"task":"miniapp://ship"})).unwrap();
    let ship_path = Path::new(ship_ws["worktree"].as_str().unwrap());
    assert!(ship_path.join("app").join("features").join("auth.txt").exists());
    assert!(ship_path.join("app").join("features").join("health.txt").exists());
    fs::write(ship_path.join("RELEASE.md"), "# Miniapp 0.1.0\n- auth\n- health\n").unwrap();
    commit_if_dirty(ship_path, "release notes");
    assert_eq!(
        service.call("gate.run", &json!({"task":"miniapp://ship","gate":"miniapp-verify"})).unwrap()["ok"],
        true
    );
    let health_digest = service.call("receipt.get", &json!({"task":"miniapp://health"})).unwrap()["digest"]
        .as_str()
        .unwrap()
        .to_owned();
    service
        .call(
            "step.advance",
            &json!({
                "task":"miniapp://ship",
                "owner":"agent-ship",
                "receipt": receipt(
                    "miniapp://ship",
                    "release notes published",
                    &["RELEASE.md"],
                    &[
                        ("miniapp://auth".into(), auth_digest),
                        ("miniapp://health".into(), health_digest)
                    ]
                )
            }),
        )
        .unwrap();

    let integrated = service.call("integration.run", &json!({"retain_worktree": true})).unwrap();
    assert_eq!(integrated["merged"].as_array().unwrap().len(), 3);
    assert!(integrated["blocked"].as_array().unwrap().is_empty());
    for uri in ["miniapp://auth", "miniapp://health", "miniapp://ship"] {
        assert_eq!(service.store.get(uri).unwrap().state, "done");
    }

    // Integration tree still proves the product gate.
    let integration_tree = Path::new(integrated["worktree"].as_str().unwrap());
    let proved = Command::new(&smoke.verify).current_dir(integration_tree).output().unwrap();
    assert!(proved.status.success(), "integration tree failed miniapp verify");
    assert!(integration_tree.join("RELEASE.md").exists());
    assert!(integration_tree.join("app").join("features").join("auth.txt").exists());
}
