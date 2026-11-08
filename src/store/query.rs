//! Read queries: list semantics ported from the reference implementation,
//! tightened with the rules in `docs/04-data-model-storage.md` §4.

use std::collections::HashMap;

use anyhow::Result;
use rusqlite::types::Value as SqlValue;
use rusqlite::{Connection, params_from_iter};
use serde::Serialize;
use serde_json::Value;

use crate::fmt;
use crate::things::model::{STATUS_COMPLETED, TYPE_HEADING, TYPE_PROJECT, TYPE_TODO, Task};

use super::{load_task_tags, task_from_row};

const ORDER_DEFAULT: &str =
    "index_value IS NULL, index_value ASC, title COLLATE NOCASE ASC, id ASC";
const ORDER_TODAY: &str =
    "today_index IS NULL, today_index ASC, index_value IS NULL, index_value ASC, id ASC";
const ORDER_LOGBOOK: &str = "stop_date IS NULL, stop_date DESC, id ASC";
const ORDER_UPCOMING: &str = "start_date IS NULL, start_date ASC, id ASC";
const ORDER_DEADLINE: &str = "deadline IS NULL, deadline ASC, id ASC";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ListKind {
    Inbox,
    Today,
    Upcoming,
    Anytime,
    Someday,
    Logbook,
    Deadlines,
    Repeating,
    Trash,
    All,
    Projects,
    Areas,
    Project(String),
    Area(String),
    Tag(String),
}

impl ListKind {
    pub fn parse(value: &str) -> Option<Self> {
        let value = value.trim();
        match value {
            "inbox" => Some(Self::Inbox),
            "today" => Some(Self::Today),
            "upcoming" => Some(Self::Upcoming),
            "anytime" => Some(Self::Anytime),
            "someday" => Some(Self::Someday),
            "logbook" | "completed" => Some(Self::Logbook),
            "deadlines" => Some(Self::Deadlines),
            "repeating" => Some(Self::Repeating),
            "trash" => Some(Self::Trash),
            "all" => Some(Self::All),
            "projects" => Some(Self::Projects),
            "areas" => Some(Self::Areas),
            _ => {
                if let Some(id) = value.strip_prefix("project-") {
                    Some(Self::Project(id.to_string()))
                } else if let Some(id) = value.strip_prefix("area-") {
                    Some(Self::Area(id.to_string()))
                } else if let Some(id) = value.strip_prefix("tag-") {
                    Some(Self::Tag(id.to_string()))
                } else {
                    None
                }
            }
        }
    }

    pub fn as_str(&self) -> String {
        match self {
            Self::Inbox => "inbox".into(),
            Self::Today => "today".into(),
            Self::Upcoming => "upcoming".into(),
            Self::Anytime => "anytime".into(),
            Self::Someday => "someday".into(),
            Self::Logbook => "logbook".into(),
            Self::Deadlines => "deadlines".into(),
            Self::Repeating => "repeating".into(),
            Self::Trash => "trash".into(),
            Self::All => "all".into(),
            Self::Projects => "projects".into(),
            Self::Areas => "areas".into(),
            Self::Project(id) => format!("project-{id}"),
            Self::Area(id) => format!("area-{id}"),
            Self::Tag(id) => format!("tag-{id}"),
        }
    }

    pub fn is_entity_list(&self) -> bool {
        matches!(self, Self::Projects | Self::Areas)
    }
}

fn filter(kind: &ListKind, today: i64) -> (String, Vec<SqlValue>, &'static str) {
    let task_base =
        format!("\"type\" = {TYPE_TODO} AND status = 0 AND trashed = 0 AND title != ''");

    match kind {
        ListKind::Inbox => (
            format!(
                "{task_base} AND start = 0 AND start_date IS NULL AND project IS NULL AND area IS NULL"
            ),
            vec![],
            ORDER_DEFAULT,
        ),
        ListKind::Today => (
            // Things rolls unfinished Today tasks over, so anything started
            // with a date up to today stays in the list. We accept either the
            // `startDate` or the `todayIndexReferenceDate` signal.
            format!(
                "{task_base} AND start = 1 AND (
                    (today_index_reference_date IS NOT NULL AND today_index_reference_date <= ?)
                    OR (start_date IS NOT NULL AND start_date <= ?)
                )"
            ),
            vec![SqlValue::Integer(today), SqlValue::Integer(today)],
            ORDER_TODAY,
        ),
        ListKind::Upcoming => (
            format!(
                "\"type\" = {TYPE_TODO} AND status = 0 AND trashed = 0 AND title != '' AND start_date > ?"
            ),
            vec![SqlValue::Integer(today)],
            ORDER_UPCOMING,
        ),
        ListKind::Anytime => (
            // Started tasks without a date, plus undated tasks that live inside
            // a project or area (`st=0`).
            format!(
                "{task_base} AND (
                    (start = 1 AND start_date IS NULL)
                    OR (start = 0 AND (project IS NOT NULL OR area IS NOT NULL))
                )"
            ),
            vec![],
            ORDER_DEFAULT,
        ),
        ListKind::Someday => (
            format!("{task_base} AND start = 2 AND start_date IS NULL"),
            vec![],
            ORDER_DEFAULT,
        ),
        ListKind::Logbook => (
            format!(
                "status IN (2, {STATUS_COMPLETED}) AND \"type\" != {TYPE_HEADING} AND trashed = 0"
            ),
            vec![],
            ORDER_LOGBOOK,
        ),
        ListKind::Deadlines => (
            format!(
                "\"type\" = {TYPE_TODO} AND status = 0 AND trashed = 0 AND title != '' AND deadline IS NOT NULL"
            ),
            vec![],
            ORDER_DEADLINE,
        ),
        ListKind::Repeating => (
            format!("\"type\" != {TYPE_HEADING} AND trashed = 0 AND recurrence_rule IS NOT NULL"),
            vec![],
            ORDER_DEFAULT,
        ),
        ListKind::Trash => ("trashed = 1".to_string(), vec![], ORDER_DEFAULT),
        ListKind::All => (
            "\"type\" = 0 AND status = 0 AND trashed = 0 AND title != ''".to_string(),
            vec![],
            ORDER_DEFAULT,
        ),
        ListKind::Project(id) => (
            format!(
                "\"type\" = {TYPE_TODO} AND status = 0 AND trashed = 0 AND title != '' AND project = ?"
            ),
            vec![SqlValue::Text(id.clone())],
            ORDER_DEFAULT,
        ),
        ListKind::Area(id) => (
            format!(
                "\"type\" = {TYPE_TODO} AND status = 0 AND trashed = 0 AND title != '' AND area = ?"
            ),
            vec![SqlValue::Text(id.clone())],
            ORDER_DEFAULT,
        ),
        ListKind::Tag(id) => (
            format!(
                "\"type\" = {TYPE_TODO} AND status = 0 AND trashed = 0 AND title != ''
                 AND id IN (SELECT task FROM task_tag WHERE tag = ?)"
            ),
            vec![SqlValue::Text(id.clone())],
            ORDER_DEFAULT,
        ),
        // Handled by the dedicated functions below.
        ListKind::Projects | ListKind::Areas => ("0 = 1".to_string(), vec![], ORDER_DEFAULT),
    }
}

pub fn list_tasks(
    conn: &Connection,
    kind: &ListKind,
    today: i64,
    limit: usize,
    offset: usize,
) -> Result<Vec<Task>> {
    if kind.is_entity_list() {
        return Ok(Vec::new());
    }

    let (where_sql, mut params, order) = filter(kind, today);
    let sql = format!("SELECT * FROM task WHERE {where_sql} ORDER BY {order} LIMIT ? OFFSET ?");
    params.push(SqlValue::Integer(limit as i64));
    params.push(SqlValue::Integer(offset as i64));

    let mut statement = conn.prepare(&sql)?;
    let rows = statement.query_map(params_from_iter(params.iter()), task_from_row)?;
    let mut tasks = Vec::new();
    for row in rows {
        tasks.push(row?);
    }

    attach_tags(conn, &mut tasks)?;
    Ok(tasks)
}

/// Total number of tasks in one list, ignoring limit/offset.
pub fn count_tasks(conn: &Connection, kind: &ListKind, today: i64) -> Result<i64> {
    let (where_sql, params, _) = filter(kind, today);
    let sql = format!("SELECT COUNT(*) FROM task WHERE {where_sql}");
    Ok(conn.query_row(&sql, params_from_iter(params.iter()), |row| row.get(0))?)
}

pub fn task_counts(conn: &Connection, today: i64) -> Result<Vec<(String, i64)>> {
    let kinds = [
        ListKind::Inbox,
        ListKind::Today,
        ListKind::Upcoming,
        ListKind::Anytime,
        ListKind::Someday,
        ListKind::Logbook,
        ListKind::Deadlines,
        ListKind::Repeating,
        ListKind::Trash,
    ];

    let mut counts = Vec::new();
    for kind in kinds {
        let (where_sql, params, _) = filter(&kind, today);
        let sql = format!("SELECT COUNT(*) FROM task WHERE {where_sql}");
        let count: i64 = conn.query_row(&sql, params_from_iter(params.iter()), |row| row.get(0))?;
        counts.push((kind.as_str(), count));
    }

    // Entity counts, handy for a quick sanity check against the app sidebar.
    let projects: i64 = conn.query_row(
        "SELECT COUNT(*) FROM task WHERE \"type\" = ?1 AND trashed = 0 AND title != ''",
        [TYPE_PROJECT],
        |row| row.get(0),
    )?;
    let areas: i64 = conn.query_row("SELECT COUNT(*) FROM area WHERE title != ''", [], |row| {
        row.get(0)
    })?;
    let tags: i64 = conn.query_row("SELECT COUNT(*) FROM tag WHERE title != ''", [], |row| {
        row.get(0)
    })?;
    counts.push(("projects".into(), projects));
    counts.push(("areas".into(), areas));
    counts.push(("tags".into(), tags));

    Ok(counts)
}

fn attach_tags(conn: &Connection, tasks: &mut [Task]) -> Result<()> {
    for task in tasks.iter_mut() {
        load_task_tags(conn, task)?;
    }
    Ok(())
}

// ------------------------------------------------------------------ outputs

#[derive(Debug, Serialize)]
pub struct ChecklistSummary {
    pub id: String,
    pub title: String,
    pub completed: bool,
    pub index: Option<i64>,
}

#[derive(Debug, Serialize)]
pub struct TaskSummary {
    pub id: String,
    pub title: String,
    #[serde(rename = "type")]
    pub kind: String,
    pub status: String,
    pub view: String,
    pub start_date: Option<String>,
    pub deadline: Option<String>,
    pub project: Option<String>,
    pub project_title: Option<String>,
    pub area: Option<String>,
    pub area_title: Option<String>,
    pub heading: Option<String>,
    pub heading_title: Option<String>,
    pub tags: Vec<String>,
    pub completed_at: Option<String>,
    pub checklist_items: u32,
    pub open_checklist_items: u32,
}

#[derive(Debug, Serialize)]
pub struct TaskDetail {
    #[serde(flatten)]
    pub summary: TaskSummary,
    pub notes: String,
    pub start_raw: i64,
    pub start_bucket: i64,
    pub index: Option<i64>,
    pub today_index: Option<i64>,
    pub today_index_reference_date: Option<String>,
    pub created_at: Option<String>,
    pub modified_at: Option<String>,
    pub recurrence: Option<Value>,
    pub checklist: Vec<ChecklistSummary>,
    pub raw: Value,
}

fn title_of_task(conn: &Connection, id: &str) -> Result<Option<String>> {
    Ok(conn
        .query_row("SELECT title FROM task WHERE id = ?1", [id], |row| {
            row.get::<_, String>(0)
        })
        .ok())
}

fn title_of_area(conn: &Connection, id: &str) -> Result<Option<String>> {
    Ok(conn
        .query_row("SELECT title FROM area WHERE id = ?1", [id], |row| {
            row.get::<_, String>(0)
        })
        .ok())
}

fn checklist_counts(conn: &Connection, task_id: &str) -> Result<(u32, u32)> {
    let (total, open): (i64, i64) = conn.query_row(
        "SELECT COUNT(*),
                COALESCE(SUM(CASE WHEN status = 3 THEN 1 ELSE 0 END), 0)
         FROM checklist_item WHERE task = ?1",
        [task_id],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    Ok((total as u32, open as u32))
}

pub fn task_summary(conn: &Connection, task: &Task, today: i64) -> Result<TaskSummary> {
    let (checklist_items, open_checklist_items) = checklist_counts(conn, &task.id)?;
    let project_title = match &task.project {
        Some(id) => title_of_task(conn, id)?,
        None => None,
    };
    let heading_title = match &task.heading {
        Some(id) => title_of_task(conn, id)?,
        None => None,
    };
    let area_title = match &task.area {
        Some(id) => title_of_area(conn, id)?,
        None => None,
    };

    Ok(TaskSummary {
        id: task.id.clone(),
        title: task.title.clone(),
        kind: task.type_str().to_string(),
        status: task.status_str().to_string(),
        view: task.view_str(today).to_string(),
        start_date: task.start_date.and_then(fmt::date_string),
        deadline: task.deadline.and_then(fmt::date_string),
        project: task.project.clone(),
        project_title,
        area: task.area.clone(),
        area_title,
        heading: task.heading.clone(),
        heading_title,
        tags: task.tags.clone(),
        completed_at: task.stop_date.and_then(fmt::instant_string),
        checklist_items,
        open_checklist_items,
    })
}

pub fn summaries(conn: &Connection, tasks: &[Task], today: i64) -> Result<Vec<TaskSummary>> {
    tasks
        .iter()
        .map(|task| task_summary(conn, task, today))
        .collect()
}

pub fn task_detail(conn: &Connection, id: &str, today: i64) -> Result<Option<TaskDetail>> {
    let Some(mut task) = conn
        .query_row("SELECT * FROM task WHERE id = ?1", [id], task_from_row)
        .ok()
    else {
        return Ok(None);
    };
    load_task_tags(conn, &mut task)?;

    let summary = task_summary(conn, &task, today)?;
    let checklist = super::checklist_for(conn, id)?
        .into_iter()
        .filter(|item| !item.leaves_tombstone)
        .map(|item| {
            let completed = item.is_completed();
            ChecklistSummary {
                id: item.id,
                title: item.title,
                completed,
                index: item.index,
            }
        })
        .collect();

    Ok(Some(TaskDetail {
        summary,
        notes: task.notes.clone(),
        start_raw: task.start,
        start_bucket: task.start_bucket,
        index: task.index,
        today_index: task.today_index,
        today_index_reference_date: task.today_index_reference_date.and_then(fmt::date_string),
        created_at: task.creation_date.and_then(fmt::instant_string),
        modified_at: task.user_modification_date.and_then(fmt::instant_string),
        recurrence: task
            .recurrence_rule
            .as_deref()
            .and_then(|text| serde_json::from_str(text).ok()),
        checklist,
        raw: Value::Object(task.raw.clone()),
    }))
}

// ----------------------------------------------------------------- projects

#[derive(Debug, Serialize)]
pub struct ProjectSummary {
    pub id: String,
    pub title: String,
    pub status: String,
    pub area: Option<String>,
    pub area_title: Option<String>,
    pub open_tasks: u32,
    pub completed_tasks: u32,
    pub headings: u32,
}

pub fn list_projects(
    conn: &Connection,
    area_id: Option<&str>,
    status: Option<&str>,
    limit: usize,
) -> Result<Vec<ProjectSummary>> {
    let mut sql =
        String::from("SELECT * FROM task WHERE \"type\" = ?1 AND trashed = 0 AND title != ''");
    let mut params: Vec<SqlValue> = vec![SqlValue::Integer(TYPE_PROJECT)];
    if let Some(area) = area_id {
        sql.push_str(" AND area = ?");
        params.push(SqlValue::Text(area.to_string()));
    }
    match status {
        Some("completed") => sql.push_str(" AND status = 3"),
        Some("canceled") => sql.push_str(" AND status = 2"),
        Some("all") => {}
        _ => sql.push_str(" AND status = 0"),
    }
    sql.push_str(" ORDER BY index_value IS NULL, index_value ASC, title COLLATE NOCASE ASC");
    sql.push_str(&format!(" LIMIT {limit}"));

    let mut statement = conn.prepare(&sql)?;
    let rows = statement.query_map(params_from_iter(params.iter()), task_from_row)?;
    let mut projects = Vec::new();
    for row in rows {
        projects.push(row?);
    }

    projects
        .iter()
        .map(|project| project_summary(conn, project))
        .collect()
}

fn project_summary(conn: &Connection, project: &Task) -> Result<ProjectSummary> {
    let open_tasks: i64 = conn.query_row(
        "SELECT COUNT(*) FROM task WHERE project = ?1 AND \"type\" = 0 AND trashed = 0 AND status = 0",
        [&project.id],
        |row| row.get(0),
    )?;
    let completed_tasks: i64 = conn.query_row(
        "SELECT COUNT(*) FROM task WHERE project = ?1 AND \"type\" = 0 AND trashed = 0 AND status = 3",
        [&project.id],
        |row| row.get(0),
    )?;
    let headings: i64 = conn.query_row(
        "SELECT COUNT(*) FROM task WHERE project = ?1 AND \"type\" = 2 AND trashed = 0",
        [&project.id],
        |row| row.get(0),
    )?;
    let area_title = match &project.area {
        Some(id) => title_of_area(conn, id)?,
        None => None,
    };

    Ok(ProjectSummary {
        id: project.id.clone(),
        title: project.title.clone(),
        status: project.status_str().to_string(),
        area: project.area.clone(),
        area_title,
        open_tasks: open_tasks as u32,
        completed_tasks: completed_tasks as u32,
        headings: headings as u32,
    })
}

#[derive(Debug, Serialize)]
pub struct HeadingSummary {
    pub id: String,
    pub title: String,
    pub index: Option<i64>,
    pub open_tasks: u32,
}

#[derive(Debug, Serialize)]
pub struct ProjectDetail {
    #[serde(flatten)]
    pub summary: ProjectSummary,
    pub notes: String,
    pub headings: Vec<HeadingSummary>,
    pub tasks: Vec<TaskSummary>,
}

pub fn project_detail(
    conn: &Connection,
    id: &str,
    today: i64,
    limit: usize,
) -> Result<Option<ProjectDetail>> {
    let Some(project) = conn
        .query_row(
            "SELECT * FROM task WHERE id = ?1 AND \"type\" = 1",
            [id],
            task_from_row,
        )
        .ok()
    else {
        return Ok(None);
    };

    let summary = project_summary(conn, &project)?;

    let mut statement = conn.prepare(
        "SELECT * FROM task WHERE project = ?1 AND \"type\" = 2 AND trashed = 0
         ORDER BY index_value IS NULL, index_value ASC, title COLLATE NOCASE ASC",
    )?;
    let rows = statement.query_map([id], task_from_row)?;
    let mut headings = Vec::new();
    for row in rows {
        let heading = row?;
        let open_tasks: i64 = conn.query_row(
            "SELECT COUNT(*) FROM task WHERE heading = ?1 AND \"type\" = 0 AND trashed = 0 AND status = 0",
            [&heading.id],
            |row| row.get(0),
        )?;
        headings.push(HeadingSummary {
            id: heading.id,
            title: heading.title,
            index: heading.index,
            open_tasks: open_tasks as u32,
        });
    }

    let tasks = list_tasks(conn, &ListKind::Project(id.to_string()), today, limit, 0)?;
    let tasks = summaries(conn, &tasks, today)?;

    Ok(Some(ProjectDetail {
        summary,
        notes: project.notes,
        headings,
        tasks,
    }))
}

#[derive(Debug, Serialize)]
pub struct AreaDetail {
    pub id: String,
    pub title: String,
    pub projects: Vec<ProjectSummary>,
    pub tasks: Vec<TaskSummary>,
}

pub fn area_detail(
    conn: &Connection,
    id: &str,
    today: i64,
    limit: usize,
) -> Result<Option<AreaDetail>> {
    let Some(title) = conn
        .query_row("SELECT title FROM area WHERE id = ?1", [id], |row| {
            row.get::<_, String>(0)
        })
        .ok()
    else {
        return Ok(None);
    };

    let projects = list_projects(conn, Some(id), Some("open"), limit)?;
    let tasks = list_tasks(conn, &ListKind::Area(id.to_string()), today, limit, 0)?;
    let tasks = summaries(conn, &tasks, today)?;

    Ok(Some(AreaDetail {
        id: id.to_string(),
        title,
        projects,
        tasks,
    }))
}

#[derive(Debug, Serialize)]
pub struct TagRef {
    pub id: String,
    pub title: String,
}

#[derive(Debug, Serialize)]
pub struct TagDetail {
    pub id: String,
    pub title: String,
    pub shortcut: Option<String>,
    pub parent: Option<String>,
    pub parent_title: Option<String>,
    pub children: Vec<TagRef>,
    pub tasks: Vec<TaskSummary>,
}

pub fn tag_detail(
    conn: &Connection,
    id: &str,
    today: i64,
    limit: usize,
) -> Result<Option<TagDetail>> {
    let Some((title, shortcut, parent)) = conn
        .query_row(
            "SELECT title, shortcut, parent FROM tag WHERE id = ?1",
            [id],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, Option<String>>(1)?,
                    row.get::<_, Option<String>>(2)?,
                ))
            },
        )
        .ok()
    else {
        return Ok(None);
    };

    let parent_title = match &parent {
        Some(parent_id) => conn
            .query_row("SELECT title FROM tag WHERE id = ?1", [parent_id], |row| {
                row.get::<_, String>(0)
            })
            .ok(),
        None => None,
    };

    let mut statement = conn.prepare(
        "SELECT id, title FROM tag WHERE parent = ?1
         ORDER BY index_value IS NULL, index_value ASC, title COLLATE NOCASE ASC",
    )?;
    let rows = statement.query_map([id], |row| {
        Ok(TagRef {
            id: row.get(0)?,
            title: row.get(1)?,
        })
    })?;
    let mut children = Vec::new();
    for row in rows {
        children.push(row?);
    }

    let tasks = list_tasks(conn, &ListKind::Tag(id.to_string()), today, limit, 0)?;
    let tasks = summaries(conn, &tasks, today)?;

    Ok(Some(TagDetail {
        id: id.to_string(),
        title,
        shortcut,
        parent,
        parent_title,
        children,
        tasks,
    }))
}

// -------------------------------------------------------------------- areas

#[derive(Debug, Serialize)]
pub struct AreaSummary {
    pub id: String,
    pub title: String,
    pub projects: u32,
    pub open_tasks: u32,
}

pub fn list_areas(conn: &Connection, limit: usize) -> Result<Vec<AreaSummary>> {
    let mut statement = conn.prepare(
        "SELECT id, title FROM area WHERE title != ''
         ORDER BY index_value IS NULL, index_value ASC, title COLLATE NOCASE ASC
         LIMIT ?1",
    )?;
    let rows = statement.query_map([limit as i64], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
    })?;

    let mut out = Vec::new();
    for row in rows {
        let (id, title) = row?;
        let projects: i64 = conn.query_row(
            "SELECT COUNT(*) FROM task WHERE area = ?1 AND \"type\" = 1 AND trashed = 0",
            [&id],
            |row| row.get(0),
        )?;
        let open_tasks: i64 = conn.query_row(
            "SELECT COUNT(*) FROM task WHERE area = ?1 AND \"type\" = 0 AND trashed = 0 AND status = 0",
            [&id],
            |row| row.get(0),
        )?;
        out.push(AreaSummary {
            id,
            title,
            projects: projects as u32,
            open_tasks: open_tasks as u32,
        });
    }

    Ok(out)
}

// --------------------------------------------------------------------- tags

#[derive(Debug, Serialize)]
pub struct TagSummary {
    pub id: String,
    pub title: String,
    pub shortcut: Option<String>,
    pub parent: Option<String>,
    pub parent_title: Option<String>,
    pub open_tasks: u32,
}

pub fn list_tags(conn: &Connection, limit: usize) -> Result<Vec<TagSummary>> {
    let mut statement = conn.prepare(
        "SELECT id, title, shortcut, parent FROM tag WHERE title != ''
         ORDER BY index_value IS NULL, index_value ASC, title COLLATE NOCASE ASC
         LIMIT ?1",
    )?;
    let rows = statement.query_map([limit as i64], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, Option<String>>(2)?,
            row.get::<_, Option<String>>(3)?,
        ))
    })?;

    let mut out = Vec::new();
    for row in rows {
        let (id, title, shortcut, parent) = row?;
        let open_tasks: i64 = conn.query_row(
            "SELECT COUNT(*) FROM task t JOIN task_tag tt ON tt.task = t.id
             WHERE tt.tag = ?1 AND t.trashed = 0 AND t.status = 0",
            [&id],
            |row| row.get(0),
        )?;
        let parent_title = match &parent {
            Some(parent_id) => conn
                .query_row("SELECT title FROM tag WHERE id = ?1", [parent_id], |row| {
                    row.get::<_, String>(0)
                })
                .ok(),
            None => None,
        };
        out.push(TagSummary {
            id,
            title,
            shortcut,
            parent,
            parent_title,
            open_tasks: open_tasks as u32,
        });
    }

    Ok(out)
}

// ------------------------------------------------------------------- search

#[derive(Debug, Default, Clone)]
pub struct SearchFilter {
    pub query: Option<String>,
    pub status: Option<i64>,
    pub start: Option<i64>,
    pub project_id: Option<String>,
    pub area_id: Option<String>,
    pub tag_ids: Vec<String>,
    pub deadline_before: Option<i64>,
    pub deadline_after: Option<i64>,
    pub include_completed: bool,
}

pub fn search_tasks(
    conn: &Connection,
    filter: &SearchFilter,
    limit: usize,
    offset: usize,
) -> Result<Vec<Task>> {
    let mut where_sql = String::from("\"type\" = 0 AND trashed = 0 AND title != ''");
    let mut params: Vec<SqlValue> = Vec::new();

    match filter.status {
        Some(status) => {
            where_sql.push_str(" AND status = ?");
            params.push(SqlValue::Integer(status));
        }
        None if filter.include_completed => {
            where_sql.push_str(" AND status IN (0, 2, 3)");
        }
        None => where_sql.push_str(" AND status = 0"),
    }

    if let Some(start) = filter.start {
        where_sql.push_str(" AND start = ?");
        params.push(SqlValue::Integer(start));
    }
    if let Some(query) = filter
        .query
        .as_deref()
        .map(str::trim)
        .filter(|q| !q.is_empty())
    {
        where_sql.push_str(" AND (title LIKE ? COLLATE NOCASE OR notes LIKE ? COLLATE NOCASE)");
        let pattern = format!("%{query}%");
        params.push(SqlValue::Text(pattern.clone()));
        params.push(SqlValue::Text(pattern));
    }
    if let Some(project) = &filter.project_id {
        where_sql.push_str(" AND project = ?");
        params.push(SqlValue::Text(project.clone()));
    }
    if let Some(area) = &filter.area_id {
        where_sql.push_str(" AND area = ?");
        params.push(SqlValue::Text(area.clone()));
    }
    if !filter.tag_ids.is_empty() {
        let placeholders = vec!["?"; filter.tag_ids.len()].join(",");
        where_sql.push_str(&format!(
            " AND id IN (SELECT task FROM task_tag WHERE tag IN ({placeholders}))"
        ));
        for tag in &filter.tag_ids {
            params.push(SqlValue::Text(tag.clone()));
        }
    }
    if let Some(day) = filter.deadline_before {
        where_sql.push_str(" AND deadline IS NOT NULL AND deadline <= ?");
        params.push(SqlValue::Integer(day));
    }
    if let Some(day) = filter.deadline_after {
        where_sql.push_str(" AND deadline IS NOT NULL AND deadline >= ?");
        params.push(SqlValue::Integer(day));
    }

    let sql =
        format!("SELECT * FROM task WHERE {where_sql} ORDER BY {ORDER_DEFAULT} LIMIT ? OFFSET ?");
    params.push(SqlValue::Integer(limit as i64));
    params.push(SqlValue::Integer(offset as i64));

    let mut statement = conn.prepare(&sql)?;
    let rows = statement.query_map(params_from_iter(params.iter()), task_from_row)?;
    let mut tasks = Vec::new();
    for row in rows {
        tasks.push(row?);
    }
    attach_tags(conn, &mut tasks)?;
    Ok(tasks)
}

/// Resolve ids to titles in a single query, used by MCP resources later.
#[allow(dead_code)]
pub fn titles(conn: &Connection, table: &str, ids: &[String]) -> Result<HashMap<String, String>> {
    if ids.is_empty() {
        return Ok(HashMap::new());
    }
    let placeholders = vec!["?"; ids.len()].join(",");
    let sql = format!("SELECT id, title FROM {table} WHERE id IN ({placeholders})");
    let mut statement = conn.prepare(&sql)?;
    let rows = statement.query_map(params_from_iter(ids.iter()), |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
    })?;
    let mut map = HashMap::new();
    for row in rows {
        let (id, title) = row?;
        map.insert(id, title);
    }
    Ok(map)
}
