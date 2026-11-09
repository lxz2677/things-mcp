use std::sync::Arc;

use things_mcp::store::Store;
use things_mcp::sync::SyncEngine;
use things_mcp::testing::FakeTransport;
use things_mcp::things::{ClientOptions, RawReply, ThingsClient};

const META: &str = r#"{"latest-server-index":5,"latest-schema-version":301,"is-empty":false,"latest-total-content-size":100}"#;
const OWN: &str =
    r#"{"history-key":"HK","is-empty":false,"latest-schema-version":301,"latest-server-index":5}"#;

fn page(_start: usize, batches: &[(&str, &str)], head: i64) -> String {
    let items: Vec<String> = batches
        .iter()
        .map(|(uuid, title)| {
            format!(
                r#"{{"{uuid}":{{"t":0,"e":"Task7","p":{{"tt":"{title}","tp":0,"ss":0,"st":0}}}}}}"#
            )
        })
        .collect();
    format!(
        r#"{{"items":[{}],"current-item-index":{head},"schema":301}}"#,
        items.join(",")
    )
}

fn fake(head: i64) -> Arc<FakeTransport> {
    FakeTransport::new(move |request| {
        let url = &request.url;

        if url.ends_with("/own-history-key") {
            return RawReply::json(200, OWN);
        }
        if url.contains("/history/HK/items") {
            let start = url
                .rsplit("start-index=")
                .next()
                .and_then(|value| value.parse::<usize>().ok())
                .unwrap_or(0);
            if start as i64 > head {
                return RawReply::json(500, r#"{"error":"start-index beyond head"}"#);
            }
            return match start {
                0 => RawReply::json(200, page(0, &[("a", "A"), ("b", "B")], head)),
                2 => RawReply::json(200, page(2, &[("c", "C")], head)),
                3 => RawReply::json(200, page(3, &[("d", "D"), ("e", "E")], head)),
                _ => RawReply::json(200, page(start, &[], head)),
            };
        }
        if url.contains("/history/HK") {
            return RawReply::json(200, META);
        }
        if url.contains("/account/") {
            return RawReply::json(
                200,
                r#"{"email":"me@example.com","history-key":"HK","status":"SYAccountStatusActive"}"#,
            );
        }
        RawReply::json(404, "{}")
    })
}

fn items_starts(fake: &FakeTransport) -> Vec<usize> {
    fake.urls()
        .iter()
        .filter(|url| url.contains("/items"))
        .filter_map(|url| {
            url.rsplit("start-index=")
                .next()
                .and_then(|value| value.parse::<usize>().ok())
        })
        .collect()
}

fn client(fake: Arc<FakeTransport>) -> ThingsClient {
    let mut options = ClientOptions::new("me@example.com", "secret", "app-instance");
    options.max_retries = 0;
    options.retry_base_ms = 1;
    options.base_url = "https://cloud.example".to_string();
    ThingsClient::new(fake, options)
}

#[tokio::test]
async fn cursor_advances_by_batch_count_and_stops_at_head() {
    let fake = fake(5);
    let store = Store::open_in_memory().unwrap();
    let mut engine = SyncEngine::new(client(fake.clone()), store);

    let report = engine.sync(false).await.unwrap();

    assert_eq!(report.cursor, 5);
    assert_eq!(report.head, 5);
    assert_eq!(report.pages, 3);
    assert_eq!(report.items, 5);
    assert_eq!(engine.store().task_count().unwrap(), 5);
    assert_eq!(engine.store().latest_server_index().unwrap(), Some(5));

    // A truncated page (2 of 5) must advance by 2, not jump to the head.
    assert_eq!(items_starts(&fake), vec![0, 2, 3]);
    // Never request start-index beyond the head: that returns HTTP 500.
    assert!(items_starts(&fake).iter().all(|start| *start <= 5));
}

#[tokio::test]
async fn second_sync_is_a_noop() {
    let fake = fake(5);
    let store = Store::open_in_memory().unwrap();
    let mut engine = SyncEngine::new(client(fake.clone()), store);

    engine.sync(false).await.unwrap();
    let after_first = items_starts(&fake).len();

    let report = engine.sync(false).await.unwrap();

    assert_eq!(report.cursor, 5);
    assert_eq!(report.pages, 0);
    assert_eq!(report.items, 0);
    assert_eq!(items_starts(&fake).len(), after_first);
}

#[tokio::test]
async fn replays_a_daily_repeat_after_discarding_a_legacy_replica() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("legacy.sqlite");
    {
        let store = Store::open(&path).unwrap();
        store.set_history_key("HK").unwrap();
        store.set_cursor(1).unwrap();
        store
            .connection()
            .execute_batch(
                "ALTER TABLE task DROP COLUMN reminder_time;
                 ALTER TABLE task ADD COLUMN reminder_time INTEGER;
                 INSERT INTO task (id) VALUES ('stale');
                 PRAGMA user_version = 2;",
            )
            .unwrap();
    }
    let store = Store::open(&path).unwrap();
    assert_eq!(store.cursor().unwrap(), 0);
    assert_eq!(store.task_count().unwrap(), 0);
    let cloud = FakeTransport::new(|request| {
        if request.url.ends_with("start-index=0") {
            return RawReply::json(
                200,
                r#"{"items":[{"daily":{"t":0,"e":"Task7","p":{
                    "tt":"Daily repeat","tp":0,"st":2,"ss":0,"sr":1794268800,
                    "ato":32400,"rr":{"fu":16,"fa":1}
                }}}],"current-item-index":1,"schema":301}"#,
            );
        }
        if request.url.ends_with("/own-history-key") {
            return RawReply::json(200, OWN);
        }
        RawReply::json(
            200,
            r#"{"latest-server-index":1,"latest-schema-version":301}"#,
        )
    });
    let mut engine = SyncEngine::new(client(cloud.clone()), store);

    let report = engine.sync(false).await.unwrap();
    assert_eq!(report.cursor, 1);
    let task = engine.store().get_task("daily").unwrap().unwrap();
    assert!(!task.instance_creation_paused);
    assert_eq!(task.instance_creation_start_date, None);
    assert_eq!(task.reminder_time, Some(32_400));
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(task.recurrence_rule.as_deref().unwrap())
            .unwrap(),
        serde_json::json!({"fu": 16, "fa": 1})
    );
    assert_eq!(engine.sync(false).await.unwrap().pages, 0);
    assert_eq!(engine.sync(true).await.unwrap().cursor, 1);
    assert_eq!(items_starts(&cloud), vec![0, 0]);
}

#[tokio::test]
async fn resets_when_cursor_is_ahead_of_the_server() {
    let fake = fake(5);
    let store = Store::open_in_memory().unwrap();
    store.set_cursor(10).unwrap();
    store.set_history_key("HK").unwrap();

    let mut engine = SyncEngine::new(client(fake.clone()), store);
    let report = engine.sync(false).await.unwrap();

    assert!(report.history_replaced);
    assert_eq!(report.cursor, 5);
    assert_eq!(engine.store().task_count().unwrap(), 5);
    assert_eq!(engine.store().latest_server_index().unwrap(), Some(5));
    assert_eq!(items_starts(&fake), vec![0, 2, 3]);
}

#[tokio::test]
async fn failed_full_replay_preserves_the_previous_replica() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("replica.sqlite");
    let store = Store::open(&path).unwrap();
    store.set_app_instance_id("installed-client").unwrap();
    let mut engine = SyncEngine::new(client(fake(5)), store);
    engine.sync(false).await.unwrap();
    let original_sync_time = engine.store().last_sync_at().unwrap();
    drop(engine);

    let failing_cloud = FakeTransport::new(|request| {
        if request.url.ends_with("start-index=0") {
            return RawReply::json(200, page(0, &[("new", "Partial rebuild")], 5));
        }
        if request.url.contains("/items") {
            return RawReply::json(500, r#"{"error":"temporary failure"}"#);
        }
        RawReply::json(200, META)
    });
    let mut engine = SyncEngine::new(client(failing_cloud), Store::open(&path).unwrap());

    assert!(engine.sync(true).await.is_err());
    assert_eq!(engine.store().cursor().unwrap(), 5);
    assert_eq!(engine.store().task_count().unwrap(), 5);
    assert_eq!(engine.store().last_sync_at().unwrap(), original_sync_time);
    assert_eq!(
        engine.store().app_instance_id().unwrap().as_deref(),
        Some("installed-client")
    );
    assert!(engine.store().get_task("a").unwrap().is_some());
    assert!(engine.store().get_task("new").unwrap().is_none());
}

#[tokio::test]
async fn a_rebuild_does_not_overwrite_concurrent_replica_progress() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("replica.sqlite");
    let store = Store::open(&path).unwrap();
    store.set_history_key("HK").unwrap();

    let cloud = FakeTransport::new(move |request| {
        if request.url.ends_with("start-index=0") {
            let mut concurrent = Store::open(&path).unwrap();
            concurrent
                .apply_page(
                    0,
                    &[things_mcp::things::IncomingItem {
                        uuid: "concurrent".into(),
                        action: 0,
                        kind: "Task7".into(),
                        props: serde_json::json!({"tt":"Concurrent task"})
                            .as_object()
                            .unwrap()
                            .clone(),
                        server_index: 0,
                    }],
                    1,
                )
                .unwrap()
                .unwrap();
            return RawReply::json(200, page(0, &[("staged", "Staged task")], 1));
        }
        RawReply::json(200, r#"{"latest-server-index":1}"#)
    });
    let mut engine = SyncEngine::new(client(cloud), store);

    let error = engine.sync(true).await.unwrap_err();
    assert!(
        error
            .to_string()
            .contains("replica changed during the rebuild")
    );
    assert_eq!(engine.store().cursor().unwrap(), 1);
    assert!(engine.store().get_task("concurrent").unwrap().is_some());
    assert!(engine.store().get_task("staged").unwrap().is_none());
}

#[tokio::test]
async fn replaces_the_history_key_when_the_stored_one_is_gone() {
    let fake = FakeTransport::new(|request| {
        let url = &request.url;

        if url.ends_with("/own-history-key") {
            return RawReply::json(
                200,
                r#"{"history-key":"NEW","is-empty":false,"latest-schema-version":301,"latest-server-index":3}"#,
            );
        }
        if url.contains("/history/OLD") {
            return RawReply::json(404, r#"{"IsSyncronyErrorResponse":true}"#);
        }
        if url.contains("/history/NEW/items") {
            if url.ends_with("start-index=0") {
                return RawReply::json(200, page(0, &[("n1", "N1")], 3));
            }
            if url.ends_with("start-index=1") {
                return RawReply::json(200, page(1, &[("n2", "N2")], 3));
            }
            if url.ends_with("start-index=2") {
                return RawReply::json(200, page(2, &[("n3", "N3")], 3));
            }
        }
        if url.contains("/history/NEW") {
            return RawReply::json(
                200,
                r#"{"latest-server-index":3,"latest-schema-version":301,"is-empty":false}"#,
            );
        }
        RawReply::json(404, "{}")
    });

    let mut store = Store::open_in_memory().unwrap();
    store.set_history_key("OLD").unwrap();
    store.set_cursor(10).unwrap();
    store
        .apply_items(&[things_mcp::things::IncomingItem {
            uuid: "stale".into(),
            action: 0,
            kind: "Task7".into(),
            props: serde_json::json!({"tt": "stale", "tp": 0, "ss": 0, "st": 0})
                .as_object()
                .unwrap()
                .clone(),
            server_index: 0,
        }])
        .unwrap();

    let mut engine = SyncEngine::new(client(fake), store);
    let report = engine.sync(false).await.unwrap();

    assert!(report.history_replaced, "must report the replaced history");
    assert_eq!(
        engine.store().history_key().unwrap().as_deref(),
        Some("NEW")
    );
    assert_eq!(engine.store().cursor().unwrap(), 3);
    assert_eq!(engine.store().task_count().unwrap(), 3);
    assert!(
        engine.store().get_task("stale").unwrap().is_none(),
        "old replica must be cleared"
    );
}

#[tokio::test]
async fn stalled_pull_reports_an_error() {
    let fake = FakeTransport::new(|request| {
        let url = &request.url;
        if url.ends_with("/own-history-key") {
            return RawReply::json(200, OWN);
        }
        if url.contains("/items") {
            return RawReply::json(200, r#"{"items":[],"current-item-index":5,"schema":301}"#);
        }
        if url.contains("/history/HK") {
            return RawReply::json(200, META);
        }
        RawReply::json(404, "{}")
    });

    let store = Store::open_in_memory().unwrap();
    let mut engine = SyncEngine::new(client(fake), store);

    let error = engine.sync(false).await.unwrap_err();
    assert!(error.to_string().contains("empty items page"), "{error}");
}
