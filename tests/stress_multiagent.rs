//! Multi-agent stress benchmark for Workspace Capsules.
//!
//! Simulates a harness dispatching many agents across a dependency DAG:
//!
//! ```text
//! root-00 .. root-(N-1)   (wave 1, parallel-ready)
//!           │
//!           ▼
//!      reconcile          (wave 2)
//!        /      \
//!    audit-a  audit-b     (wave 3, parallel)
//!        \      /
//!           ▼
//!         final           (wave 4)
//! ```
//!
//! Proves: candidate readiness, dependency branch merge, TaskReceipt publish,
//! task.context recovery, claim exclusivity under contention, and integration.
//!
//! Scale with `STRESS_ROOTS` (default 12). Example:
//! `STRESS_ROOTS=24 cargo test --test stress_multiagent -- --nocapture`

mod common;

use std::{
    env,
    path::Path,
    sync::{Arc, Barrier, Mutex},
    thread,
    time::Instant,
};

use serde_json::{Value, json};

use common::{Fixture, commit, ingest, task};

fn roots() -> usize {
    env::var("STRESS_ROOTS").ok().and_then(|value| value.parse().ok()).unwrap_or(12).clamp(4, 64)
}

fn empty_digest() -> String {
    taskfleet::cas::digest_bytes(b"")
}

fn receipt(task_uri: &str, summary: &str, paths: &[&str], dep_digests: &[(String, String)]) -> Value {
    let digest = empty_digest();
    json!({
        "schema_version": 1,
        "task_uri": task_uri,
        "step_id": "execute",
        "producer": {"agent_id": "stress-agent", "harness": "stress", "model": null},
        "source": {
            "base_tree": "base",
            "result_tree": "result",
            "branch": null,
            "commit": null,
            "workspace_provider": "git-worktree",
            "workspace_id": null
        },
        "dependencies": dep_digests.iter().map(|(uri, dig)| json!({"task_uri": uri, "receipt_digest": dig})).collect::<Vec<_>>(),
        "changes": {"paths": paths, "patch_digest": digest},
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
            "decisions": [format!("decision for {task_uri}")],
            "assumptions": [],
            "symbols": [format!("Sym_{task_uri}")],
            "contracts": [format!("{task_uri} exports surface")],
            "followups": []
        },
        "fingerprint": {
            "input_digest": digest,
            "lockfiles": {},
            "toolchain": {"rustc": "1"}
        },
        "created_at": "2026-08-07T12:00:00Z"
    })
}

fn finish_agent(service: &mut taskfleet::Service, uri: &str, owner: &str, file: &str, body: &str, summary: &str, deps: &[(String, String)]) {
    let workspace = service.call("workspace.prepare", &json!({"task": uri})).unwrap();
    let path = Path::new(workspace["worktree"].as_str().unwrap());
    commit(path, file, body);
    let published = service
        .call(
            "step.advance",
            &json!({
                "task": uri,
                "owner": owner,
                "receipt": receipt(uri, summary, &[file], deps)
            }),
        )
        .unwrap();
    assert_eq!(published["state"], "candidate");
    assert!(published["receipt"]["digest"].as_str().unwrap().starts_with("sha256:"));
}

#[test]
fn multiagent_dag_stress_completes_with_context_merge_and_integration() {
    let n = roots();
    let started = Instant::now();
    let fixture = Fixture::new("");
    let mut service = fixture.service();

    let mut tasks = Vec::new();
    for index in 0..n {
        let mut item = task(&format!("stress://root-{index:02}"), &format!("Root {index:02}"), "wave1");
        item["meta"]["wave"] = json!(1);
        item["meta"]["index"] = json!(index);
        tasks.push(item);
    }
    let root_uris = (0..n).map(|index| format!("stress://root-{index:02}")).collect::<Vec<_>>();

    let mut reconcile = task("stress://reconcile", "Reconcile", "wave2");
    reconcile["depends_on"] = json!(root_uris.clone());
    reconcile["meta"]["wave"] = json!(2);
    tasks.push(reconcile);

    let mut audit_a = task("stress://audit-a", "Audit A", "wave3");
    audit_a["depends_on"] = json!(["stress://reconcile"]);
    audit_a["meta"]["wave"] = json!(3);
    tasks.push(audit_a);

    let mut audit_b = task("stress://audit-b", "Audit B", "wave3");
    audit_b["depends_on"] = json!(["stress://reconcile"]);
    audit_b["meta"]["wave"] = json!(3);
    tasks.push(audit_b);

    let mut final_task = task("stress://final", "Final", "wave4");
    final_task["depends_on"] = json!(["stress://audit-a", "stress://audit-b"]);
    final_task["meta"]["wave"] = json!(4);
    tasks.push(final_task);

    let ingest_started = Instant::now();
    ingest(&mut service, tasks);
    let ingest_ms = ingest_started.elapsed().as_millis();

    // Wave 1: N distinct agents claim one root each.
    let wave1_started = Instant::now();
    let mut root_receipts = Vec::new();
    for (index, uri) in root_uris.iter().enumerate() {
        let owner = format!("agent-root-{index:02}");
        let claimed = service
            .call(
                "task.claim",
                &json!({"owner": owner, "filter": {"op":"eq","path":"uri","value": uri}, "limit": 1}),
            )
            .unwrap();
        assert_eq!(claimed.as_array().unwrap().len(), 1);
        let file = format!("root-{index:02}.txt");
        finish_agent(
            &mut service,
            uri,
            &owner,
            &file,
            &format!("payload-{index:02}\n"),
            &format!("root {index:02} complete"),
            &[],
        );
        let got = service.call("receipt.get", &json!({"task": uri})).unwrap();
        root_receipts.push((uri.clone(), got["digest"].as_str().unwrap().to_owned()));
    }
    let wave1_ms = wave1_started.elapsed().as_millis();

    // No wave-1 leftovers claimable; reconcile is now ready.
    let ready = service.call("task.query", &json!({"ready": true, "states": ["backlog"]})).unwrap();
    let ready_uris = ready
        .as_array()
        .unwrap()
        .iter()
        .map(|row| row["uri"].as_str().unwrap().to_owned())
        .collect::<Vec<_>>();
    assert!(ready_uris.contains(&"stress://reconcile".to_owned()));
    assert!(!ready_uris.iter().any(|uri| uri.starts_with("stress://root-")));

    // Wave 2: reconcile recovers every root receipt and merges every root branch.
    let wave2_started = Instant::now();
    service
        .call(
            "task.claim",
            &json!({"owner":"agent-reconcile","filter":{"op":"eq","path":"uri","value":"stress://reconcile"}}),
        )
        .unwrap();
    let context = service
        .call(
            "task.context",
            &json!({
                "task": "stress://reconcile",
                "include": ["dependency.receipts.context_exports", "dependency.changes.paths", "dependency.artifacts[role=proof]"],
                "budget_bytes": 4_194_304
            }),
        )
        .unwrap();
    assert_eq!(context["dependencies"].as_array().unwrap().len(), n);
    for (uri, digest) in &root_receipts {
        let found = context["dependencies"]
            .as_array()
            .unwrap()
            .iter()
            .find(|item| item["task_uri"] == *uri)
            .unwrap();
        assert_eq!(found["receipt_digest"], *digest);
        assert!(found["context_exports"]["summary"].as_str().unwrap().contains("complete"));
    }

    let prepared = service.call("workspace.prepare", &json!({"task":"stress://reconcile"})).unwrap();
    assert_eq!(prepared["merged_dependencies"].as_array().unwrap().len(), n);
    let reconcile_path = Path::new(prepared["worktree"].as_str().unwrap());
    for index in 0..n {
        let file = reconcile_path.join(format!("root-{index:02}.txt"));
        let body = std::fs::read_to_string(&file).unwrap().replace("\r\n", "\n");
        assert_eq!(body, format!("payload-{index:02}\n"));
    }
    finish_agent(
        &mut service,
        "stress://reconcile",
        "agent-reconcile",
        "reconcile.txt",
        "merged\n",
        "reconcile complete",
        &root_receipts,
    );
    let wave2_ms = wave2_started.elapsed().as_millis();

    // Wave 3: two audits in parallel readiness after reconcile.
    let wave3_started = Instant::now();
    for (owner, uri, file) in [
        ("agent-audit-a", "stress://audit-a", "audit-a.txt"),
        ("agent-audit-b", "stress://audit-b", "audit-b.txt"),
    ] {
        service
            .call("task.claim", &json!({"owner": owner, "filter": {"op":"eq","path":"uri","value": uri}}))
            .unwrap();
        let ctx = service.call("task.context", &json!({"task": uri})).unwrap();
        assert_eq!(ctx["dependencies"].as_array().unwrap().len(), 1);
        assert_eq!(ctx["dependencies"][0]["task_uri"], "stress://reconcile");
        let prepared = service.call("workspace.prepare", &json!({"task": uri})).unwrap();
        assert!(Path::new(prepared["worktree"].as_str().unwrap()).join("reconcile.txt").exists());
        let reconcile_digest = service.call("receipt.get", &json!({"task":"stress://reconcile"})).unwrap()["digest"]
            .as_str()
            .unwrap()
            .to_owned();
        finish_agent(
            &mut service,
            uri,
            owner,
            file,
            &format!("{owner}\n"),
            &format!("{uri} complete"),
            &[("stress://reconcile".into(), reconcile_digest)],
        );
    }
    let wave3_ms = wave3_started.elapsed().as_millis();

    // Wave 4: final controller.
    let wave4_started = Instant::now();
    service
        .call(
            "task.claim",
            &json!({"owner":"agent-final","filter":{"op":"eq","path":"uri","value":"stress://final"}}),
        )
        .unwrap();
    let final_context = service.call("task.context", &json!({"task":"stress://final","include":["*"]})).unwrap();
    assert_eq!(final_context["dependencies"].as_array().unwrap().len(), 2);
    let prepared = service.call("workspace.prepare", &json!({"task":"stress://final"})).unwrap();
    let final_path = Path::new(prepared["worktree"].as_str().unwrap());
    assert!(final_path.join("audit-a.txt").exists());
    assert!(final_path.join("audit-b.txt").exists());
    assert!(final_path.join("reconcile.txt").exists());
    for index in 0..n {
        assert!(final_path.join(format!("root-{index:02}.txt")).exists());
    }
    let audit_a = service.call("receipt.get", &json!({"task":"stress://audit-a"})).unwrap()["digest"]
        .as_str()
        .unwrap()
        .to_owned();
    let audit_b = service.call("receipt.get", &json!({"task":"stress://audit-b"})).unwrap()["digest"]
        .as_str()
        .unwrap()
        .to_owned();
    finish_agent(
        &mut service,
        "stress://final",
        "agent-final",
        "final.txt",
        "done\n",
        "final complete",
        &[("stress://audit-a".into(), audit_a), ("stress://audit-b".into(), audit_b)],
    );
    let wave4_ms = wave4_started.elapsed().as_millis();

    // Integration of all candidates in dependency order.
    let integration_started = Instant::now();
    let integrated = service.call("integration.run", &json!({})).unwrap();
    let integration_ms = integration_started.elapsed().as_millis();
    assert_eq!(integrated["merged"].as_array().unwrap().len(), n + 4);
    assert!(integrated["blocked"].as_array().unwrap().is_empty());

    for uri in root_uris.iter().cloned().chain([
        "stress://reconcile".into(),
        "stress://audit-a".into(),
        "stress://audit-b".into(),
        "stress://final".into(),
    ]) {
        assert_eq!(service.store.get(&uri).unwrap().state, "done");
    }

    let gc = service.call("workspace.gc", &json!({})).unwrap();
    let total_ms = started.elapsed().as_millis();
    let tasks_total = n + 4;
    eprintln!(
        "\n=== multiagent stress results ===\n\
         roots={n} total_tasks={tasks_total}\n\
         ingest_ms={ingest_ms}\n\
         wave1_roots_ms={wave1_ms} ({:.2} tasks/s)\n\
         wave2_reconcile_ms={wave2_ms}\n\
         wave3_audits_ms={wave3_ms}\n\
         wave4_final_ms={wave4_ms}\n\
         integration_ms={integration_ms}\n\
         gc_kept={}\n\
         total_ms={total_ms} ({:.2} tasks/s end-to-end)\n",
        n as f64 / (wave1_ms.max(1) as f64 / 1000.0),
        gc["kept"],
        tasks_total as f64 / (total_ms.max(1) as f64 / 1000.0),
    );
}

#[test]
fn concurrent_claims_never_double_lease_the_same_task() {
    let n = roots().clamp(8, 32);
    let fixture = Fixture::new("");
    let mut bootstrap = fixture.service();
    let tasks = (0..n)
        .map(|index| task(&format!("stress://race-{index:02}"), &format!("Race {index:02}"), "race"))
        .collect::<Vec<_>>();
    ingest(&mut bootstrap, tasks);
    drop(bootstrap);

    let barrier = Arc::new(Barrier::new(n));
    let claimed = Arc::new(Mutex::new(Vec::<String>::new()));
    let config = fixture.config.clone();
    let mut handles = Vec::new();
    let started = Instant::now();
    for index in 0..n {
        let barrier = Arc::clone(&barrier);
        let claimed = Arc::clone(&claimed);
        let config = config.clone();
        handles.push(thread::spawn(move || {
            let mut service = taskfleet::Service::open(&config).unwrap();
            barrier.wait();
            let owner = format!("racer-{index:02}");
            for _ in 0..64 {
                match service.call("task.claim", &json!({"owner": owner, "limit": 1})) {
                    Ok(value) => {
                        let Some(uri) = value.as_array().and_then(|items| items.first()).and_then(|item| item["task"]["uri"].as_str()) else {
                            break;
                        };
                        claimed.lock().unwrap().push(uri.to_owned());
                        break;
                    }
                    Err(_) => {
                        // Another writer won the same candidate; retry.
                        thread::yield_now();
                    }
                }
            }
        }));
    }
    for handle in handles {
        handle.join().unwrap();
    }
    let elapsed_ms = started.elapsed().as_millis();
    let uris = claimed.lock().unwrap().clone();
    let mut unique = uris.clone();
    unique.sort();
    unique.dedup();
    assert_eq!(uris.len(), unique.len(), "duplicate leases detected: {uris:?}");
    assert_eq!(uris.len(), n, "expected every task claimed exactly once");

    let mut service = taskfleet::Service::open(&config).unwrap();
    let leftovers = service.call("task.query", &json!({"ready": true, "states": ["backlog"]})).unwrap();
    assert!(leftovers.as_array().unwrap().is_empty());
    eprintln!(
        "\n=== concurrent claim stress ===\nagents={n} unique_leases={} elapsed_ms={elapsed_ms}\n",
        unique.len()
    );
}
