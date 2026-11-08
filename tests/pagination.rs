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
