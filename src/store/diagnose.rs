//! Diagnostics for verifying list semantics against a real account.
//!
//! The list predicates in `docs/04` and the reference implementation disagree
//! on a few points (which field marks a task as "Today", whether Someday
//! excludes dated tasks, ...). `verify` prints these candidate counts so a
//! mismatch with Things.app can be traced to a single predicate instead of
//! guessing.

use std::collections::BTreeMap;

use anyhow::Result;
use rusqlite::Connection;
use serde::Serialize;

#[derive(Debug, Default, Serialize)]
pub struct Diagnostics {
    pub totals: BTreeMap<String, i64>,
    pub candidates: BTreeMap<String, i64>,
}

pub fn run(conn: &Connection, today: i64) -> Result<Diagnostics> {
    let mut totals = BTreeMap::new();
    let mut candidates = BTreeMap::new();

    for (label, sql) in [
        ("task", "SELECT COUNT(*) FROM task"),
        ("area", "SELECT COUNT(*) FROM area"),
        ("tag", "SELECT COUNT(*) FROM tag"),
        ("checklist_item", "SELECT COUNT(*) FROM checklist_item"),
        (
            "task.open",
            "SELECT COUNT(*) FROM task WHERE status = 0 AND trashed = 0",
        ),
        (
            "task.completed",
            "SELECT COUNT(*) FROM task WHERE status = 3 AND trashed = 0",
        ),
        (
            "task.canceled",
            "SELECT COUNT(*) FROM task WHERE status = 2 AND trashed = 0",
        ),
        (
            "task.trashed",
            "SELECT COUNT(*) FROM task WHERE trashed = 1",
        ),
        ("task.type0", "SELECT COUNT(*) FROM task WHERE \"type\" = 0"),
        ("task.type1", "SELECT COUNT(*) FROM task WHERE \"type\" = 1"),
        ("task.type2", "SELECT COUNT(*) FROM task WHERE \"type\" = 2"),
        ("task.start0", "SELECT COUNT(*) FROM task WHERE start = 0"),
        ("task.start1", "SELECT COUNT(*) FROM task WHERE start = 1"),
        ("task.start2", "SELECT COUNT(*) FROM task WHERE start = 2"),
        (
            "task.empty_title",
            "SELECT COUNT(*) FROM task WHERE title = ''",
        ),
    ] {
        totals.insert(label.to_string(), count(conn, sql, [])?);
    }

    // Base set shared by every list: open todos with a title.
    let base = "\"type\" = 0 AND status = 0 AND trashed = 0 AND title != ''";

    let candidates_sql: [(&str, String); 20] = [
        (
            "today.st1_tir_le",
            format!(
                "SELECT COUNT(*) FROM task WHERE {base} AND start = 1 AND today_index_reference_date <= {today}"
            ),
        ),
        (
            "today.st1_tir_eq",
            format!(
                "SELECT COUNT(*) FROM task WHERE {base} AND start = 1 AND today_index_reference_date = {today}"
            ),
        ),
        (
            "today.st1_sr_le",
            format!(
                "SELECT COUNT(*) FROM task WHERE {base} AND start = 1 AND start_date <= {today}"
            ),
        ),
        (
            "today.st1_sr_eq",
            format!(
                "SELECT COUNT(*) FROM task WHERE {base} AND start = 1 AND start_date = {today}"
            ),
        ),
        (
            "today.st1_union",
            format!(
                "SELECT COUNT(*) FROM task WHERE {base} AND start = 1 AND ((today_index_reference_date <= {today}) OR (start_date <= {today}))"
            ),
        ),
        (
            "today.st1_no_dates",
            format!(
                "SELECT COUNT(*) FROM task WHERE {base} AND start = 1 AND start_date IS NULL AND today_index_reference_date IS NULL"
            ),
        ),
        (
            "anytime.st1_no_sr",
            format!("SELECT COUNT(*) FROM task WHERE {base} AND start = 1 AND start_date IS NULL"),
        ),
        (
            "anytime.st0_project_area",
            format!(
                "SELECT COUNT(*) FROM task WHERE {base} AND start = 0 AND (project IS NOT NULL OR area IS NOT NULL)"
            ),
        ),
        (
            "anytime.union",
            format!(
                "SELECT COUNT(*) FROM task WHERE {base} AND ((start = 1 AND start_date IS NULL) OR (start = 0 AND (project IS NOT NULL OR area IS NOT NULL)))"
            ),
        ),
        (
            "anytime.docs_variant",
            format!(
                "SELECT COUNT(*) FROM task WHERE {base} AND start = 0 AND (project IS NOT NULL OR area IS NOT NULL OR start_date <= {today})"
            ),
        ),
        (
            "someday.st2",
            format!("SELECT COUNT(*) FROM task WHERE {base} AND start = 2"),
        ),
        (
            "someday.st2_no_sr",
            format!("SELECT COUNT(*) FROM task WHERE {base} AND start = 2 AND start_date IS NULL"),
        ),
        (
            "upcoming.sr_gt",
            format!("SELECT COUNT(*) FROM task WHERE {base} AND start_date > {today}"),
        ),
        (
            "upcoming.st2_sr_gt",
            format!(
                "SELECT COUNT(*) FROM task WHERE {base} AND start = 2 AND start_date > {today}"
            ),
        ),
        (
            "inbox.current",
            format!(
                "SELECT COUNT(*) FROM task WHERE {base} AND start = 0 AND start_date IS NULL AND project IS NULL AND area IS NULL"
            ),
        ),
        (
            "inbox.start0_no_dates",
            format!("SELECT COUNT(*) FROM task WHERE {base} AND start = 0 AND start_date IS NULL"),
        ),
        (
            "logbook.status23_type_ne2",
            "SELECT COUNT(*) FROM task WHERE status IN (2, 3) AND \"type\" != 2 AND trashed = 0"
                .to_string(),
        ),
        (
            "logbook.status23",
            "SELECT COUNT(*) FROM task WHERE status IN (2, 3) AND trashed = 0".to_string(),
        ),
        (
            "st1.tir_set",
            format!(
                "SELECT COUNT(*) FROM task WHERE {base} AND start = 1 AND today_index_reference_date IS NOT NULL"
            ),
        ),
        (
            "st1.sr_set",
            format!(
                "SELECT COUNT(*) FROM task WHERE {base} AND start = 1 AND start_date IS NOT NULL"
            ),
        ),
    ];

    for (label, sql) in candidates_sql {
        candidates.insert(label.to_string(), count(conn, &sql, [])?);
    }

    Ok(Diagnostics { totals, candidates })
}

fn count(conn: &Connection, sql: &str, params: impl rusqlite::Params) -> Result<i64> {
    Ok(conn.query_row(sql, params, |row| row.get(0))?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::things::{ACTION_CREATE, IncomingItem};
    use serde_json::{Value, json};

    fn task(uuid: &str, props: Value) -> IncomingItem {
        IncomingItem {
            uuid: uuid.to_string(),
            action: ACTION_CREATE,
            kind: "Task7".into(),
            props: props.as_object().unwrap().clone(),
            server_index: 0,
        }
    }

    #[test]
    fn candidate_counts_distinguish_the_competing_predicates() {
        let mut store = crate::store::Store::open_in_memory().unwrap();
        let today = crate::fmt::today_epoch_day();

        store
            .apply_items(&[
                task("inbox", json!({"tt": "inbox", "tp": 0, "ss": 0, "st": 0})),
                task(
                    "today",
                    json!({"tt": "today", "tp": 0, "ss": 0, "st": 1,
                           "sr": today * 86_400, "tir": today * 86_400}),
                ),
                task(
                    "anytime",
                    json!({"tt": "anytime", "tp": 0, "ss": 0, "st": 1}),
                ),
                task(
                    "someday",
                    json!({"tt": "someday", "tp": 0, "ss": 0, "st": 2}),
                ),
                task(
                    "upcoming",
                    json!({"tt": "upcoming", "tp": 0, "ss": 0, "st": 2,
                           "sr": (today + 1) * 86_400}),
                ),
            ])
            .unwrap();

        let diagnostics = run(store.connection(), today).unwrap();

        assert_eq!(diagnostics.candidates["today.st1_tir_eq"], 1);
        assert_eq!(diagnostics.candidates["today.st1_sr_eq"], 1);
        assert_eq!(diagnostics.candidates["today.st1_union"], 1);
        assert_eq!(diagnostics.candidates["anytime.st1_no_sr"], 1);
        assert_eq!(diagnostics.candidates["someday.st2"], 2);
        assert_eq!(diagnostics.candidates["someday.st2_no_sr"], 1);
        assert_eq!(diagnostics.candidates["upcoming.sr_gt"], 1);
        assert_eq!(diagnostics.totals["task"], 5);
        assert_eq!(diagnostics.totals["task.start1"], 2);
    }
}
