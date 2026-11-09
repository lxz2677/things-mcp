//! Consistency audit: compare decoded columns against the raw wire properties
//! and check referential integrity. Run with `things-mcp doctor` after a sync;
//! every check should be `ok`.

use anyhow::Result;
use rusqlite::Connection;
use serde::Serialize;

#[derive(Debug, Serialize)]
pub struct CheckResult {
    pub name: &'static str,
    pub count: i64,
    pub samples: Vec<String>,
    pub ok: bool,
}

pub fn audit(conn: &Connection) -> Result<Vec<CheckResult>> {
    let checks: [(&'static str, &str); 20] = [
        (
            "decode.title",
            "SELECT id FROM task
             WHERE json_type(raw, '$.tt') = 'text' AND title != json_extract(raw, '$.tt')",
        ),
        (
            "decode.start",
            "SELECT id FROM task
             WHERE json_type(raw, '$.st') = 'integer' AND start != json_extract(raw, '$.st')",
        ),
        (
            "decode.status",
            "SELECT id FROM task
             WHERE json_type(raw, '$.ss') = 'integer' AND status != json_extract(raw, '$.ss')",
        ),
        (
            "decode.type",
            "SELECT id FROM task
             WHERE json_type(raw, '$.tp') = 'integer' AND \"type\" != json_extract(raw, '$.tp')",
        ),
        (
            "decode.trashed",
            "SELECT id FROM task
             WHERE json_type(raw, '$.tr') IN ('true', 'false', 'integer')
               AND trashed != CAST(json_extract(raw, '$.tr') AS INTEGER)",
        ),
        (
            "decode.index",
            "SELECT id FROM task
             WHERE json_type(raw, '$.ix') = 'integer'
               AND (index_value IS NULL OR index_value != json_extract(raw, '$.ix'))",
        ),
        (
            "decode.today_index",
            "SELECT id FROM task
             WHERE json_type(raw, '$.ti') = 'integer'
               AND (today_index IS NULL OR today_index != json_extract(raw, '$.ti'))",
        ),
        (
            "decode.start_date",
            "SELECT id FROM task
             WHERE (json_type(raw, '$.sr') IN ('integer', 'real')
                    AND start_date IS NOT CAST(json_extract(raw, '$.sr') / 86400 AS INTEGER))
                OR (json_type(raw, '$.sr') = 'null' AND start_date IS NOT NULL)",
        ),
        (
            "decode.deadline",
            "SELECT id FROM task
             WHERE (json_type(raw, '$.dd') IN ('integer', 'real')
                    AND deadline IS NOT CAST(json_extract(raw, '$.dd') / 86400 AS INTEGER))
                OR (json_type(raw, '$.dd') = 'null' AND deadline IS NOT NULL)",
        ),
        (
            "decode.tir",
            "SELECT id FROM task
             WHERE (json_type(raw, '$.tir') IN ('integer', 'real')
                    AND today_index_reference_date IS NOT CAST(json_extract(raw, '$.tir') / 86400 AS INTEGER))
                OR (json_type(raw, '$.tir') = 'null' AND today_index_reference_date IS NOT NULL)",
        ),
        (
            "decode.project",
            "SELECT id FROM task
             WHERE json_type(raw, '$.pr') = 'array'
               AND COALESCE(project, '') != COALESCE(json_extract(raw, '$.pr[0]'), '')",
        ),
        (
            "decode.area",
            "SELECT id FROM task
             WHERE json_type(raw, '$.ar') = 'array'
               AND COALESCE(area, '') != COALESCE(json_extract(raw, '$.ar[0]'), '')",
        ),
        (
            "decode.heading",
            "SELECT id FROM task
             WHERE json_type(raw, '$.agr') = 'array'
               AND COALESCE(heading, '') != COALESCE(json_extract(raw, '$.agr[0]'), '')",
        ),
        (
            "decode.reminder",
            "SELECT id FROM task
             WHERE reminder_time IS NOT CASE
                       WHEN json_type(raw, '$.ato') = 'integer'
                        AND json_extract(raw, '$.ato') BETWEEN 0 AND 86399
                       THEN json_extract(raw, '$.ato') END",
        ),
        (
            "decode.notes",
            "SELECT id FROM task
             WHERE json_type(raw, '$.nt') = 'object'
               AND json_extract(raw, '$.nt.ps') IS NULL
               AND json_type(raw, '$.nt.v') = 'text'
               AND notes != json_extract(raw, '$.nt.v')",
        ),
        (
            "ref.project_missing",
            "SELECT id FROM task
             WHERE project IS NOT NULL AND project NOT IN (SELECT id FROM task)",
        ),
        (
            "ref.area_missing",
            "SELECT id FROM task
             WHERE area IS NOT NULL AND area NOT IN (SELECT id FROM area)",
        ),
        (
            "ref.heading_missing",
            "SELECT id FROM task
             WHERE heading IS NOT NULL AND heading NOT IN (SELECT id FROM task)",
        ),
        (
            "ref.tag_missing",
            "SELECT task FROM task_tag WHERE tag NOT IN (SELECT id FROM tag)",
        ),
        (
            "ref.checklist_missing_task",
            "SELECT id FROM checklist_item
             WHERE task IS NOT NULL AND task NOT IN (SELECT id FROM task)",
        ),
    ];

    let mut results = Vec::new();
    for (name, sql) in checks {
        let count: i64 = conn.query_row(&format!("SELECT COUNT(*) FROM ({sql})"), [], |row| {
            row.get(0)
        })?;
        let samples = if count == 0 {
            Vec::new()
        } else {
            let mut statement = conn.prepare(sql)?;
            let rows = statement.query_map([], |row| row.get::<_, String>(0))?;
            rows.take(5).collect::<rusqlite::Result<Vec<_>>>()?
        };
        results.push(CheckResult {
            name,
            count,
            samples,
            ok: count == 0,
        });
    }

    // Structural checks that are informational rather than failures.
    let empty_titles: i64 =
        conn.query_row("SELECT COUNT(*) FROM task WHERE title = ''", [], |row| {
            row.get(0)
        })?;
    results.push(CheckResult {
        name: "info.empty_title",
        count: empty_titles,
        samples: Vec::new(),
        ok: true,
    });

    let unknown_kinds: i64 = conn.query_row(
        "SELECT COUNT(*) FROM task WHERE kind NOT LIKE 'Task%'",
        [],
        |row| row.get(0),
    )?;
    results.push(CheckResult {
        name: "info.unknown_kind",
        count: unknown_kinds,
        samples: Vec::new(),
        ok: true,
    });

    let compat_status: Option<String> = conn
        .query_row(
            "SELECT value FROM meta WHERE key = 'compat_status'",
            [],
            |row| row.get::<_, String>(0),
        )
        .ok();
    let compat_reason: Option<String> = conn
        .query_row(
            "SELECT value FROM meta WHERE key = 'compat_reason'",
            [],
            |row| row.get::<_, String>(0),
        )
        .ok()
        .filter(|reason| !reason.is_empty());
    let incompatible = compat_status.as_deref() == Some("incompatible");
    results.push(CheckResult {
        name: "compat.status",
        count: i64::from(incompatible),
        samples: compat_reason.into_iter().collect(),
        ok: !incompatible,
    });

    let cursor: i64 = meta_i64(conn, "processed_remote_index")?.unwrap_or(0);
    let head = meta_i64(conn, "latest_server_index")?;
    let ahead = head.is_some_and(|head| cursor > head);
    results.push(CheckResult {
        name: "sync.cursor_ahead_of_head",
        count: i64::from(ahead),
        samples: Vec::new(),
        ok: !ahead,
    });

    Ok(results)
}

fn meta_i64(conn: &Connection, key: &str) -> Result<Option<i64>> {
    let value: Option<String> = conn
        .query_row("SELECT value FROM meta WHERE key = ?1", [key], |row| {
            row.get(0)
        })
        .ok();
    Ok(value.and_then(|value| value.parse().ok()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::things::{ACTION_CREATE, IncomingItem};
    use serde_json::json;

    fn seeded() -> crate::store::Store {
        let mut store = crate::store::Store::open_in_memory().unwrap();
        store
            .apply_items(&[IncomingItem {
                uuid: "t1".into(),
                action: ACTION_CREATE,
                kind: "Task7".into(),
                props: json!({
                    "tt": "title", "st": 1, "ss": 0, "tp": 0,
                    "sr": 1792368000, "tir": 1792368000,
                    "ix": -3, "ti": -7, "tr": false,
                    "pr": ["missing-project"]
                })
                .as_object()
                .unwrap()
                .clone(),
                server_index: 0,
            }])
            .unwrap();
        store
    }

    #[test]
    fn clean_decode_passes_but_dangling_reference_is_flagged() {
        let store = seeded();
        let results = audit(store.connection()).unwrap();

        let by_name = |name: &str| {
            results
                .iter()
                .find(|result| result.name == name)
                .unwrap_or_else(|| panic!("missing check {name}"))
        };

        assert!(by_name("decode.title").ok);
        assert!(by_name("decode.start").ok);
        assert!(by_name("decode.start_date").ok);
        assert!(by_name("decode.project").ok);
        assert!(
            !by_name("ref.project_missing").ok,
            "a project id without a row must be flagged"
        );
    }

    #[test]
    fn flags_an_incompatible_replica() {
        let store = seeded();
        store
            .set_compat_incompatible(
                "cloud schema 302 (supported 301)",
                Some(302),
                &std::collections::BTreeMap::new(),
            )
            .unwrap();

        let results = audit(store.connection()).unwrap();
        let check = results
            .iter()
            .find(|result| result.name == "compat.status")
            .unwrap();
        assert!(!check.ok);
        assert_eq!(check.count, 1);
    }

    #[test]
    fn detects_a_corrupted_column() {
        let store = seeded();
        store
            .connection()
            .execute("UPDATE task SET title = 'tampered' WHERE id = 't1'", [])
            .unwrap();

        let results = audit(store.connection()).unwrap();
        let title = results
            .iter()
            .find(|result| result.name == "decode.title")
            .unwrap();
        assert!(!title.ok);
        assert_eq!(title.count, 1);
        assert_eq!(title.samples, vec!["t1"]);
    }
}
