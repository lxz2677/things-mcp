//! Write path: build gold-standard payloads, push them, then apply locally.
//!
//! Safety model:
//!
//! * creates are validated against the 34-key Task7 snapshot (or the 9/4/5-key
//!   sets for checklist items, areas, and tags) **before** any network call;
//! * writes only happen when `THINGS_ENABLE_WRITES=1` is set – `dry_run` works
//!   without it and returns the exact payload that would be pushed;
//! * we commit **before** applying locally, so a failed push never leaves the
//!   local replica claiming something the cloud does not have;
//! * on an ancestor conflict we pull once and retry the same change.

use anyhow::{Result, anyhow, bail};
use serde::Serialize;
use serde_json::{Map, Value, json};

use crate::fmt;
use crate::ids;
use crate::store::Store;
use crate::sync::SyncEngine;
use crate::things::encode::{self, TaskDraft, When};
use crate::things::{ACTION_CREATE, ACTION_DELETE, ACTION_UPDATE, IncomingItem, ThingsError};

#[derive(Debug, Default, Clone)]
pub struct TaskPatch {
    pub title: Option<String>,
    pub notes: Option<String>,
    pub when: Option<When>,
    /// `Some(None)` clears the deadline.
    pub deadline: Option<Option<i64>>,
    pub project: Option<Option<String>>,
    pub area: Option<Option<String>>,
    pub heading: Option<Option<String>>,
    pub tags: Option<Vec<String>>,
}

impl TaskPatch {
    pub fn is_empty(&self) -> bool {
        self.title.is_none()
            && self.notes.is_none()
            && self.when.is_none()
            && self.deadline.is_none()
            && self.project.is_none()
            && self.area.is_none()
            && self.heading.is_none()
            && self.tags.is_none()
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct WriteOutcome {
    pub status: String,
    pub id: String,
    pub kind: String,
    pub payload: Value,
    pub pushed: bool,
    pub server_head_index: Option<i64>,
}

/// Read the `THINGS_ENABLE_WRITES` switch.
pub fn writes_enabled_from_env() -> bool {
    std::env::var("THINGS_ENABLE_WRITES")
        .map(|value| {
            matches!(
                value.trim().to_ascii_lowercase().as_str(),
                "1" | "true" | "yes" | "on"
            )
        })
        .unwrap_or(false)
}

pub struct WriteService {
    engine: SyncEngine,
    writes_enabled: bool,
}

impl WriteService {
    pub fn new(engine: SyncEngine, writes_enabled: bool) -> Self {
        Self {
            engine,
            writes_enabled,
        }
    }

    /// `THINGS_ENABLE_WRITES=1|true|yes|on` enables real pushes.
    pub fn from_env(engine: SyncEngine) -> Self {
        Self::new(engine, writes_enabled_from_env())
    }

    pub fn writes_enabled(&self) -> bool {
        self.writes_enabled
    }

    pub fn store(&self) -> &Store {
        self.engine.store()
    }

    pub async fn sync(&mut self) -> Result<crate::sync::SyncReport> {
        self.engine.sync(false).await
    }

    // --------------------------------------------------------------- creates

    pub async fn create_task(&mut self, draft: TaskDraft, dry_run: bool) -> Result<WriteOutcome> {
        let payload =
            encode::task_create_payload(&draft, fmt::today_epoch_day(), fmt::now_seconds());
        encode::validate_task_create(&payload)?;

        let id = ids::new_id();
        let kind = encode::TASK_KIND;
        if dry_run {
            return Ok(outcome("dry-run", id, kind, payload, false, None));
        }
        self.ensure_writable()?;
        self.engine.sync(false).await?;

        let head = self
            .push(encode::envelope(&id, ACTION_CREATE, kind, payload.clone()))
            .await?;
        self.apply_local(&id, ACTION_CREATE, kind, &payload)?;
        Ok(outcome("created", id, kind, payload, true, Some(head)))
    }

    pub async fn create_area(&mut self, title: &str, dry_run: bool) -> Result<WriteOutcome> {
        let payload = encode::area_create_payload(title, 0);
        encode::validate_area_create(&payload)?;

        let id = ids::new_id();
        let kind = encode::AREA_KIND;
        if dry_run {
            return Ok(outcome("dry-run", id, kind, payload, false, None));
        }
        self.ensure_writable()?;
        self.engine.sync(false).await?;

        let head = self
            .push(encode::envelope(&id, ACTION_CREATE, kind, payload.clone()))
            .await?;
        self.apply_local(&id, ACTION_CREATE, kind, &payload)?;
        Ok(outcome("created", id, kind, payload, true, Some(head)))
    }

    pub async fn create_tag(
        &mut self,
        title: &str,
        parent: Option<&str>,
        shortcut: Option<&str>,
        dry_run: bool,
    ) -> Result<WriteOutcome> {
        let payload = encode::tag_create_payload(title, 0, parent, shortcut);
        encode::validate_tag_create(&payload)?;

        let id = ids::new_id();
        let kind = encode::TAG_KIND;
        if dry_run {
            return Ok(outcome("dry-run", id, kind, payload, false, None));
        }
        self.ensure_writable()?;
        self.engine.sync(false).await?;

        let head = self
            .push(encode::envelope(&id, ACTION_CREATE, kind, payload.clone()))
            .await?;
        self.apply_local(&id, ACTION_CREATE, kind, &payload)?;
        Ok(outcome("created", id, kind, payload, true, Some(head)))
    }

    pub async fn add_checklist_item(
        &mut self,
        task_id: &str,
        title: &str,
        dry_run: bool,
    ) -> Result<WriteOutcome> {
        self.expect_task(task_id)?;
        let payload = encode::checklist_create_payload(task_id, title, 0, fmt::now_seconds());
        encode::validate_checklist_create(&payload)?;

        let id = ids::new_id();
        let kind = encode::CHECKLIST_KIND;
        if dry_run {
            return Ok(outcome("dry-run", id, kind, payload, false, None));
        }
        self.ensure_writable()?;
        self.engine.sync(false).await?;

        let head = self
            .push(encode::envelope(&id, ACTION_CREATE, kind, payload.clone()))
            .await?;
        self.apply_local(&id, ACTION_CREATE, kind, &payload)?;
        Ok(outcome("created", id, kind, payload, true, Some(head)))
    }

    // --------------------------------------------------------------- updates

    pub async fn update_task(
        &mut self,
        id: &str,
        patch: TaskPatch,
        dry_run: bool,
    ) -> Result<WriteOutcome> {
        if patch.is_empty() {
            bail!("nothing to update: provide at least one field");
        }
        let existing = self.expect_task(id)?;
        let today = fmt::today_epoch_day();
        let now = fmt::now_seconds();
        let mut delta = Map::new();

        if let Some(title) = patch.title {
            if title != existing.title {
                delta.insert("tt".into(), json!(title));
            }
        }
        if let Some(notes) = patch.notes {
            if notes != existing.notes {
                delta.insert("nt".into(), encode::notes_object(&notes));
            }
        }
        if let Some(when) = patch.when {
            let (st, sr, tir, sb) = when.fields(today);
            delta.insert("st".into(), json!(st));
            delta.insert("sr".into(), json!(sr.map(fmt::day_to_wire)));
            delta.insert("tir".into(), json!(tir.map(fmt::day_to_wire)));
            delta.insert("sb".into(), json!(sb));
        }
        if let Some(deadline) = patch.deadline {
            delta.insert("dd".into(), json!(deadline.map(fmt::day_to_wire)));
        }
        if let Some(project) = patch.project {
            delta.insert(
                "pr".into(),
                json!(project.map(|id| vec![id]).unwrap_or_default()),
            );
        }
        if let Some(area) = patch.area {
            delta.insert(
                "ar".into(),
                json!(area.map(|id| vec![id]).unwrap_or_default()),
            );
        }
        if let Some(heading) = patch.heading {
            delta.insert(
                "agr".into(),
                json!(heading.map(|id| vec![id]).unwrap_or_default()),
            );
        }
        if let Some(tags) = patch.tags {
            delta.insert("tg".into(), json!(tags));
        }

        if delta.is_empty() {
            bail!("nothing changed");
        }
        delta.insert("md".into(), json!(now));

        self.push_update(
            id,
            Value::Object(delta),
            dry_run,
            "updated",
            encode::TASK_KIND,
        )
        .await
    }

    pub async fn complete_task(&mut self, id: &str, dry_run: bool) -> Result<WriteOutcome> {
        self.expect_task(id)?;
        let delta = encode::complete_update(fmt::now_seconds());
        self.push_update(id, delta, dry_run, "completed", encode::TASK_KIND)
            .await
    }

    pub async fn cancel_task(&mut self, id: &str, dry_run: bool) -> Result<WriteOutcome> {
        self.expect_task(id)?;
        let delta = encode::cancel_update(fmt::now_seconds());
        self.push_update(id, delta, dry_run, "canceled", encode::TASK_KIND)
            .await
    }

    pub async fn reopen_task(&mut self, id: &str, dry_run: bool) -> Result<WriteOutcome> {
        self.expect_task(id)?;
        let delta = encode::reopen_update(fmt::now_seconds());
        self.push_update(id, delta, dry_run, "reopened", encode::TASK_KIND)
            .await
    }

    pub async fn trash_task(
        &mut self,
        id: &str,
        restore: bool,
        dry_run: bool,
    ) -> Result<WriteOutcome> {
        self.expect_task(id)?;
        let delta = encode::trash_update(!restore, fmt::now_seconds());
        let status = if restore { "restored" } else { "trashed" };
        self.push_update(id, delta, dry_run, status, encode::TASK_KIND)
            .await
    }

    pub async fn delete_task(&mut self, id: &str, dry_run: bool) -> Result<WriteOutcome> {
        self.expect_task(id)?;
        let kind = encode::TASK_KIND;
        let empty = json!({});
        if dry_run {
            return Ok(outcome(
                "dry-run",
                id.to_string(),
                kind,
                json!({}),
                false,
                None,
            ));
        }
        self.ensure_writable()?;
        self.engine.sync(false).await?;

        let head = self.push(encode::delete_envelope(id, kind)).await?;
        self.apply_local(id, ACTION_DELETE, kind, &empty)?;
        Ok(outcome(
            "deleted",
            id.to_string(),
            kind,
            json!({}),
            true,
            Some(head),
        ))
    }

    // --------------------------------------------------------------- helpers

    async fn push_update(
        &mut self,
        id: &str,
        payload: Value,
        dry_run: bool,
        status: &str,
        kind: &'static str,
    ) -> Result<WriteOutcome> {
        if dry_run {
            return Ok(outcome(
                "dry-run",
                id.to_string(),
                kind,
                payload,
                false,
                None,
            ));
        }
        self.ensure_writable()?;
        self.engine.sync(false).await?;

        let map = encode::envelope(id, ACTION_UPDATE, kind, payload.clone());
        let head = self.push(map).await?;
        self.apply_local(id, ACTION_UPDATE, kind, &payload)?;
        Ok(outcome(
            status,
            id.to_string(),
            kind,
            payload,
            true,
            Some(head),
        ))
    }

    async fn push(&mut self, change_map: Value) -> Result<i64> {
        let history_key = self
            .engine
            .store()
            .history_key()?
            .ok_or_else(|| anyhow!("no history key yet; run `things-mcp sync` first"))?;

        let ancestor = self.ancestor()?;
        match self
            .engine
            .client()
            .commit(&history_key, ancestor, &change_map)
            .await
        {
            Ok(result) => {
                self.engine
                    .store()
                    .set_commit_head(result.server_head_index)?;
                Ok(result.server_head_index)
            }
            Err(ThingsError::Conflict(reason)) => {
                tracing::warn!(%reason, "commit conflict; pulling and retrying once");
                self.engine.sync(false).await?;
                let history_key = self
                    .engine
                    .store()
                    .history_key()?
                    .ok_or_else(|| anyhow!("history key disappeared during retry"))?;
                let ancestor = self.engine.store().cursor()?;
                let result = self
                    .engine
                    .client()
                    .commit(&history_key, ancestor, &change_map)
                    .await?;
                self.engine
                    .store()
                    .set_commit_head(result.server_head_index)?;
                Ok(result.server_head_index)
            }
            Err(error) => Err(error.into()),
        }
    }

    fn ancestor(&self) -> Result<i64> {
        let cursor = self.engine.store().cursor()?;
        let commit_head = self.engine.store().commit_head()?.unwrap_or(0);
        Ok(cursor.max(commit_head))
    }

    fn apply_local(&mut self, id: &str, action: i64, kind: &str, payload: &Value) -> Result<()> {
        let props = payload.as_object().cloned().unwrap_or_default();
        let item = IncomingItem {
            uuid: id.to_string(),
            action,
            kind: kind.to_string(),
            props,
            server_index: 0,
        };
        self.engine.store_mut().apply_items(&[item])?;
        Ok(())
    }

    fn expect_task(&self, id: &str) -> Result<crate::things::Task> {
        self.engine
            .store()
            .get_task(id)?
            .ok_or_else(|| anyhow!("task {id} not found locally; run `things-mcp sync` first"))
    }

    fn ensure_writable(&self) -> Result<()> {
        if !self.writes_enabled {
            bail!(
                "writes are disabled: set THINGS_ENABLE_WRITES=1 to push changes (dry_run is always allowed)"
            );
        }
        Ok(())
    }
}

fn outcome(
    status: &str,
    id: String,
    kind: &'static str,
    payload: Value,
    pushed: bool,
    server_head_index: Option<i64>,
) -> WriteOutcome {
    WriteOutcome {
        status: status.to_string(),
        id,
        kind: kind.to_string(),
        payload,
        pushed,
        server_head_index,
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use serde_json::json;

    use super::*;
    use crate::store::Store;
    use crate::testing::FakeTransport;
    use crate::things::{ClientOptions, RawReply, ThingsClient};

    fn client(fake: Arc<FakeTransport>) -> ThingsClient {
        let mut options = ClientOptions::new("me@example.com", "pw", "app");
        options.base_url = "https://cloud.example".to_string();
        options.max_retries = 0;
        ThingsClient::new(fake, options)
    }

    fn engine(fake: Arc<FakeTransport>, seed: Option<Value>) -> SyncEngine {
        let mut store = Store::open_in_memory().unwrap();
        store.set_history_key("HK").unwrap();
        if let Some(props) = seed {
            store
                .apply_items(&[IncomingItem {
                    uuid: "task-1".into(),
                    action: ACTION_CREATE,
                    kind: encode::TASK_KIND.into(),
                    props: props.as_object().unwrap().clone(),
                    server_index: 0,
                }])
                .unwrap();
        }
        SyncEngine::new(client(fake), store)
    }

    fn fake_ok(head: i64) -> Arc<FakeTransport> {
        FakeTransport::new(move |request| {
            if request.url.contains("/commit") {
                return RawReply::json(200, format!(r#"{{"server-head-index":{head}}}"#));
            }
            if request.url.contains("/history/HK") {
                return RawReply::json(
                    200,
                    r#"{"latest-server-index":0,"latest-schema-version":301,"is-empty":false}"#,
                );
            }
            RawReply::json(404, "{}")
        })
    }

    #[tokio::test]
    async fn create_validates_pushes_then_applies_locally() {
        let fake = fake_ok(1);
        let mut service = WriteService::new(engine(fake.clone(), None), true);

        let outcome = service
            .create_task(TaskDraft::new("buy milk"), false)
            .await
            .unwrap();

        assert_eq!(outcome.status, "created");
        assert!(outcome.pushed);
        let task = service.store().get_task(&outcome.id).unwrap().unwrap();
        assert_eq!(task.title, "buy milk");

        let commit = fake
            .requests()
            .into_iter()
            .find(|request| request.url.contains("/commit"))
            .expect("commit request");
        assert!(
            commit.url.contains("ancestor-index=0&_cnt=1"),
            "{}",
            commit.url
        );

        let body: Value = serde_json::from_slice(commit.body.as_ref().unwrap()).unwrap();
        let item = body.get(outcome.id.as_str()).expect("change map entry");
        assert_eq!(item["t"], 0);
        assert_eq!(item["e"], "Task7");
        assert_eq!(item["p"].as_object().unwrap().len(), 34);

        // The pull cursor must not jump over unseen remote items.
        assert_eq!(service.store().cursor().unwrap(), 0);
        assert_eq!(service.store().commit_head().unwrap(), Some(1));
    }

    #[tokio::test]
    async fn dry_run_touches_neither_the_network_nor_the_database() {
        let fake = fake_ok(1);
        let mut service = WriteService::new(engine(fake.clone(), None), false);

        let outcome = service
            .create_task(TaskDraft::new("preview"), true)
            .await
            .unwrap();

        assert_eq!(outcome.status, "dry-run");
        assert!(!outcome.pushed);
        assert_eq!(outcome.payload.as_object().unwrap().len(), 34);
        assert!(fake.requests().is_empty());
        assert!(service.store().get_task(&outcome.id).unwrap().is_none());
    }

    #[tokio::test]
    async fn writes_are_refused_without_the_env_flag() {
        let fake = fake_ok(1);
        let mut service = WriteService::new(engine(fake.clone(), None), false);

        let error = service
            .create_task(TaskDraft::new("nope"), false)
            .await
            .unwrap_err();
        assert!(
            error.to_string().contains("THINGS_ENABLE_WRITES"),
            "{error}"
        );
        assert!(fake.requests().is_empty());
    }

    #[tokio::test]
    async fn update_sends_only_the_changed_fields() {
        let fake = fake_ok(1);
        let mut service = WriteService::new(
            engine(
                fake.clone(),
                Some(json!({"tt": "old", "st": 0, "ss": 0, "tp": 0})),
            ),
            true,
        );

        let patch = TaskPatch {
            title: Some("new".into()),
            ..Default::default()
        };
        service.update_task("task-1", patch, false).await.unwrap();

        let commit = fake
            .requests()
            .into_iter()
            .find(|request| request.url.contains("/commit"))
            .unwrap();
        let body: Value = serde_json::from_slice(commit.body.as_ref().unwrap()).unwrap();
        let payload = &body["task-1"]["p"];
        assert_eq!(payload["tt"], "new");
        assert!(payload["md"].is_number());
        assert_eq!(
            payload.as_object().unwrap().len(),
            2,
            "update must stay minimal: {payload}"
        );
        assert_eq!(
            service.store().get_task("task-1").unwrap().unwrap().title,
            "new"
        );
    }

    #[tokio::test]
    async fn completion_sets_status_sp_st_and_md() {
        let fake = fake_ok(1);
        let mut service = WriteService::new(
            engine(
                fake.clone(),
                Some(json!({"tt": "t", "st": 0, "ss": 0, "tp": 0})),
            ),
            true,
        );

        service.complete_task("task-1", false).await.unwrap();

        let commit = fake
            .requests()
            .into_iter()
            .find(|request| request.url.contains("/commit"))
            .unwrap();
        let body: Value = serde_json::from_slice(commit.body.as_ref().unwrap()).unwrap();
        let payload = &body["task-1"]["p"];
        assert_eq!(payload["ss"], 3);
        assert_eq!(payload["st"], 1);
        assert!(payload["sp"].is_number());
        assert!(payload["md"].is_number());

        let task = service.store().get_task("task-1").unwrap().unwrap();
        assert_eq!(task.status, 3);
        assert_eq!(task.start, 1);
    }

    #[tokio::test]
    async fn delete_uses_a_tombstone_envelope_and_removes_the_row() {
        let fake = fake_ok(1);
        let mut service = WriteService::new(
            engine(
                fake.clone(),
                Some(json!({"tt": "gone", "st": 0, "ss": 0, "tp": 0})),
            ),
            true,
        );

        service.delete_task("task-1", false).await.unwrap();

        let commit = fake
            .requests()
            .into_iter()
            .find(|request| request.url.contains("/commit"))
            .unwrap();
        let body: Value = serde_json::from_slice(commit.body.as_ref().unwrap()).unwrap();
        assert_eq!(body["task-1"]["t"], 2);
        assert_eq!(body["task-1"]["e"], "Task7");
        assert_eq!(body["task-1"]["p"], json!({}));
        assert!(service.store().get_task("task-1").unwrap().is_none());
    }

    #[tokio::test]
    async fn retries_once_after_an_ancestor_conflict() {
        let commits = Arc::new(AtomicUsize::new(0));
        let counter = commits.clone();

        let fake = FakeTransport::new(move |request| {
            if request.url.contains("/commit") {
                let attempt = counter.fetch_add(1, Ordering::SeqCst);
                if attempt == 0 {
                    return RawReply::json(409, r#"{"error":"OutdatedAncestor"}"#);
                }
                return RawReply::json(200, r#"{"server-head-index":5}"#);
            }
            if request.url.contains("/history/HK") {
                return RawReply::json(
                    200,
                    r#"{"latest-server-index":0,"latest-schema-version":301,"is-empty":false}"#,
                );
            }
            RawReply::json(404, "{}")
        });

        let mut service = WriteService::new(engine(fake, None), true);
        let outcome = service
            .create_task(TaskDraft::new("retry me"), false)
            .await
            .unwrap();

        assert!(outcome.pushed);
        assert_eq!(commits.load(Ordering::SeqCst), 2);
        assert!(service.store().get_task(&outcome.id).unwrap().is_some());
    }
}
