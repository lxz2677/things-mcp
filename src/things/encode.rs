//! Wire encoders for the write path.
//!
//! The create payloads are *gold standard*: the official clients strictly
//! decode them and crash when a key is missing (three incidents upstream).
//! `validate_task_create` is the safety gate that refuses to push an incomplete
//! snapshot.
//!
//! Updates are **minimal deltas**: only the fields that changed, plus `md` when
//! the content/status changed (pure reordering must not send `md`).

use anyhow::{Context, Result, bail};
use chrono::{Datelike, NaiveDate};
use serde_json::{Map, Value, json};

use crate::fmt;
use crate::notes;

pub const TASK_KIND: &str = "Task7";
pub const CHECKLIST_KIND: &str = "ChecklistItem3";
pub const AREA_KIND: &str = "Area3";
pub const TAG_KIND: &str = "Tag4";

/// The 34 keys every Task7 create snapshot must contain.
pub const TASK_CREATE_KEYS: [&str; 34] = [
    "tt", "nt", "tp", "ss", "st", "sb", "ix", "ti", "tir", "md", "cd", "sp", "sr", "dd", "dds",
    "do", "tr", "pr", "agr", "ar", "tg", "rr", "rt", "lai", "icc", "icp", "icsd", "acrd", "ato",
    "lt", "rp", "rmd", "xx", "dl",
];

/// The 9 keys of a ChecklistItem3 create snapshot.
pub const CHECKLIST_CREATE_KEYS: [&str; 9] = ["tt", "ss", "ix", "ts", "lt", "cd", "sp", "md", "xx"];

const UNIX_EPOCH_CE: i64 = 719_163;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum When {
    Inbox,
    Today,
    Anytime,
    Someday,
    Evening,
    Date(i64),
}

impl When {
    /// Parse a `when` value: a keyword or `YYYY-MM-DD`.
    pub fn parse(value: &str) -> Result<Self> {
        let value = value.trim();
        match value {
            "inbox" => Ok(Self::Inbox),
            "today" => Ok(Self::Today),
            "anytime" => Ok(Self::Anytime),
            "someday" => Ok(Self::Someday),
            "evening" => Ok(Self::Evening),
            "tomorrow" => Ok(Self::Date(fmt::today_epoch_day() + 1)),
            other => {
                let date = NaiveDate::parse_from_str(other, "%Y-%m-%d")
                    .with_context(|| format!("invalid `when` value `{other}` (expected inbox|today|tomorrow|anytime|someday|evening|YYYY-MM-DD)"))?;
                Ok(Self::Date(date.num_days_from_ce() as i64 - UNIX_EPOCH_CE))
            }
        }
    }

    /// `(start, startDate, todayIndexReferenceDate, startBucket)`.
    pub fn fields(&self, today: i64) -> (i64, Option<i64>, Option<i64>, i64) {
        match self {
            Self::Inbox => (0, None, None, 0),
            Self::Today => (1, Some(today), Some(today), 0),
            Self::Anytime => (1, None, None, 0),
            Self::Someday => (2, None, None, 0),
            Self::Evening => (1, Some(today), Some(today), 1),
            Self::Date(day) => (2, Some(*day), Some(*day), 0),
        }
    }
}

/// Everything needed to build a create snapshot.
#[derive(Debug, Clone)]
pub struct TaskDraft {
    pub title: String,
    pub notes: String,
    pub when: When,
    pub deadline: Option<i64>,
    pub project: Option<String>,
    pub area: Option<String>,
    pub heading: Option<String>,
    pub tags: Vec<String>,
    /// `0` = todo, `1` = project.
    pub task_type: i64,
    /// Repeater rule object; encoded into both `rr` and `rp`.
    pub repeater: Option<Value>,
    pub index: i64,
    pub today_index: i64,
}

impl TaskDraft {
    pub fn new(title: impl Into<String>) -> Self {
        Self {
            title: title.into(),
            notes: String::new(),
            when: When::Inbox,
            deadline: None,
            project: None,
            area: None,
            heading: None,
            tags: Vec::new(),
            task_type: 0,
            repeater: None,
            index: 0,
            today_index: 0,
        }
    }

    pub fn project(title: impl Into<String>) -> Self {
        let mut draft = Self::new(title);
        draft.task_type = 1;
        draft.when = When::Anytime;
        draft
    }
}

/// Parse `YYYY-MM-DD` into an epoch day.
pub fn parse_day(value: &str) -> Result<i64> {
    let date = NaiveDate::parse_from_str(value.trim(), "%Y-%m-%d")
        .with_context(|| format!("invalid date `{value}` (expected YYYY-MM-DD)"))?;
    Ok(date.num_days_from_ce() as i64 - UNIX_EPOCH_CE)
}

/// `nt`: full-text note object with the CRC32 checksum of the UTF-8 bytes.
pub fn notes_object(text: &str) -> Value {
    json!({
        "_t": "tx",
        "ch": notes::checksum(text),
        "v": text,
        "t": 1,
    })
}

/// `xx`: the required extension object.
pub fn extension() -> Value {
    json!({"sn": {}, "_t": "oo"})
}

fn ids_vec(value: &Option<String>) -> Vec<String> {
    value.iter().cloned().collect()
}

/// Build the 34-key gold-standard Task7 create snapshot.
pub fn task_create_payload(draft: &TaskDraft, today: i64, now: f64) -> Value {
    let (st, sr, tir, sb) = draft.when.fields(today);

    json!({
        "tt": draft.title,
        "nt": notes_object(&draft.notes),
        "tp": draft.task_type,
        "ss": 0,
        "st": st,
        "sb": sb,
        "ix": draft.index,
        "ti": draft.today_index,
        "tir": tir.map(fmt::day_to_wire),
        "md": Value::Null,
        "cd": now,
        "sp": Value::Null,
        "sr": sr.map(fmt::day_to_wire),
        "dd": draft.deadline.map(fmt::day_to_wire),
        "dds": Value::Null,
        "do": 0,
        "tr": false,
        "pr": ids_vec(&draft.project),
        "agr": ids_vec(&draft.heading),
        "ar": ids_vec(&draft.area),
        "tg": draft.tags,
        "rr": draft.repeater.clone().unwrap_or(Value::Null),
        "rp": draft.repeater.clone().unwrap_or(Value::Null),
        "rt": [],
        "lai": Value::Null,
        "icc": 0,
        "icp": false,
        "icsd": Value::Null,
        "acrd": Value::Null,
        "ato": Value::Null,
        "lt": false,
        "rmd": Value::Null,
        "xx": extension(),
        "dl": [],
    })
}

/// ChecklistItem3 create snapshot (9 keys; `ts` is the parent task array).
pub fn checklist_create_payload(task_id: &str, title: &str, index: i64, now: f64) -> Value {
    json!({
        "tt": title,
        "ss": 0,
        "ix": index,
        "ts": [task_id],
        "lt": false,
        "cd": now,
        "sp": Value::Null,
        "md": now,
        "xx": extension(),
    })
}

/// Area3 create snapshot (4 keys, no `md`).
pub fn area_create_payload(title: &str, index: i64) -> Value {
    json!({
        "tt": title,
        "ix": index,
        "tg": [],
        "xx": extension(),
    })
}

/// Tag4 create snapshot (5 keys, no `md`, `pn` is an array).
pub fn tag_create_payload(
    title: &str,
    index: i64,
    parent: Option<&str>,
    shortcut: Option<&str>,
) -> Value {
    json!({
        "tt": title,
        "ix": index,
        "pn": parent.map(|id| vec![id]).unwrap_or_default(),
        "sh": shortcut,
        "xx": extension(),
    })
}

// ------------------------------------------------------------------- updates

pub fn complete_update(now: f64) -> Value {
    // The official client also sets `st = 1` when completing.
    json!({"ss": 3, "sp": now, "st": 1, "md": now})
}

pub fn cancel_update(now: f64) -> Value {
    json!({"ss": 2, "sp": now, "md": now})
}

pub fn reopen_update(now: f64) -> Value {
    json!({"ss": 0, "sp": Value::Null, "md": now})
}

pub fn title_update(title: &str, now: f64) -> Value {
    json!({"tt": title, "md": now})
}

pub fn notes_update(text: &str, now: f64) -> Value {
    json!({"nt": notes_object(text), "md": now})
}

pub fn when_update(when: &When, today: i64, now: f64) -> Value {
    let (st, sr, tir, sb) = when.fields(today);
    json!({
        "st": st,
        "sr": sr.map(fmt::day_to_wire),
        "tir": tir.map(fmt::day_to_wire),
        "sb": sb,
        "md": now,
    })
}

pub fn deadline_update(day: Option<i64>, now: f64) -> Value {
    json!({"dd": day.map(fmt::day_to_wire), "md": now})
}

pub fn container_update(
    project: &Option<String>,
    area: &Option<String>,
    heading: &Option<String>,
    now: f64,
) -> Value {
    json!({
        "pr": ids_vec(project),
        "ar": ids_vec(area),
        "agr": ids_vec(heading),
        "md": now,
    })
}

pub fn tags_update(tags: &[String], now: f64) -> Value {
    json!({"tg": tags, "md": now})
}

pub fn trash_update(trashed: bool, now: f64) -> Value {
    json!({"tr": trashed, "md": now})
}

/// Hard delete: the payload is always an empty object.
pub fn delete_envelope(uuid: &str, kind: &str) -> Value {
    json!({ uuid: {"t": 2, "e": kind, "p": {}} })
}

pub fn envelope(uuid: &str, action: i64, kind: &str, payload: Value) -> Value {
    json!({ uuid: {"t": action, "e": kind, "p": payload} })
}

// ------------------------------------------------------------- safety gates

/// Reject a Task7 create snapshot that is missing a gold-standard key or has a
/// value of the wrong type. A bad snapshot can crash the official Mac client.
pub fn validate_task_create(payload: &Value) -> Result<()> {
    let obj = payload
        .as_object()
        .context("task create payload must be an object")?;

    let missing: Vec<&str> = TASK_CREATE_KEYS
        .iter()
        .copied()
        .filter(|key| !obj.contains_key(*key))
        .collect();
    if !missing.is_empty() {
        bail!("refusing to push: Task7 create payload is missing gold-standard keys {missing:?}");
    }

    for key in ["tt"] {
        require_string(obj, key)?;
    }
    for key in ["tr", "lt", "icp"] {
        require_bool(obj, key)?;
    }
    for key in ["tg", "pr", "agr", "ar", "rt", "dl"] {
        require_array(obj, key)?;
    }
    for key in ["tp", "ss", "st", "sb", "ix", "ti", "do", "icc"] {
        require_i64(obj, key)?;
    }
    for key in ["nt", "xx"] {
        require_object(obj, key)?;
    }
    require_nullable_number(obj, "cd", false)?;
    for key in [
        "md", "sp", "lai", "sr", "dd", "dds", "tir", "icsd", "acrd", "rmd",
    ] {
        require_nullable_number(obj, key, true)?;
    }
    for key in ["rr", "rp"] {
        let value = &obj[key];
        if !(value.is_null() || value.is_object()) {
            bail!("{key} must be null or a recurrence object");
        }
    }
    if obj["nt"].get("v").and_then(Value::as_str).is_none() {
        bail!("nt must be a note object with a string `v`");
    }
    if obj["xx"].get("_t").and_then(Value::as_str) != Some("oo") {
        bail!("xx must be {{\"sn\":{{}},\"_t\":\"oo\"}}");
    }

    Ok(())
}

pub fn validate_checklist_create(payload: &Value) -> Result<()> {
    let obj = payload
        .as_object()
        .context("checklist create payload must be an object")?;
    let missing: Vec<&str> = CHECKLIST_CREATE_KEYS
        .iter()
        .copied()
        .filter(|key| !obj.contains_key(*key))
        .collect();
    if !missing.is_empty() {
        bail!("refusing to push: ChecklistItem3 create payload is missing {missing:?}");
    }
    require_string(obj, "tt")?;
    require_i64(obj, "ss")?;
    require_i64(obj, "ix")?;
    require_array(obj, "ts")?;
    require_bool(obj, "lt")?;
    require_object(obj, "xx")?;
    for key in ["cd", "sp", "md"] {
        require_nullable_number(obj, key, key != "cd")?;
    }
    Ok(())
}

pub fn validate_area_create(payload: &Value) -> Result<()> {
    let obj = payload
        .as_object()
        .context("area create payload must be an object")?;
    for key in ["tt", "ix", "tg", "xx"] {
        if !obj.contains_key(key) {
            bail!("refusing to push: Area3 create payload is missing `{key}`");
        }
    }
    require_string(obj, "tt")?;
    require_i64(obj, "ix")?;
    require_array(obj, "tg")?;
    require_object(obj, "xx")?;
    Ok(())
}

pub fn validate_tag_create(payload: &Value) -> Result<()> {
    let obj = payload
        .as_object()
        .context("tag create payload must be an object")?;
    for key in ["tt", "ix", "pn", "sh", "xx"] {
        if !obj.contains_key(key) {
            bail!("refusing to push: Tag4 create payload is missing `{key}`");
        }
    }
    require_string(obj, "tt")?;
    require_i64(obj, "ix")?;
    require_array(obj, "pn")?;
    if !(obj["sh"].is_null() || obj["sh"].is_string()) {
        bail!("sh must be a string or null");
    }
    require_object(obj, "xx")?;
    Ok(())
}

fn require_string(obj: &Map<String, Value>, key: &str) -> Result<()> {
    if !obj[key].is_string() {
        bail!("{key} must be a string");
    }
    Ok(())
}

fn require_bool(obj: &Map<String, Value>, key: &str) -> Result<()> {
    if !obj[key].is_boolean() {
        bail!("{key} must be a boolean");
    }
    Ok(())
}

fn require_i64(obj: &Map<String, Value>, key: &str) -> Result<()> {
    if obj[key].as_i64().is_none() {
        bail!("{key} must be an integer");
    }
    Ok(())
}

fn require_array(obj: &Map<String, Value>, key: &str) -> Result<()> {
    if !obj[key].is_array() {
        bail!("{key} must be an array");
    }
    Ok(())
}

fn require_object(obj: &Map<String, Value>, key: &str) -> Result<()> {
    if !obj[key].is_object() {
        bail!("{key} must be an object");
    }
    Ok(())
}

fn require_nullable_number(obj: &Map<String, Value>, key: &str, nullable: bool) -> Result<()> {
    let value = &obj[key];
    if value.is_null() && nullable {
        return Ok(());
    }
    if value.as_f64().is_none() {
        bail!(
            "{key} must be a number{}",
            if nullable { " or null" } else { "" }
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn today() -> i64 {
        fmt::today_epoch_day()
    }

    #[test]
    fn create_payload_has_exactly_the_gold_standard_keys() {
        let payload = task_create_payload(&TaskDraft::new("buy milk"), today(), 1_700_000_000.5);
        let obj = payload.as_object().unwrap();

        assert_eq!(obj.len(), 34, "Task7 create must have 34 keys");
        for key in TASK_CREATE_KEYS {
            assert!(obj.contains_key(key), "missing {key}");
        }
        validate_task_create(&payload).expect("payload must pass the safety gate");
    }

    #[test]
    fn when_keywords_map_to_the_documented_fields() {
        let now = 1_700_000_000.0;

        let mut draft = TaskDraft::new("today task");
        draft.when = When::Today;
        let payload = task_create_payload(&draft, today(), now);
        assert_eq!(payload["st"], 1);
        assert_eq!(payload["sr"], fmt::day_to_wire(today()));
        assert_eq!(payload["tir"], fmt::day_to_wire(today()));

        let mut draft = TaskDraft::new("someday task");
        draft.when = When::Someday;
        let payload = task_create_payload(&draft, today(), now);
        assert_eq!(payload["st"], 2);
        assert!(payload["sr"].is_null());

        let mut draft = TaskDraft::new("evening task");
        draft.when = When::Evening;
        let payload = task_create_payload(&draft, today(), now);
        assert_eq!(payload["st"], 1);
        assert_eq!(payload["sb"], 1);

        let mut draft = TaskDraft::new("dated task");
        draft.when = When::Date(today() + 10);
        let payload = task_create_payload(&draft, today(), now);
        assert_eq!(payload["st"], 2);
        assert_eq!(payload["sr"], fmt::day_to_wire(today() + 10));
    }

    #[test]
    fn tomorrow_is_a_relative_date() {
        let when = When::parse("tomorrow").unwrap();
        let today = fmt::today_epoch_day();
        assert_eq!(when.fields(today).1, Some(today + 1));
        assert_eq!(when.fields(today).0, 2);
    }

    #[test]
    fn parses_dates_and_rejects_garbage() {
        assert_eq!(When::parse("2026-12-01").unwrap().fields(0).1, {
            let date = NaiveDate::parse_from_str("2026-12-01", "%Y-%m-%d").unwrap();
            Some(date.num_days_from_ce() as i64 - UNIX_EPOCH_CE)
        });
        assert!(When::parse("next tuesday").is_err());
    }

    #[test]
    fn notes_carry_the_crc32_of_utf8_bytes() {
        let text = "Mac端捕获测试\n第二行";
        let note = notes_object(text);
        assert_eq!(note["ch"], 75990805u64);
        assert_eq!(note["v"], text);
        assert_eq!(note["_t"], "tx");
        assert_eq!(note["t"], 1);
    }

    #[test]
    fn safety_gate_rejects_a_missing_key() {
        let mut payload = task_create_payload(&TaskDraft::new("x"), today(), 1.0);
        payload.as_object_mut().unwrap().remove("dl");
        let error = validate_task_create(&payload).unwrap_err();
        assert!(error.to_string().contains("missing"), "{error}");

        let mut payload = task_create_payload(&TaskDraft::new("x"), today(), 1.0);
        payload["tr"] = json!("yes");
        assert!(validate_task_create(&payload).is_err());
    }

    #[test]
    fn update_deltas_are_minimal() {
        assert_eq!(
            complete_update(100.5),
            json!({"ss": 3, "sp": 100.5, "st": 1, "md": 100.5})
        );
        assert_eq!(
            cancel_update(100.5),
            json!({"ss": 2, "sp": 100.5, "md": 100.5})
        );
        assert_eq!(
            title_update("new", 100.5),
            json!({"tt": "new", "md": 100.5})
        );
        assert_eq!(trash_update(true, 100.5), json!({"tr": true, "md": 100.5}));
    }

    #[test]
    fn delete_uses_an_empty_payload() {
        let envelope = delete_envelope("abc", TASK_KIND);
        assert_eq!(envelope["abc"]["t"], 2);
        assert_eq!(envelope["abc"]["e"], "Task7");
        assert_eq!(envelope["abc"]["p"], json!({}));
    }

    #[test]
    fn area_and_tag_snapshots_match_the_captures() {
        let area = area_create_payload("Work", 0);
        assert_eq!(area.as_object().unwrap().len(), 4);
        assert_eq!(area["tg"], json!([]));
        assert_eq!(area["xx"], json!({"sn": {}, "_t": "oo"}));

        let tag = tag_create_payload("urgent", 0, None, None);
        assert_eq!(tag.as_object().unwrap().len(), 5);
        assert_eq!(tag["pn"], json!([]));
        assert!(tag["sh"].is_null());

        let child = tag_create_payload("child", 0, Some("parent-id"), Some("c"));
        assert_eq!(child["pn"], json!(["parent-id"]));
        assert_eq!(child["sh"], "c");
    }

    #[test]
    fn checklist_payload_has_nine_keys() {
        let payload = checklist_create_payload("task-id", "step", 0, 100.0);
        assert_eq!(payload.as_object().unwrap().len(), 9);
        assert_eq!(payload["ts"], json!(["task-id"]));
        validate_checklist_create(&payload).unwrap();
    }
}
