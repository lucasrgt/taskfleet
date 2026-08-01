mod common;

use std::fs;

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
