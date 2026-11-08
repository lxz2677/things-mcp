//! Prepare write payloads for offline preview or sync and commit them to Things Cloud.

use std::path::{Path, PathBuf};

use anyhow::{Result, anyhow, bail};
use serde::Serialize;
use serde_json::{Map, Value, json};

use crate::fmt;
use crate::ids;
use crate::store::Store;
use crate::sync::SyncEngine;
use crate::things::encode::{self, TaskDraft, When};
use crate::things::{
    ACTION_CREATE, ACTION_DELETE, ACTION_UPDATE, IncomingItem, TYPE_HEADING, TYPE_PROJECT,
    ThingsError,
};

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

pub enum WriteService {
    Preview(PathBuf),
    Live(Box<SyncEngine>),
}

impl WriteService {
    pub fn live(engine: SyncEngine) -> Self {
        Self::Live(Box::new(engine))
    }

    pub async fn create_task(&mut self, draft: TaskDraft) -> Result<WriteOutcome> {
        self.execute(
            &ids::new_id(),
            ACTION_CREATE,
            encode::TASK_KIND,
            "created",
            |replica| {
                validate_container(
                    replica,
                    draft.project.as_deref(),
                    draft.area.as_deref(),
                    draft.heading.as_deref(),
                )?;
                let payload =
                    encode::task_create_payload(&draft, fmt::today_epoch_day(), fmt::now_seconds());
                encode::validate_task_create(&payload)?;
                Ok(payload)
            },
        )
        .await
    }

    pub async fn create_area(&mut self, title: &str) -> Result<WriteOutcome> {
        self.execute(
            &ids::new_id(),
            ACTION_CREATE,
            encode::AREA_KIND,
            "created",
            |_| {
                let payload = encode::area_create_payload(title, 0);
                encode::validate_area_create(&payload)?;
                Ok(payload)
            },
        )
        .await
    }

    pub async fn create_tag(
        &mut self,
        title: &str,
        parent: Option<&str>,
        shortcut: Option<&str>,
    ) -> Result<WriteOutcome> {
        self.execute(
            &ids::new_id(),
            ACTION_CREATE,
            encode::TAG_KIND,
            "created",
            |replica| {
                if let Some(parent) = parent
                    && replica.query(|store| store.get_tag(parent))?.is_none()
                {
                    bail!("parent tag {parent} not found locally");
                }
                let payload = encode::tag_create_payload(title, 0, parent, shortcut);
                encode::validate_tag_create(&payload)?;
                Ok(payload)
            },
        )
        .await
    }

    pub async fn add_checklist_item(&mut self, task_id: &str, title: &str) -> Result<WriteOutcome> {
        self.execute(
            &ids::new_id(),
            ACTION_CREATE,
            encode::CHECKLIST_KIND,
            "created",
            |replica| {
                expect_task(replica, task_id)?;
                let payload =
                    encode::checklist_create_payload(task_id, title, 0, fmt::now_seconds());
                encode::validate_checklist_create(&payload)?;
                Ok(payload)
            },
        )
        .await
    }

    pub async fn update_task(&mut self, id: &str, patch: TaskPatch) -> Result<WriteOutcome> {
        if patch.is_empty() {
            bail!("nothing to update: provide at least one field");
        }
        self.execute(id, ACTION_UPDATE, encode::TASK_KIND, "updated", |replica| {
            let existing = expect_task(replica, id)?;
            validate_container(
                replica,
                patch.project.as_ref().and_then(|id| id.as_deref()),
                patch.area.as_ref().and_then(|id| id.as_deref()),
                patch.heading.as_ref().and_then(|id| id.as_deref()),
            )?;
            let mut delta = Map::new();

            if let Some(title) = &patch.title
                && *title != existing.title
            {
                delta.insert("tt".into(), json!(title));
            }
            if let Some(notes) = &patch.notes
                && *notes != existing.notes
            {
                delta.insert("nt".into(), encode::notes_object(notes));
            }
            if let Some(when) = &patch.when {
                let (st, sr, tir, sb) = when.fields(fmt::today_epoch_day());
                delta.insert("st".into(), json!(st));
                delta.insert("sr".into(), json!(sr.map(fmt::day_to_wire)));
                delta.insert("tir".into(), json!(tir.map(fmt::day_to_wire)));
                delta.insert("sb".into(), json!(sb));
            }
            if let Some(deadline) = patch.deadline {
                delta.insert("dd".into(), json!(deadline.map(fmt::day_to_wire)));
            }
            if let Some(project) = &patch.project {
                delta.insert("pr".into(), json!(project.as_slice()));
            }
            if let Some(area) = &patch.area {
                delta.insert("ar".into(), json!(area.as_slice()));
            }
            if let Some(heading) = &patch.heading {
                delta.insert("agr".into(), json!(heading.as_slice()));
            }
            if let Some(tags) = &patch.tags {
                delta.insert("tg".into(), json!(tags));
            }

            if delta.is_empty() {
                bail!("nothing changed");
            }
            delta.insert("md".into(), json!(fmt::now_seconds()));
            Ok(Value::Object(delta))
        })
        .await
    }

    pub async fn complete_task(&mut self, id: &str) -> Result<WriteOutcome> {
        self.change_task(id, ACTION_UPDATE, "completed", || {
            encode::complete_update(fmt::now_seconds())
        })
        .await
    }

    pub async fn cancel_task(&mut self, id: &str) -> Result<WriteOutcome> {
        self.change_task(id, ACTION_UPDATE, "canceled", || {
            encode::cancel_update(fmt::now_seconds())
        })
        .await
    }

    pub async fn reopen_task(&mut self, id: &str) -> Result<WriteOutcome> {
        self.change_task(id, ACTION_UPDATE, "reopened", || {
            encode::reopen_update(fmt::now_seconds())
        })
        .await
    }

    pub async fn trash_task(&mut self, id: &str, restore: bool) -> Result<WriteOutcome> {
        let status = if restore { "restored" } else { "trashed" };
        self.change_task(id, ACTION_UPDATE, status, || {
            encode::trash_update(!restore, fmt::now_seconds())
        })
        .await
    }

    pub async fn delete_task(&mut self, id: &str) -> Result<WriteOutcome> {
        self.change_task(id, ACTION_DELETE, "deleted", || json!({}))
            .await
    }

    async fn change_task(
        &mut self,
        id: &str,
        action: i64,
        status: &str,
        prepare: impl Fn() -> Value,
    ) -> Result<WriteOutcome> {
        self.execute(id, action, encode::TASK_KIND, status, |replica| {
            expect_task(replica, id)?;
            Ok(prepare())
        })
        .await
    }

    async fn execute(
        &mut self,
        id: &str,
        action: i64,
        kind: &str,
        status: &str,
        prepare: impl Fn(&Replica<'_>) -> Result<Value>,
    ) -> Result<WriteOutcome> {
        let engine = match self {
            Self::Live(engine) => engine,
            Self::Preview(path) => {
                return Ok(WriteOutcome {
                    status: "dry-run".into(),
                    id: id.into(),
                    kind: kind.into(),
                    payload: prepare(&Replica::OnDisk(path))?,
                    pushed: false,
                    server_head_index: None,
                });
            }
        };

        let mut report = engine.sync(false).await?;
        let mut payload = prepare(&Replica::Synced(engine.store()))?;
        let change = encode::envelope(id, action, kind, payload.clone());
        let result = match engine
            .client()
            .commit(&report.history_key, report.cursor, &change)
            .await
        {
            Err(ThingsError::Conflict(reason)) => {
                tracing::warn!(%reason, "commit conflict; pulling and retrying once");
                let history_key = report.history_key;
                report = engine.sync(false).await?;
                if report.history_replaced || report.history_key != history_key {
                    bail!("cloud history changed during the write; retry the operation");
                }
                payload = prepare(&Replica::Synced(engine.store()))?;
                let change = encode::envelope(id, action, kind, payload.clone());
                engine
                    .client()
                    .commit(&report.history_key, report.cursor, &change)
                    .await?
            }
            result => result?,
        };

        engine.store_mut().apply_commit(
            &IncomingItem {
                uuid: id.into(),
                action,
                kind: kind.into(),
                props: payload.as_object().cloned().unwrap_or_default(),
                server_index: 0,
            },
            &report.history_key,
            report.generation,
            result.server_head_index,
        )?;

        Ok(WriteOutcome {
            status: status.into(),
            id: id.into(),
            kind: kind.into(),
            payload,
            pushed: true,
            server_head_index: Some(result.server_head_index),
        })
    }
}

enum Replica<'a> {
    OnDisk(&'a Path),
    Synced(&'a Store),
}

impl Replica<'_> {
    fn query<T>(&self, read: impl FnOnce(&Store) -> Result<Option<T>>) -> Result<Option<T>> {
        match self {
            Self::OnDisk(path) => match Store::open_read_only(path)? {
                Some(store) => read(&store),
                None => Ok(None),
            },
            Self::Synced(store) => read(store),
        }
    }
}

fn validate_container(
    replica: &Replica<'_>,
    project: Option<&str>,
    area: Option<&str>,
    heading: Option<&str>,
) -> Result<()> {
    if let Some(id) = project {
        let task = replica.query(|store| store.get_task(id))?.ok_or_else(|| {
            anyhow!("project {id} not found locally; run `things-mcp sync` first")
        })?;
        if task.type_ != TYPE_PROJECT {
            bail!("{id} is not a project (type {})", task.type_);
        }
    }
    if let Some(id) = area
        && replica.query(|store| store.get_area(id))?.is_none()
    {
        bail!("area {id} not found locally; run `things-mcp sync` first");
    }
    if let Some(id) = heading {
        let task = replica
            .query(|store| store.get_task(id))?
            .ok_or_else(|| anyhow!("heading {id} not found locally"))?;
        if task.type_ != TYPE_HEADING {
            bail!("{id} is not a heading (type {})", task.type_);
        }
    }
    Ok(())
}

fn expect_task(replica: &Replica<'_>, id: &str) -> Result<crate::things::Task> {
    replica
        .query(|store| store.get_task(id))?
        .ok_or_else(|| anyhow!("task {id} not found locally; run `things-mcp sync` first"))
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

    fn live_store(service: &WriteService) -> &Store {
        let WriteService::Live(engine) = service else {
            panic!("expected a live service");
        };
        engine.store()
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
        let mut service = WriteService::live(engine(fake.clone(), None));

        let outcome = service
            .create_task(TaskDraft::new("buy milk"))
            .await
            .unwrap();

        assert_eq!(outcome.status, "created");
        assert!(outcome.pushed);
        let task = live_store(&service).get_task(&outcome.id).unwrap().unwrap();
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
        assert_eq!(live_store(&service).cursor().unwrap(), 0);
        assert_eq!(live_store(&service).commit_head().unwrap(), Some(1));
    }

    #[tokio::test]
    async fn dry_run_does_not_create_the_database_or_its_directory() {
        let directory = tempfile::tempdir().unwrap();
        let parent = directory.path().join("absent");
        let mut service = WriteService::Preview(parent.join("preview.sqlite"));

        let outcome = service
            .create_task(TaskDraft::new("preview"))
            .await
            .unwrap();

        assert_eq!(outcome.status, "dry-run");
        assert!(!outcome.pushed);
        assert_eq!(outcome.payload.as_object().unwrap().len(), 34);
        assert!(!parent.exists());
    }

    #[tokio::test]
    async fn update_sends_only_the_changed_fields() {
        let fake = fake_ok(1);
        let mut service = WriteService::live(engine(
            fake.clone(),
            Some(json!({"tt": "old", "st": 0, "ss": 0, "tp": 0})),
        ));

        let patch = TaskPatch {
            title: Some("new".into()),
            ..Default::default()
        };
        service.update_task("task-1", patch).await.unwrap();

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
            live_store(&service)
                .get_task("task-1")
                .unwrap()
                .unwrap()
                .title,
            "new"
        );
    }

    #[tokio::test]
    async fn completion_sets_status_sp_st_and_md() {
        let fake = fake_ok(1);
        let mut service = WriteService::live(engine(
            fake.clone(),
            Some(json!({"tt": "t", "st": 0, "ss": 0, "tp": 0})),
        ));

        service.complete_task("task-1").await.unwrap();

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

        let task = live_store(&service).get_task("task-1").unwrap().unwrap();
        assert_eq!(task.status, 3);
        assert_eq!(task.start, 1);
    }

    #[tokio::test]
    async fn delete_uses_a_tombstone_envelope_and_removes_the_row() {
        let fake = fake_ok(1);
        let mut service = WriteService::live(engine(
            fake.clone(),
            Some(json!({"tt": "gone", "st": 0, "ss": 0, "tp": 0})),
        ));

        service.delete_task("task-1").await.unwrap();

        let commit = fake
            .requests()
            .into_iter()
            .find(|request| request.url.contains("/commit"))
            .unwrap();
        let body: Value = serde_json::from_slice(commit.body.as_ref().unwrap()).unwrap();
        assert_eq!(body["task-1"]["t"], 2);
        assert_eq!(body["task-1"]["e"], "Task7");
        assert_eq!(body["task-1"]["p"], json!({}));
        assert!(live_store(&service).get_task("task-1").unwrap().is_none());
    }

    #[tokio::test]
    async fn refuses_a_create_with_an_unknown_project() {
        let directory = tempfile::tempdir().unwrap();
        let mut service = WriteService::Preview(directory.path().join("absent.sqlite"));
        let mut draft = TaskDraft::new("orphan");
        draft.project = Some("missing-project".into());

        let error = service.create_task(draft).await.unwrap_err();
        assert!(error.to_string().contains("not found"), "{error}");
    }

    #[tokio::test]
    async fn accepts_a_create_in_a_known_project() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("preview.sqlite");
        let mut store = Store::open(&path).unwrap();
        store
            .apply_items(&[IncomingItem {
                uuid: "proj".into(),
                action: ACTION_CREATE,
                kind: encode::TASK_KIND.into(),
                props: json!({"tt": "Proj", "tp": 1, "st": 1, "ss": 0})
                    .as_object()
                    .unwrap()
                    .clone(),
                server_index: 0,
            }])
            .unwrap();

        drop(store);
        let mut service = WriteService::Preview(path.clone());
        let mut draft = TaskDraft::new("child");
        draft.project = Some("proj".into());

        let outcome = service.create_task(draft).await.unwrap();
        assert_eq!(outcome.status, "dry-run");
        assert_eq!(outcome.payload["pr"], json!(["proj"]));
        assert_eq!(
            Store::open_read_only(&path)
                .unwrap()
                .unwrap()
                .task_count()
                .unwrap(),
            1
        );
    }

    #[tokio::test]
    async fn validates_container_after_the_initial_pull_and_after_a_conflict() {
        for conflict in [false, true] {
            let commits = Arc::new(AtomicUsize::new(0));
            let counter = commits.clone();
            let fake = FakeTransport::new(move |request| {
                if request.url.contains("/commit") {
                    counter.fetch_add(1, Ordering::SeqCst);
                    return RawReply::json(409, r#"{"error":"OutdatedAncestor"}"#);
                }
                if request.url.contains("/items") {
                    return RawReply::json(
                        200,
                        r#"{"current-item-index":1,"items":[{"task-1":{"t":2,"e":"Task7","p":{}}}]}"#,
                    );
                }
                let head = i64::from(!conflict || counter.load(Ordering::SeqCst) > 0);
                RawReply::json(200, json!({"latest-server-index":head}).to_string())
            });
            let mut service =
                WriteService::live(engine(fake, Some(json!({"tt":"Project","tp":1}))));
            let mut draft = TaskDraft::new("child");
            draft.project = Some("task-1".into());

            let error = service.create_task(draft).await.unwrap_err();

            assert!(
                error.to_string().contains("project task-1 not found"),
                "{error}"
            );
            assert_eq!(commits.load(Ordering::SeqCst), usize::from(conflict));
        }
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

        let mut service = WriteService::live(engine(fake, None));
        let outcome = service
            .create_task(TaskDraft::new("retry me"))
            .await
            .unwrap();

        assert!(outcome.pushed);
        assert_eq!(commits.load(Ordering::SeqCst), 2);
        assert!(
            live_store(&service)
                .get_task(&outcome.id)
                .unwrap()
                .is_some()
        );
    }
}
