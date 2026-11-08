//! Replay a realistic two-page history through the sync engine: creates for
//! every entity kind, then updates, a notes patch, a hard delete, a tombstone,
//! and a settings item. This exercises the whole decode → SQLite pipeline
//! without needing cloud credentials.

use std::sync::Arc;

use serde_json::json;
use things_mcp::store::{ListKind, Store, list_tasks, project_detail};
use things_mcp::sync::SyncEngine;
use things_mcp::testing::FakeTransport;
use things_mcp::things::{ClientOptions, RawReply, ThingsClient};

const OWN: &str =
    r#"{"history-key":"HK","is-empty":false,"latest-schema-version":301,"latest-server-index":11}"#;
const META: &str = r#"{"latest-server-index":11,"latest-schema-version":301,"is-empty":false}"#;

fn page(batches: &[serde_json::Value], head: i64) -> String {
    let items = batches
        .iter()
        .map(|batch| batch.to_string())
        .collect::<Vec<_>>()
        .join(",");
    format!(r#"{{"items":[{items}],"current-item-index":{head},"schema":301}}"#)
}

fn fake() -> Arc<FakeTransport> {
    FakeTransport::new(|request| {
        let url = &request.url;

        if url.ends_with("/own-history-key") {
            return RawReply::json(200, OWN);
        }
        if url.contains("/history/HK/items") {
            if url.ends_with("start-index=0") {
                return RawReply::json(
                    200,
                    page(
                        &[
                            json!({"area1": {"t":0,"e":"Area3","p":{"tt":"Work","ix":0,"tg":[],"xx":{"sn":{},"_t":"oo"}}}}),
                            json!({"tag1": {"t":0,"e":"Tag4","p":{"tt":"urgent","ix":0,"pn":[],"sh":null,"xx":{}}}}),
                            json!({"proj1": {"t":0,"e":"Task7","p":{"tt":"Launch","tp":1,"ss":0,"st":1,"ar":["area1"],"tg":["tag1"]}}}),
                            json!({"task1": {"t":0,"e":"Task7","p":{
                                "tt":"Write docs","tp":0,"ss":0,"st":0,
                                "pr":["proj1"],"ar":["area1"],"tg":["tag1"],
                                "nt":{"_t":"tx","ch":0,"v":"first line\nsecond"},
                                "ix":-10
                            }}}),
                            json!({"ci1": {"t":0,"e":"ChecklistItem3","p":{
                                "tt":"step 1","ss":0,"ix":0,"ts":["task1"],
                                "lt":false,"cd":1.0,"md":1.0,"sp":null
                            }}}),
                        ],
                        11,
                    ),
                );
            }
            if url.ends_with("start-index=5") {
                return RawReply::json(
                    200,
                    page(
                        &[
                            // Completed, with the trailing st=1 the real client sends.
                            json!({"task1": {"t":1,"e":"Task7","p":{
                                "ss":3,"sp":1700000000.5,"st":1,"md":1700000001.25
                            }}}),
                            // Notes patch: replace the first line ("first line", 10 bytes).
                            json!({"task1": {"t":1,"e":"Task7","p":{
                                "nt":{"t":2,"ps":[{"p":0,"l":10,"r":"first"}]}
                            }}}),
                            // Two entities in one batch: create then hard delete.
                            json!({
                                "task2": {"t":0,"e":"Task7","p":{"tt":"Temp","tp":0,"ss":0,"st":0}},
                                "task3": {"t":0,"e":"Task7","p":{"tt":"Keep","tp":0,"ss":0,"st":0}}
                            }),
                            json!({"task2": {"t":2,"e":"Task7","p":{}}}),
                            // Tombstone for the tag.
                            json!({"stone1": {"t":0,"e":"Tombstone2","p":{"dld":1700000002.0,"dloid":"tag1"}}}),
                            // Settings are ignored by the read model.
                            json!({"set1": {"t":0,"e":"Settings5","p":{"grpt":0,"ld":0,"li":0,"usat":null,"xx":{}}}}),
                        ],
                        11,
                    ),
                );
            }
        }
        if url.contains("/history/HK") {
            return RawReply::json(200, META);
        }
        RawReply::json(404, "{}")
    })
}

fn client(fake: Arc<FakeTransport>) -> ThingsClient {
    let mut options = ClientOptions::new("me@example.com", "secret", "app-instance");
    options.base_url = "https://cloud.example".to_string();
    options.max_retries = 0;
    ThingsClient::new(fake, options)
}

#[tokio::test]
async fn replays_creates_updates_patches_deletes_and_tombstones() {
    let store = Store::open_in_memory().unwrap();
    let mut engine = SyncEngine::new(client(fake()), store);

    let report = engine.sync(false).await.unwrap();
    assert_eq!(report.cursor, 11);
    assert_eq!(report.pages, 2);

    let store = engine.store();
    let today = things_mcp::fmt::today_epoch_day();

    // Project + area + tag create.
    let project = project_detail(store.connection(), "proj1", today, 50)
        .unwrap()
        .expect("project");
    assert_eq!(project.summary.title, "Launch");
    assert_eq!(project.summary.area.as_deref(), Some("area1"));
    assert_eq!(project.summary.open_tasks, 0);
    assert_eq!(project.summary.completed_tasks, 1);

    // Task: update merged onto the create, notes patch applied once.
    let task = store.get_task("task1").unwrap().expect("task1");
    assert_eq!(task.title, "Write docs");
    assert_eq!(task.status, 3);
    assert_eq!(task.start, 1);
    assert_eq!(task.stop_date, Some(1_700_000_000.5));
    assert_eq!(task.user_modification_date, Some(1_700_000_001.25));
    assert_eq!(task.notes, "first\nsecond");
    assert_eq!(task.project.as_deref(), Some("proj1"));
    assert_eq!(task.area.as_deref(), Some("area1"));

    // The tag was tombstoned, so its row and junction rows are gone.
    assert!(store.get_tag("tag1").unwrap().is_none());
    assert!(task.tags.is_empty(), "tombstoned tag must be detached");

    // Checklist item keeps its parent.
    let checklist = store.checklist_for("task1").unwrap();
    assert_eq!(checklist.len(), 1);
    assert_eq!(checklist[0].title, "step 1");
    assert_eq!(checklist[0].task.as_deref(), Some("task1"));

    // Hard delete removed task2; task3 survived.
    assert!(store.get_task("task2").unwrap().is_none());
    assert!(store.get_task("task3").unwrap().is_some());

    // List semantics on the replayed data.
    let logbook = list_tasks(store.connection(), &ListKind::Logbook, today, 50, 0).unwrap();
    let titles: Vec<&str> = logbook.iter().map(|task| task.title.as_str()).collect();
    assert_eq!(titles, vec!["Write docs"]);

    let all = list_tasks(store.connection(), &ListKind::All, today, 50, 0).unwrap();
    let titles: Vec<&str> = all.iter().map(|task| task.title.as_str()).collect();
    assert_eq!(titles, vec!["Keep"]);
}
