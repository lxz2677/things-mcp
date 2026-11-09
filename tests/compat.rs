//! Compatibility gate: a newer cloud schema, entity generation, unknown entity
//! kind, or unknown action must stop sync and poison the replica for data tools.

use std::sync::Arc;

use things_mcp::store::Store;
use things_mcp::sync::SyncEngine;
use things_mcp::testing::FakeTransport;
use things_mcp::things::{ClientOptions, RawReply, ThingsClient, ThingsError};

const OWN: &str =
    r#"{"history-key":"HK","is-empty":false,"latest-schema-version":301,"latest-server-index":1}"#;

fn meta(schema: i64) -> String {
    format!(r#"{{"latest-server-index":1,"latest-schema-version":{schema},"is-empty":false}}"#)
}

fn page(schema: i64, item: &str) -> String {
    format!(r#"{{"items":[{item}],"current-item-index":1,"schema":{schema}}}"#)
}

fn client(fake: Arc<FakeTransport>) -> ThingsClient {
    let mut options = ClientOptions::new("me@example.com", "pw", "app");
    options.base_url = "https://cloud.example".to_string();
    options.max_retries = 0;
    ThingsClient::new(fake, options)
}

fn engine(meta_schema: i64, page_schema: i64, item: &'static str) -> SyncEngine {
    SyncEngine::new(
        client(fake_cloud(meta_schema, page_schema, item)),
        Store::open_in_memory().unwrap(),
    )
}

fn fake_cloud(meta_schema: i64, page_schema: i64, item: &'static str) -> Arc<FakeTransport> {
    FakeTransport::new(move |request| {
        let url = &request.url;
        if url.ends_with("/own-history-key") {
            return RawReply::json(200, OWN);
        }
        if url.contains("/history/HK/items") {
            return RawReply::json(200, page(page_schema, item));
        }
        if url.contains("/history/HK") {
            return RawReply::json(200, meta(meta_schema));
        }
        RawReply::json(404, "{}")
    })
}

#[tokio::test]
async fn rejects_a_newer_cloud_schema() {
    let mut engine = engine(302, 302, r#"{"u":{"t":0,"e":"Task7","p":{"tt":"x"}}}"#);

    let error = engine.sync(false).await.unwrap_err();
    assert!(error.to_string().contains("incompatible"), "{error}");
    assert!(error.to_string().contains("302"), "{error}");

    let compat = engine.store().compat().unwrap();
    assert!(compat.checked);
    assert!(!compat.ok);
    assert!(compat.reason.unwrap().contains("302"));
    assert!(engine.store().ensure_compatible().is_err());
}

#[tokio::test]
async fn rejects_a_newer_entity_generation() {
    let mut engine = engine(301, 301, r#"{"u":{"t":0,"e":"Task8","p":{"tt":"x"}}}"#);

    let error = engine.sync(false).await.unwrap_err();
    assert!(error.to_string().contains("Task8"), "{error}");
    let compat = engine.store().compat().unwrap();
    assert!(!compat.ok);
    assert_eq!(
        compat.entities.get("Task"),
        None,
        "Task8 is not recorded as supported"
    );
}

#[tokio::test]
async fn rejects_an_unknown_entity_kind() {
    let mut engine = engine(301, 301, r#"{"u":{"t":0,"e":"Note2","p":{}}}"#);

    let error = engine.sync(false).await.unwrap_err();
    assert!(error.to_string().contains("unknown entity"), "{error}");
    assert!(!engine.store().compat().unwrap().ok);
}

#[tokio::test]
async fn rejects_an_unknown_action() {
    let mut engine = engine(301, 301, r#"{"u":{"t":3,"e":"Task7","p":{}}}"#);

    let error = engine.sync(false).await.unwrap_err();
    assert!(error.to_string().contains("unsupported action"), "{error}");
    assert!(!engine.store().compat().unwrap().ok);
}

#[tokio::test]
async fn records_a_compatible_cloud() {
    let mut engine = engine(301, 301, r#"{"u":{"t":0,"e":"Task7","p":{"tt":"x"}}}"#);

    engine.sync(false).await.unwrap();

    let compat = engine.store().compat().unwrap();
    assert!(compat.checked);
    assert!(compat.ok);
    assert_eq!(compat.cloud_schema, Some(301));
    assert_eq!(compat.entities.get("Task"), Some(&7));
    engine.store().ensure_compatible().unwrap();
}

#[tokio::test]
async fn a_successful_sync_clears_a_previous_incompatibility() {
    let store = Store::open_in_memory().unwrap();
    store
        .set_compat_incompatible("old cloud schema 302", Some(302), &Default::default())
        .unwrap();
    assert!(store.ensure_compatible().is_err());

    let fake = FakeTransport::new(move |request| {
        let url = &request.url;
        if url.ends_with("/own-history-key") {
            return RawReply::json(200, OWN);
        }
        if url.contains("/history/HK/items") {
            return RawReply::json(
                200,
                page(301, r#"{"u":{"t":0,"e":"Task7","p":{"tt":"x"}}}"#),
            );
        }
        if url.contains("/history/HK") {
            return RawReply::json(200, meta(301));
        }
        RawReply::json(404, "{}")
    });

    let mut engine = SyncEngine::new(client(fake), store);
    engine.sync(false).await.unwrap();
    engine.store().ensure_compatible().unwrap();
    let compat = engine.store().compat().unwrap();
    assert!(compat.ok);
    assert_eq!(compat.cloud_schema, Some(301));
    assert_eq!(compat.reason, None);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn concurrent_syncs_do_not_lose_incompatibility() {
    let dir = tempfile::tempdir().unwrap();
    for (full, older_schema, newer_schema) in
        [(false, 301, 302), (true, 301, 302), (false, 302, 301)]
    {
        let path = dir
            .path()
            .join(format!("replica-{full}-{older_schema}.sqlite"));
        let store = Store::open(&path).unwrap();
        store.set_history_key("HK").unwrap();

        let older_cloud = FakeTransport::new(move |request| {
            assert!(request.url.ends_with("/history/HK"));
            // Finish a newer sync while the older one is waiting for its
            // already captured response. Use a separate SQLite connection.
            tokio::task::block_in_place(|| {
                tokio::runtime::Handle::current().block_on(async {
                    let newer_cloud = FakeTransport::new(move |_| {
                        RawReply::json(200, empty_history_meta(newer_schema))
                    });
                    let mut newer =
                        SyncEngine::new(client(newer_cloud), Store::open(&path).unwrap());
                    let result = newer.sync(false).await;
                    assert_eq!(result.is_ok(), newer_schema == 301, "{result:?}");
                });
            });
            RawReply::json(200, empty_history_meta(older_schema))
        });
        let mut older = SyncEngine::new(client(older_cloud), store);

        let error = older.sync(full).await.unwrap_err();
        assert!(matches!(
            error.downcast_ref(),
            Some(ThingsError::IncompatibleVersion(reason)) if reason.contains("302")
        ));
        assert!(older.store().ensure_compatible().is_err());
        assert_eq!(older.store().compat().unwrap().cloud_schema, Some(302));
        assert_eq!(
            older.store().last_sync_at().unwrap().is_some(),
            newer_schema == 301
        );
    }
}

fn empty_history_meta(schema: i64) -> String {
    format!(r#"{{"latest-server-index":0,"latest-schema-version":{schema}}}"#)
}

#[tokio::test]
async fn revalidates_history_even_at_the_cloud_head_on_upgrade() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("replica.sqlite");
    {
        let store = Store::open(&path).unwrap();
        store.set_history_key("HK").unwrap();
        store.set_cursor(1).unwrap();
        store.meta_set("compat_status", "ok").unwrap();
        store
            .connection()
            .execute_batch(
                "INSERT INTO task (id, kind) VALUES ('u', 'Task8');
                 PRAGMA user_version = 2;",
            )
            .unwrap();
    }
    let fake = fake_cloud(
        301,
        301,
        r#"{"u":{"t":0,"e":"Task8","p":{"tt":"from cloud"}}}"#,
    );
    let mut engine = SyncEngine::new(client(fake), Store::open(&path).unwrap());

    let error = engine.sync(false).await.unwrap_err();
    assert!(error.to_string().contains("Task8"), "{error}");
    assert!(engine.store().ensure_compatible().is_err());
    assert_eq!(engine.store().cursor().unwrap(), 0);
    assert!(engine.store().get_task("u").unwrap().is_none());
}
