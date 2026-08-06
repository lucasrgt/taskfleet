mod common;

use std::{
    fs,
    sync::{Arc, Barrier},
    thread,
};

use serde_json::json;
use taskfleet::model::{Config, Filter};

use common::{Fixture, ingest, task};

#[test]
fn structured_filters_cover_scalar_collection_and_boolean_operations() {
    let value = json!({"meta":{"platform":"website","points":3},"tags":["backend","bug"],"title":"Fix booking"});
    let filters: Vec<Filter> = serde_json::from_value(json!([
        {"op":"eq","path":"meta.platform","value":"website"},
        {"op":"ne","path":"meta.platform","value":"travelers"},
        {"op":"gt","path":"meta.points","value":2},
        {"op":"gte","path":"meta.points","value":3},
        {"op":"lt","path":"meta.points","value":4},
        {"op":"lte","path":"meta.points","value":3},
        {"op":"contains","path":"tags","value":"backend"},
        {"op":"contains","path":"title","value":"booking"},
        {"op":"in","path":"meta.platform","values":["website","partners"]},
        {"op":"exists","path":"meta.points"},
        {"op":"and","args":[{"op":"true"},{"op":"eq","path":"meta.points","value":3}]},
        {"op":"or","args":[{"op":"eq","path":"meta.points","value":0},{"op":"true"}]},
        {"op":"not","arg":{"op":"eq","path":"meta.points","value":0}}
    ]))
    .unwrap();
    assert!(filters.iter().all(|filter| filter.matches(&value)));
    assert!(
        !serde_json::from_value::<Filter>(json!({"op":"exists","path":"missing"}))
            .unwrap()
            .matches(&value)
    );
    assert!(
        !serde_json::from_value::<Filter>(json!({"op":"contains","path":"meta.points","value":3}))
            .unwrap()
            .matches(&value)
    );
}

#[test]
fn config_fails_closed_for_schema_workflows_and_gate_references() {
    let fixture = Fixture::new("");
    fs::write(&fixture.config, "schema = 2\n").unwrap();
    assert!(Config::load(&fixture.config).unwrap_err().to_string().contains("unsupported"));
    fs::write(&fixture.config, "schema=1\n[[workflow]]\nid='empty'\n").unwrap();
    assert!(Config::load(&fixture.config).unwrap_err().to_string().contains("no steps"));
    fs::write(
        &fixture.config,
        "schema=1\n[[workflow]]\nid='bad'\n[[workflow.step]]\nid='x'\ngates=['missing']\n",
    )
    .unwrap();
    assert!(Config::load(&fixture.config).unwrap_err().to_string().contains("unknown gate"));
    for (body, message) in [
        ("schema=1\n[[gate]]\nid='bad'\nkind='network'\n", "unsupported gate kind"),
        ("schema=1\n[[gate]]\nid='bad'\n", "has no command"),
        ("schema=1\n[[route]]\nworkflow='missing'\n", "route references"),
        ("schema=1\n[project]\ndefault_workflow='missing'\n", "unknown default"),
        ("schema=1\n[[view]]\nid='same'\n[[view]]\nid='same'\n", "duplicate view"),
        (
            "schema=1\n[[workflow]]\nid='x'\n[[workflow.step]]\nid='same'\n[[workflow.step]]\nid='same'\n",
            "duplicate step",
        ),
    ] {
        fs::write(&fixture.config, body).unwrap();
        assert!(Config::load(&fixture.config).unwrap_err().to_string().contains(message));
    }
    fs::write(&fixture.config, "[project]\nrepository='.'\n").unwrap();
    assert_eq!(Config::load(&fixture.config).unwrap().schema, 1);
}

#[test]
fn one_task_store_projects_into_multiple_views_without_duplicates() {
    let fixture = Fixture::new(
        r#"
[[view]]
id = "travelers"
filter = { op="eq", path="meta.platform", value="travelers" }
[[view]]
id = "all-backend"
filter = { op="contains", path="tags", value="backend" }
"#,
    );
    let mut service = fixture.service();
    ingest(&mut service, vec![task("fibery://task/1", "One", "travelers")]);
    assert_eq!(service.call("task.query", &json!({"view":"travelers"})).unwrap().as_array().unwrap().len(), 1);
    assert_eq!(service.call("task.query", &json!({"view":"all-backend"})).unwrap().as_array().unwrap().len(), 1);
    ingest(
        &mut service,
        vec![json!({"uri":"fibery://task/1","title":"Updated","meta":{"platform":"travelers"}})],
    );
    let tasks = service.call("task.query", &json!({"full":true})).unwrap();
    assert_eq!(tasks.as_array().unwrap().len(), 1);
    assert_eq!(tasks[0]["title"], "Updated");
    assert_eq!(tasks[0]["revision"], 1);
    assert!(
        service
            .call("task.query", &json!({"view":"missing"}))
            .unwrap_err()
            .to_string()
            .contains("unknown view")
    );
}

#[test]
fn dependencies_routes_claims_and_leases_are_transactional() {
    let fixture = Fixture::new(
        r#"
[[workflow]]
id = "web"
[[workflow.step]]
id = "execute"
[[route]]
workflow = "web"
when = { op="eq", path="meta.platform", value="website" }
"#,
    );
    let mut service = fixture.service();
    let blocker = task("jira://A", "A", "website");
    let mut downstream = task("jira://B", "B", "website");
    downstream["depends_on"] = json!(["jira://A"]);
    ingest(&mut service, vec![blocker, downstream]);
    let claimed = service.call("task.claim", &json!({"owner":"agent-1","limit":2,"lease_seconds":30})).unwrap();
    assert_eq!(claimed.as_array().unwrap().len(), 1);
    assert_eq!(claimed[0]["task"]["uri"], "jira://A");
    assert_eq!(claimed[0]["execution"]["workflow"], "web");
    assert_eq!(
        service
            .call("task.query", &json!({"ready":true,"states":["backlog"]}))
            .unwrap()
            .as_array()
            .unwrap()
            .len(),
        0
    );
    assert!(service.call("task.heartbeat", &json!({"task":"jira://A","owner":"wrong"})).is_err());
    let heartbeat = service
        .call("task.heartbeat", &json!({"task":"jira://A","owner":"agent-1","lease_seconds":60}))
        .unwrap();
    assert!(heartbeat["lease_until"].as_i64().unwrap() > 0);
    assert!(
        service
            .store
            .claim(&["jira://A".into()], "other", 1, &[None])
            .unwrap_err()
            .to_string()
            .contains("could not be claimed")
    );
}

#[test]
fn ingest_rejects_invalid_tasks_and_dependency_cycles_atomically() {
    let fixture = Fixture::new("");
    let mut service = fixture.service();
    assert!(service.call("task.ingest", &json!({"tasks":[{"uri":"","title":""}]})).is_err());
    let mut one = task("task://one", "One", "x");
    one["depends_on"] = json!(["task://two", "task://one"]);
    let mut two = task("task://two", "Two", "x");
    two["depends_on"] = json!(["task://one"]);
    assert!(
        service
            .call("task.ingest", &json!({"tasks":[one,two]}))
            .unwrap_err()
            .to_string()
            .contains("cycle")
    );
    assert!(service.call("task.query", &json!({})).unwrap().as_array().unwrap().is_empty());
    let mut self_cycle = task("task://self", "Self", "x");
    self_cycle["depends_on"] = json!(["task://self"]);
    assert!(service.call("task.ingest", &json!({"tasks":[self_cycle]})).is_err());
}

#[test]
fn filters_fail_closed_for_incompatible_values_and_match_object_keys() {
    let value = json!({"meta":{"key":"value"},"name":"z"});
    assert!(
        serde_json::from_value::<Filter>(json!({"op":"contains","path":"meta","value":"key"}))
            .unwrap()
            .matches(&value)
    );
    assert!(
        serde_json::from_value::<Filter>(json!({"op":"gt","path":"name","value":"a"}))
            .unwrap()
            .matches(&value)
    );
    assert!(
        !serde_json::from_value::<Filter>(json!({"op":"gt","path":"name","value":1}))
            .unwrap()
            .matches(&value)
    );
}

#[test]
fn reconcile_expires_dead_owners_and_retry_is_explicit() {
    let fixture = Fixture::new("");
    let mut service = fixture.service();
    ingest(&mut service, vec![task("task://1", "One", "x")]);
    service.call("task.claim", &json!({"owner":"agent"})).unwrap();
    service.store.0.execute("UPDATE task SET lease_until=0", []).unwrap();
    assert_eq!(service.call("reconcile", &json!({})).unwrap()["expired_leases"], 1);
    assert_eq!(service.call("task.get", &json!({"task":"task://1"})).unwrap()["execution"]["state"], "backlog");
    service.call("task.claim", &json!({"owner":"agent"})).unwrap();
    service.call("task.block", &json!({"task":"task://1","reason":"external blocker"})).unwrap();
    service.store.0.execute("UPDATE task SET lease_until=0", []).unwrap();
    assert_eq!(service.call("reconcile", &json!({})).unwrap()["expired_leases"], 0);
    assert_eq!(service.call("task.get", &json!({"task":"task://1"})).unwrap()["execution"]["state"], "blocked");
    assert_eq!(service.call("task.retry", &json!({"task":"task://1"})).unwrap()["state"], "backlog");
    assert!(service.call("task.retry", &json!({"task":"task://1"})).is_err());
}

#[test]
fn concurrent_services_do_not_duplicate_a_claim() {
    let fixture = Fixture::new("");
    let mut service = fixture.service();
    ingest(&mut service, vec![task("task://race", "Race", "x")]);
    drop(service);
    let barrier = Arc::new(Barrier::new(2));
    let handles = ["agent-a", "agent-b"].map(|owner| {
        let config = fixture.config.clone();
        let barrier = barrier.clone();
        thread::spawn(move || {
            let mut service = taskfleet::Service::open(&config).unwrap();
            barrier.wait();
            service.call("task.claim", &json!({"owner":owner})).map(|value| value.as_array().unwrap().len())
        })
    });
    let outcomes = handles.into_iter().map(|handle| handle.join().unwrap()).collect::<Vec<_>>();
    assert_eq!(outcomes.iter().filter(|result| result.is_ok()).count(), 1);
    assert_eq!(outcomes.into_iter().filter_map(Result::ok).sum::<usize>(), 1);
}

#[test]
fn corrupt_database_fails_open_with_a_diagnostic() {
    let fixture = Fixture::new("");
    let database = fixture.repo.join(".taskfleet/state.sqlite");
    fs::create_dir_all(database.parent().unwrap()).unwrap();
    fs::write(&database, b"not a sqlite database").unwrap();
    let error = match taskfleet::Service::open(&fixture.config) {
        Ok(_) => panic!("corrupt database opened"),
        Err(error) => error,
    };
    assert!(error.to_string().contains("database"));
}

#[test]
fn durable_controls_order_claims_and_fail_closed_dependencies() {
    let fixture = Fixture::new("");
    let mut service = fixture.service();
    let a = task("task://a", "A", "x");
    let b = task("task://b", "B", "x");
    let mut dependent = task("task://dependent", "Dependent", "x");
    dependent["depends_on"] = json!(["task://a"]);
    ingest(&mut service, vec![a, b, dependent]);
    service.call("task.reprioritize", &json!({"task":"task://a","priority":5})).unwrap();
    service.call("task.reprioritize", &json!({"task":"task://b","priority":50})).unwrap();
    let query = service.call("task.query", &json!({})).unwrap();
    assert_eq!(query[0]["uri"], "task://b");
    assert_eq!(query[0]["queue_priority"], 50);
    let claim = service.call("task.claim", &json!({"owner":"agent"})).unwrap();
    assert_eq!(claim[0]["task"]["uri"], "task://b");

    let paused = service.call("task.pause", &json!({"task":"task://b"})).unwrap();
    assert_eq!(paused["execution"]["state"], "backlog");
    assert_eq!(paused["execution"]["paused"], true);
    assert!(paused["execution"]["owner"].is_null());
    assert!(service.call("task.heartbeat", &json!({"task":"task://b","owner":"agent"})).is_err());
    assert!(
        service
            .call("task.query", &json!({"ready":true}))
            .unwrap()
            .as_array()
            .unwrap()
            .iter()
            .all(|row| row["uri"] != "task://b")
    );
    assert_eq!(service.call("reconcile", &json!({})).unwrap()["expired_leases"], 0);
    drop(service);

    let mut service = fixture.service();
    let paused = service.call("task.get", &json!({"task":"task://b"})).unwrap();
    assert_eq!(paused["execution"]["paused"], true);
    assert_eq!(paused["execution"]["queue_priority"], 50);
    service.call("task.resume", &json!({"task":"task://b"})).unwrap();
    assert_eq!(
        service.call("task.claim", &json!({"owner":"replacement"})).unwrap()[0]["task"]["uri"],
        "task://b"
    );
    service.call("task.cancel", &json!({"task":"task://b","reason":"obsolete"})).unwrap();
    let cancelled = service.call("task.get", &json!({"task":"task://b"})).unwrap();
    assert_eq!(cancelled["execution"]["state"], "cancelled");
    assert_eq!(cancelled["execution"]["error"], "obsolete");
    assert!(service.call("task.resume", &json!({"task":"task://b"})).is_err());
    ingest(&mut service, vec![task("task://b", "B updated", "x")]);
    assert_eq!(
        service.call("task.get", &json!({"task":"task://b"})).unwrap()["execution"]["state"],
        "cancelled"
    );

    service.call("task.cancel", &json!({"task":"task://a"})).unwrap();
    let ready = service.call("task.query", &json!({"ready":true})).unwrap();
    assert!(ready.as_array().unwrap().iter().all(|row| row["uri"] != "task://dependent"));
}

#[test]
fn stale_execution_cannot_overwrite_a_pause_or_cancel() {
    let fixture = Fixture::new("");
    let mut first = fixture.service();
    ingest(&mut first, vec![task("task://race-control", "Race", "x")]);
    first.call("task.claim", &json!({"owner":"agent"})).unwrap();
    let stale = first.store.get("task://race-control").unwrap();
    let mut second = fixture.service();
    second.call("task.pause", &json!({"task":"task://race-control"})).unwrap();
    assert!(
        first
            .store
            .execution(&stale.task.uri, &stale.state, "blocked", stale.step, stale.owner.as_deref(), Some("late"))
            .is_err()
    );
    let row = second.store.get("task://race-control").unwrap();
    assert!(row.paused);
    assert_eq!(row.state, "backlog");
    second.call("task.resume", &json!({"task":"task://race-control"})).unwrap();
    second.call("task.cancel", &json!({"task":"task://race-control"})).unwrap();
    assert!(
        first
            .store
            .execution(&stale.task.uri, &stale.state, "candidate", stale.step, None, None)
            .is_err()
    );
    assert_eq!(second.store.get("task://race-control").unwrap().state, "cancelled");
}

#[test]
fn current_databases_migrate_and_newer_schemas_fail_closed() {
    let fixture = Fixture::new("");
    let database = fixture.repo.join(".taskfleet/state.sqlite");
    fs::create_dir_all(database.parent().unwrap()).unwrap();
    let connection = rusqlite::Connection::open(&database).unwrap();
    connection.execute_batch("CREATE TABLE task(uri TEXT PRIMARY KEY, doc TEXT NOT NULL, state TEXT NOT NULL DEFAULT 'backlog', workflow TEXT, step INTEGER NOT NULL DEFAULT 0, owner TEXT, lease_until INTEGER, branch TEXT, worktree TEXT, error TEXT, revision INTEGER NOT NULL DEFAULT 0); PRAGMA user_version=0;").unwrap();
    drop(connection);
    let service = fixture.service();
    let version: i64 = service.store.0.pragma_query_value(None, "user_version", |row| row.get(0)).unwrap();
    assert_eq!(version, 1);
    let columns = service
        .store
        .0
        .prepare("PRAGMA table_info(task)")
        .unwrap()
        .query_map([], |row| row.get::<_, String>(1))
        .unwrap()
        .collect::<rusqlite::Result<Vec<_>>>()
        .unwrap();
    assert!(columns.contains(&"paused".into()) && columns.contains(&"queue_priority".into()));
    drop(service);
    let connection = rusqlite::Connection::open(&database).unwrap();
    connection.pragma_update(None, "user_version", 2).unwrap();
    drop(connection);
    let error = match taskfleet::Service::open(&fixture.config) {
        Ok(_) => panic!("newer schema opened"),
        Err(error) => error,
    };
    assert!(error.to_string().contains("unsupported database schema"));
}
