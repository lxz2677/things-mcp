//! SQLite persistence for the local Things replica.
//!
//! Design notes:
//!
//! * every entity table has an explicit set of queryable columns plus a `raw`
//!   JSON blob holding every wire property seen so far;
//! * `apply_page` commits the entities **and** the sync cursor in one
//!   transaction, and only if nobody else advanced the cursor meanwhile, so an
//!   MCP server and a CLI can sync concurrently without double-applying
//!   non-idempotent updates such as note patches.

pub mod audit;
pub mod diagnose;
mod query;

pub use query::{
    AreaDetail, AreaSummary, ChecklistSummary, HeadingSummary, ListKind, ProjectDetail,
    ProjectSummary, SearchFilter, TagDetail, TagRef, TagSummary, TaskDetail, TaskSummary,
    area_detail, count_tasks, list_areas, list_projects, list_tags, list_tasks, project_detail,
    search_tasks, summaries, tag_detail, task_counts, task_detail,
};

use std::path::Path;
use std::time::Duration;

use anyhow::{Context, Result};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use serde_json::Value;

use crate::things::model::{Area, ChecklistItem, EntityKind, Tag, Task};
use crate::things::wire;
use crate::things::{ACTION_CREATE, ACTION_DELETE, ACTION_UPDATE, IncomingItem};

/// Bump when adding tables/columns; `init` applies migrations for older files.
const SCHEMA_VERSION: i64 = 1;

pub struct Store {
    conn: Connection,
}

#[derive(Debug, Default, Clone, Copy)]
pub struct ApplyStats {
    pub upserted: u64,
    pub deleted: u64,
    pub skipped: u64,
    pub unknown: u64,
}

impl Store {
    pub fn open(path: &Path) -> Result<Self> {
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent)
                    .with_context(|| format!("creating {}", parent.display()))?;
            }
        }
        let conn = Connection::open(path)
            .with_context(|| format!("opening database {}", path.display()))?;
        Self::init(conn)
    }

    pub fn open_in_memory() -> Result<Self> {
        Self::init(Connection::open_in_memory()?)
    }

    fn init(conn: Connection) -> Result<Self> {
        conn.busy_timeout(Duration::from_secs(10))?;
        // WAL lets a long-running MCP server and ad-hoc CLI runs share the file.
        // journal_mode returns a row, so it has to go through execute_batch.
        conn.execute_batch(
            "PRAGMA journal_mode=WAL;
             PRAGMA synchronous=NORMAL;
             PRAGMA temp_store=MEMORY;
             PRAGMA foreign_keys=OFF;",
        )?;
        let existing: i64 = conn.pragma_query_value(None, "user_version", |row| row.get(0))?;
        if existing > SCHEMA_VERSION {
            anyhow::bail!(
                "database schema version {existing} is newer than supported ({SCHEMA_VERSION}); update things-mcp"
            );
        }

        conn.execute_batch(include_str!("schema.sql"))?;

        if existing < SCHEMA_VERSION {
            // Baseline: schema.sql creates every v1 table.
            conn.pragma_update(None, "user_version", SCHEMA_VERSION)?;
        }
        Ok(Self { conn })
    }

    pub fn connection(&self) -> &Connection {
        &self.conn
    }

    // ----------------------------------------------------------------- meta

    pub fn meta_get(&self, key: &str) -> Result<Option<String>> {
        Ok(self
            .conn
            .query_row("SELECT value FROM meta WHERE key = ?1", [key], |row| {
                row.get(0)
            })
            .optional()?)
    }

    pub fn meta_set(&self, key: &str, value: &str) -> Result<()> {
        self.conn.execute(
            "INSERT INTO meta(key, value) VALUES(?1, ?2)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            params![key, value],
        )?;
        Ok(())
    }

    pub fn cursor(&self) -> Result<i64> {
        Ok(meta_i64(&self.conn, CURSOR_KEY)?.unwrap_or(0))
    }

    pub fn set_cursor(&self, value: i64) -> Result<()> {
        set_meta(&self.conn, CURSOR_KEY, &value.to_string())
    }

    pub fn history_key(&self) -> Result<Option<String>> {
        self.meta_get("history_key")
    }

    pub fn set_history_key(&self, value: &str) -> Result<()> {
        self.meta_set("history_key", value)
    }

    pub fn app_instance_id(&self) -> Result<Option<String>> {
        self.meta_get("app_instance_id")
    }

    pub fn set_app_instance_id(&self, value: &str) -> Result<()> {
        self.meta_set("app_instance_id", value)
    }

    /// Highest history index acknowledged by a commit (may be ahead of the
    /// pull cursor, because we do not pull our own writes immediately).
    pub fn commit_head(&self) -> Result<Option<i64>> {
        meta_i64(&self.conn, "commit_head")
    }

    pub fn set_commit_head(&self, value: i64) -> Result<()> {
        set_meta(&self.conn, "commit_head", &value.to_string())
    }

    pub fn latest_server_index(&self) -> Result<Option<i64>> {
        meta_i64(&self.conn, "latest_server_index")
    }

    pub fn set_latest_server_index(&self, value: i64) -> Result<()> {
        set_meta(&self.conn, "latest_server_index", &value.to_string())
    }

    pub fn schema_version(&self) -> Result<i64> {
        Ok(meta_i64(&self.conn, "schema")?.unwrap_or(0))
    }

    pub fn set_schema_version(&self, value: i64) -> Result<()> {
        set_meta(&self.conn, "schema", &value.to_string())
    }

    pub fn last_sync_at(&self) -> Result<Option<String>> {
        self.meta_get("last_sync_at")
    }

    pub fn set_last_sync_at(&self, value: &str) -> Result<()> {
        self.meta_set("last_sync_at", value)
    }

    // -------------------------------------------------------------- applying

    /// Apply items and advance the cursor atomically.
    ///
    /// Returns `Ok(None)` without writing anything when another process already
    /// moved the cursor (the caller should re-read it and retry).
    pub fn apply_page(
        &mut self,
        expected_cursor: i64,
        items: &[IncomingItem],
        next_cursor: i64,
    ) -> Result<Option<ApplyStats>> {
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let current = meta_i64(&tx, CURSOR_KEY)?.unwrap_or(0);
        if current != expected_cursor {
            // Dropping the transaction rolls back; nothing was written.
            return Ok(None);
        }
        let stats = apply_items(&tx, items)?;
        set_meta(&tx, CURSOR_KEY, &next_cursor.to_string())?;
        tx.commit()?;
        Ok(Some(stats))
    }

    /// Apply items without touching the cursor (tests, full imports).
    pub fn apply_items(&mut self, items: &[IncomingItem]) -> Result<ApplyStats> {
        let tx = self.conn.transaction()?;
        let stats = apply_items(&tx, items)?;
        tx.commit()?;
        Ok(stats)
    }

    /// Remove every cached entity and reset the cursor.
    pub fn reset(&mut self) -> Result<()> {
        let tx = self.conn.transaction()?;
        tx.execute_batch(
            "DELETE FROM task;
             DELETE FROM area;
             DELETE FROM tag;
             DELETE FROM checklist_item;
             DELETE FROM task_tag;
             DELETE FROM area_tag;",
        )?;
        set_meta(&tx, CURSOR_KEY, "0")?;
        tx.commit()?;
        Ok(())
    }

    // ------------------------------------------------------------- read-only

    pub fn task_count(&self) -> Result<i64> {
        Ok(self
            .conn
            .query_row("SELECT COUNT(*) FROM task", [], |row| row.get(0))?)
    }

    pub fn get_task(&self, id: &str) -> Result<Option<Task>> {
        let Some(mut task) = get_task(&self.conn, id)? else {
            return Ok(None);
        };
        load_task_tags(&self.conn, &mut task)?;
        Ok(Some(task))
    }

    pub fn get_area(&self, id: &str) -> Result<Option<Area>> {
        let Some(mut area) = get_area(&self.conn, id)? else {
            return Ok(None);
        };
        load_area_tags(&self.conn, &mut area)?;
        Ok(Some(area))
    }

    pub fn get_tag(&self, id: &str) -> Result<Option<Tag>> {
        get_tag(&self.conn, id)
    }

    pub fn checklist_for(&self, task_id: &str) -> Result<Vec<ChecklistItem>> {
        checklist_for(&self.conn, task_id)
    }
}

const CURSOR_KEY: &str = "processed_remote_index";

fn meta_i64(conn: &Connection, key: &str) -> Result<Option<i64>> {
    Ok(conn
        .query_row("SELECT value FROM meta WHERE key = ?1", [key], |row| {
            row.get::<_, String>(0)
        })
        .optional()?
        .and_then(|value| value.parse::<i64>().ok()))
}

fn set_meta(conn: &Connection, key: &str, value: &str) -> Result<()> {
    conn.execute(
        "INSERT INTO meta(key, value) VALUES(?1, ?2)
         ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        params![key, value],
    )?;
    Ok(())
}

fn apply_items(conn: &Connection, items: &[IncomingItem]) -> Result<ApplyStats> {
    let mut stats = ApplyStats::default();

    for item in items {
        match EntityKind::from_wire(&item.kind) {
            EntityKind::Task => match item.action {
                ACTION_DELETE => {
                    delete_task(conn, &item.uuid)?;
                    stats.deleted += 1;
                }
                ACTION_CREATE | ACTION_UPDATE => {
                    let mut task = get_task(conn, &item.uuid)?.unwrap_or_default();
                    task.id = item.uuid.clone();
                    task.kind = item.kind.clone();
                    load_task_tags(conn, &mut task)?;
                    wire::apply_task(&mut task, &item.props);
                    save_task(conn, &task)?;
                    replace_task_tags(conn, &task)?;
                    stats.upserted += 1;
                }
                other => {
                    tracing::warn!(uuid = %item.uuid, action = other, "unknown task action");
                    stats.skipped += 1;
                }
            },
            EntityKind::Area => match item.action {
                ACTION_DELETE => {
                    conn.execute("DELETE FROM area WHERE id = ?1", [&item.uuid])?;
                    conn.execute("DELETE FROM area_tag WHERE area = ?1", [&item.uuid])?;
                    stats.deleted += 1;
                }
                ACTION_CREATE | ACTION_UPDATE => {
                    let mut area = get_area(conn, &item.uuid)?.unwrap_or_default();
                    area.id = item.uuid.clone();
                    area.kind = item.kind.clone();
                    load_area_tags(conn, &mut area)?;
                    wire::apply_area(&mut area, &item.props);
                    save_area(conn, &area)?;
                    replace_area_tags(conn, &area)?;
                    stats.upserted += 1;
                }
                other => {
                    tracing::warn!(uuid = %item.uuid, action = other, "unknown area action");
                    stats.skipped += 1;
                }
            },
            EntityKind::Tag => match item.action {
                ACTION_DELETE => {
                    conn.execute("DELETE FROM tag WHERE id = ?1", [&item.uuid])?;
                    conn.execute("DELETE FROM task_tag WHERE tag = ?1", [&item.uuid])?;
                    conn.execute("DELETE FROM area_tag WHERE tag = ?1", [&item.uuid])?;
                    stats.deleted += 1;
                }
                ACTION_CREATE | ACTION_UPDATE => {
                    let mut tag = get_tag(conn, &item.uuid)?.unwrap_or_default();
                    tag.id = item.uuid.clone();
                    tag.kind = item.kind.clone();
                    wire::apply_tag(&mut tag, &item.props);
                    save_tag(conn, &tag)?;
                    stats.upserted += 1;
                }
                other => {
                    tracing::warn!(uuid = %item.uuid, action = other, "unknown tag action");
                    stats.skipped += 1;
                }
            },
            EntityKind::ChecklistItem => match item.action {
                ACTION_DELETE => {
                    conn.execute("DELETE FROM checklist_item WHERE id = ?1", [&item.uuid])?;
                    stats.deleted += 1;
                }
                ACTION_CREATE | ACTION_UPDATE => {
                    let mut checklist = get_checklist_item(conn, &item.uuid)?.unwrap_or_default();
                    checklist.id = item.uuid.clone();
                    checklist.kind = item.kind.clone();
                    wire::apply_checklist_item(&mut checklist, &item.props);
                    save_checklist_item(conn, &checklist)?;
                    stats.upserted += 1;
                }
                other => {
                    tracing::warn!(uuid = %item.uuid, action = other, "unknown checklist action");
                    stats.skipped += 1;
                }
            },
            EntityKind::Tombstone => {
                let deleted = item
                    .props
                    .get("dloid")
                    .or_else(|| item.props.get("do"))
                    .and_then(Value::as_str)
                    .map(ToOwned::to_owned);
                if let Some(id) = deleted {
                    delete_entity(conn, &id)?;
                    stats.deleted += 1;
                } else {
                    stats.skipped += 1;
                }
            }
            EntityKind::Settings => {
                // Settings are not needed for read tools.
                stats.skipped += 1;
            }
            EntityKind::Unknown => {
                tracing::warn!(kind = %item.kind, uuid = %item.uuid, "unknown entity kind");
                stats.unknown += 1;
            }
        }
    }

    Ok(stats)
}

fn delete_entity(conn: &Connection, id: &str) -> Result<()> {
    delete_task(conn, id)?;
    conn.execute("DELETE FROM area WHERE id = ?1", [id])?;
    conn.execute("DELETE FROM tag WHERE id = ?1", [id])?;
    conn.execute("DELETE FROM checklist_item WHERE id = ?1", [id])?;
    // A tombstone may name a tag, area, or task: drop every reference to it.
    conn.execute("DELETE FROM task_tag WHERE tag = ?1", [id])?;
    conn.execute("DELETE FROM area_tag WHERE tag = ?1", [id])?;
    Ok(())
}

fn delete_task(conn: &Connection, id: &str) -> Result<()> {
    conn.execute("DELETE FROM task WHERE id = ?1", [id])?;
    conn.execute("DELETE FROM task_tag WHERE task = ?1", [id])?;
    conn.execute("DELETE FROM checklist_item WHERE task = ?1", [id])?;
    Ok(())
}

// ------------------------------------------------------------------- tasks

fn get_task(conn: &Connection, id: &str) -> Result<Option<Task>> {
    Ok(conn
        .query_row("SELECT * FROM task WHERE id = ?1", [id], task_from_row)
        .optional()?)
}

fn task_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Task> {
    Ok(Task {
        id: row.get("id")?,
        kind: row.get("kind")?,
        type_: row.get("type")?,
        status: row.get("status")?,
        trashed: row.get::<_, i64>("trashed")? != 0,
        title: row.get("title")?,
        notes: row.get("notes")?,
        creation_date: row.get("creation_date")?,
        user_modification_date: row.get("user_modification_date")?,
        stop_date: row.get("stop_date")?,
        last_reminder_interaction_date: row.get("last_reminder_interaction_date")?,
        start: row.get("start")?,
        start_date: row.get("start_date")?,
        start_bucket: row.get("start_bucket")?,
        deadline: row.get("deadline")?,
        deadline_suppression_date: row.get("deadline_suppression_date")?,
        deadline_offset: row.get("deadline_offset")?,
        index: row.get("index_value")?,
        today_index: row.get("today_index")?,
        today_index_reference_date: row.get("today_index_reference_date")?,
        project: row.get("project")?,
        area: row.get("area")?,
        heading: row.get("heading")?,
        tags: Vec::new(),
        recurrence_rule: row.get("recurrence_rule")?,
        repeating_template: row.get("repeating_template")?,
        instance_creation_start_date: row.get("instance_creation_start_date")?,
        instance_creation_paused: row.get::<_, i64>("instance_creation_paused")? != 0,
        instance_creation_count: row.get("instance_creation_count")?,
        after_completion_reference_date: row.get("after_completion_reference_date")?,
        next_instance_start_date: row.get("next_instance_start_date")?,
        leaves_tombstone: row.get::<_, i64>("leaves_tombstone")? != 0,
        experimental: row
            .get::<_, Option<String>>("experimental")?
            .and_then(|text| serde_json::from_str(&text).ok()),
        raw: row
            .get::<_, String>("raw")
            .ok()
            .and_then(|text| serde_json::from_str(&text).ok())
            .unwrap_or_default(),
    })
}

fn save_task(conn: &Connection, task: &Task) -> Result<()> {
    conn.execute(
        "INSERT OR REPLACE INTO task (
            id, kind, \"type\", status, trashed, title, notes,
            creation_date, user_modification_date, stop_date, last_reminder_interaction_date,
            start, start_date, start_bucket, deadline, deadline_suppression_date, deadline_offset,
            index_value, today_index, today_index_reference_date,
            project, area, heading,
            recurrence_rule, repeating_template, instance_creation_start_date,
            instance_creation_paused, instance_creation_count,
            after_completion_reference_date, next_instance_start_date,
            leaves_tombstone, experimental, raw
         ) VALUES (
            ?1, ?2, ?3, ?4, ?5, ?6, ?7,
            ?8, ?9, ?10, ?11,
            ?12, ?13, ?14, ?15, ?16, ?17,
            ?18, ?19, ?20,
            ?21, ?22, ?23,
            ?24, ?25, ?26,
            ?27, ?28,
            ?29, ?30,
            ?31, ?32, ?33
         )",
        params![
            task.id,
            task.kind,
            task.type_,
            task.status,
            task.trashed as i64,
            task.title,
            task.notes,
            task.creation_date,
            task.user_modification_date,
            task.stop_date,
            task.last_reminder_interaction_date,
            task.start,
            task.start_date,
            task.start_bucket,
            task.deadline,
            task.deadline_suppression_date,
            task.deadline_offset,
            task.index,
            task.today_index,
            task.today_index_reference_date,
            task.project,
            task.area,
            task.heading,
            task.recurrence_rule,
            task.repeating_template,
            task.instance_creation_start_date,
            task.instance_creation_paused as i64,
            task.instance_creation_count,
            task.after_completion_reference_date,
            task.next_instance_start_date,
            task.leaves_tombstone as i64,
            task.experimental.as_ref().map(Value::to_string),
            Value::Object(task.raw.clone()).to_string(),
        ],
    )?;
    Ok(())
}

fn replace_task_tags(conn: &Connection, task: &Task) -> Result<()> {
    conn.execute("DELETE FROM task_tag WHERE task = ?1", [&task.id])?;
    for tag in &task.tags {
        conn.execute(
            "INSERT OR IGNORE INTO task_tag(task, tag) VALUES(?1, ?2)",
            params![task.id, tag],
        )?;
    }
    Ok(())
}

// -------------------------------------------------------------------- area

fn get_area(conn: &Connection, id: &str) -> Result<Option<Area>> {
    Ok(conn
        .query_row("SELECT * FROM area WHERE id = ?1", [id], |row| {
            Ok(Area {
                id: row.get("id")?,
                kind: row.get("kind")?,
                title: row.get("title")?,
                visible: row.get::<_, i64>("visible")? != 0,
                index: row.get("index_value")?,
                tags: Vec::new(),
                raw: row
                    .get::<_, String>("raw")
                    .ok()
                    .and_then(|text| serde_json::from_str(&text).ok())
                    .unwrap_or_default(),
            })
        })
        .optional()?)
}

fn save_area(conn: &Connection, area: &Area) -> Result<()> {
    conn.execute(
        "INSERT OR REPLACE INTO area(id, kind, title, visible, index_value, raw)
         VALUES(?1, ?2, ?3, ?4, ?5, ?6)",
        params![
            area.id,
            area.kind,
            area.title,
            area.visible as i64,
            area.index,
            Value::Object(area.raw.clone()).to_string()
        ],
    )?;
    Ok(())
}

fn replace_area_tags(conn: &Connection, area: &Area) -> Result<()> {
    conn.execute("DELETE FROM area_tag WHERE area = ?1", [&area.id])?;
    for tag in &area.tags {
        conn.execute(
            "INSERT OR IGNORE INTO area_tag(area, tag) VALUES(?1, ?2)",
            params![area.id, tag],
        )?;
    }
    Ok(())
}

// --------------------------------------------------------------------- tag

fn get_tag(conn: &Connection, id: &str) -> Result<Option<Tag>> {
    Ok(conn
        .query_row("SELECT * FROM tag WHERE id = ?1", [id], |row| {
            Ok(Tag {
                id: row.get("id")?,
                kind: row.get("kind")?,
                title: row.get("title")?,
                shortcut: row.get("shortcut")?,
                parent: row.get("parent")?,
                index: row.get("index_value")?,
                raw: row
                    .get::<_, String>("raw")
                    .ok()
                    .and_then(|text| serde_json::from_str(&text).ok())
                    .unwrap_or_default(),
            })
        })
        .optional()?)
}

fn save_tag(conn: &Connection, tag: &Tag) -> Result<()> {
    conn.execute(
        "INSERT OR REPLACE INTO tag(id, kind, title, shortcut, parent, index_value, raw)
         VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![
            tag.id,
            tag.kind,
            tag.title,
            tag.shortcut,
            tag.parent,
            tag.index,
            Value::Object(tag.raw.clone()).to_string()
        ],
    )?;
    Ok(())
}

// ---------------------------------------------------------- checklist item

fn get_checklist_item(conn: &Connection, id: &str) -> Result<Option<ChecklistItem>> {
    Ok(conn
        .query_row("SELECT * FROM checklist_item WHERE id = ?1", [id], |row| {
            Ok(ChecklistItem {
                id: row.get("id")?,
                kind: row.get("kind")?,
                title: row.get("title")?,
                status: row.get("status")?,
                index: row.get("index_value")?,
                creation_date: row.get("creation_date")?,
                user_modification_date: row.get("user_modification_date")?,
                stop_date: row.get("stop_date")?,
                task: row.get("task")?,
                leaves_tombstone: row.get::<_, i64>("leaves_tombstone")? != 0,
                raw: row
                    .get::<_, String>("raw")
                    .ok()
                    .and_then(|text| serde_json::from_str(&text).ok())
                    .unwrap_or_default(),
            })
        })
        .optional()?)
}

fn save_checklist_item(conn: &Connection, item: &ChecklistItem) -> Result<()> {
    conn.execute(
        "INSERT OR REPLACE INTO checklist_item (
            id, kind, title, status, index_value, creation_date,
            user_modification_date, stop_date, task, leaves_tombstone, raw
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
        params![
            item.id,
            item.kind,
            item.title,
            item.status,
            item.index,
            item.creation_date,
            item.user_modification_date,
            item.stop_date,
            item.task,
            item.leaves_tombstone as i64,
            Value::Object(item.raw.clone()).to_string()
        ],
    )?;
    Ok(())
}

fn checklist_for(conn: &Connection, task_id: &str) -> Result<Vec<ChecklistItem>> {
    let mut statement = conn.prepare(
        "SELECT * FROM checklist_item WHERE task = ?1
         ORDER BY index_value IS NULL, index_value ASC, title COLLATE NOCASE ASC",
    )?;
    let rows = statement.query_map([task_id], |row| {
        Ok(ChecklistItem {
            id: row.get("id")?,
            kind: row.get("kind")?,
            title: row.get("title")?,
            status: row.get("status")?,
            index: row.get("index_value")?,
            creation_date: row.get("creation_date")?,
            user_modification_date: row.get("user_modification_date")?,
            stop_date: row.get("stop_date")?,
            task: row.get("task")?,
            leaves_tombstone: row.get::<_, i64>("leaves_tombstone")? != 0,
            raw: Default::default(),
        })
    })?;

    let mut items = Vec::new();
    for item in rows {
        items.push(item?);
    }
    Ok(items)
}

/// Attach tag ids to a task loaded from a row (tags live in a junction table).
pub(crate) fn load_task_tags(conn: &Connection, task: &mut Task) -> Result<()> {
    let mut statement = conn.prepare("SELECT tag FROM task_tag WHERE task = ?1")?;
    let rows = statement.query_map([&task.id], |row| row.get::<_, String>(0))?;
    task.tags = rows.collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(())
}

/// Attach tag ids to an area loaded from a row.
pub(crate) fn load_area_tags(conn: &Connection, area: &mut Area) -> Result<()> {
    let mut statement = conn.prepare("SELECT tag FROM area_tag WHERE area = ?1")?;
    let rows = statement.query_map([&area.id], |row| row.get::<_, String>(0))?;
    area.tags = rows.collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stamps_schema_version() {
        let store = Store::open_in_memory().unwrap();
        let version: i64 = store
            .connection()
            .pragma_query_value(None, "user_version", |row| row.get(0))
            .unwrap();
        assert_eq!(version, SCHEMA_VERSION);
    }

    #[test]
    fn rejects_a_newer_database() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("future.sqlite");
        {
            let store = Store::open(&path).unwrap();
            store
                .connection()
                .pragma_update(None, "user_version", SCHEMA_VERSION + 1)
                .unwrap();
        }

        let error = Store::open(&path).err().expect("must reject newer schema");
        assert!(
            error.to_string().contains("newer than supported"),
            "{error}"
        );
    }
}
