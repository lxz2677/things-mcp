//! Wire codec: apply abbreviated wire properties to local entities.
//!
//! The abbreviations are documented in `docs/03-sync-engine.md` §5. All keys are
//! applied defensively: a missing key means "not changed", an explicit `null`
//! means "clear this value", and unknown keys are preserved in `raw`.

use serde_json::{Map, Value};

use crate::notes;

use super::model::{Area, ChecklistItem, Tag, Task};

/// Day-grained wire values are Unix seconds of UTC midnight; convert to epoch days.
pub fn day_from_wire(value: &Value) -> Option<i64> {
    let secs = number(value)?;
    Some((secs / 86_400.0).floor() as i64)
}

fn number(value: &Value) -> Option<f64> {
    match value {
        Value::Number(n) => n.as_f64(),
        _ => None,
    }
}

fn int(value: &Value) -> Option<i64> {
    match value {
        Value::Number(n) => n.as_i64().or_else(|| n.as_f64().map(|f| f as i64)),
        _ => None,
    }
}

pub(crate) fn reminder_offset(value: &Value) -> Option<i64> {
    value
        .as_i64()
        .filter(|seconds| (0..86_400).contains(seconds))
}

fn boolean(value: &Value) -> Option<bool> {
    match value {
        Value::Bool(b) => Some(*b),
        Value::Number(n) => n.as_i64().map(|i| i != 0),
        _ => None,
    }
}

fn ids(value: &Value) -> Option<Vec<String>> {
    match value {
        Value::Null => Some(Vec::new()),
        Value::Array(items) => Some(
            items
                .iter()
                .filter_map(|v| v.as_str().map(ToOwned::to_owned))
                .collect(),
        ),
        Value::String(s) => Some(vec![s.clone()]),
        _ => None,
    }
}

/// `pr` / `ar` / `agr` are arrays in the canonical snapshot but strings in some
/// historical payloads; accept both and take the first value.
fn first_id(value: &Value) -> Option<String> {
    match value {
        Value::Null => None,
        Value::String(s) => Some(s.clone()),
        Value::Array(items) => items.iter().find_map(|v| v.as_str().map(ToOwned::to_owned)),
        _ => None,
    }
}

fn opt_int(props: &Map<String, Value>, key: &str) -> Option<i64> {
    props.get(key).and_then(int)
}

fn opt_bool(props: &Map<String, Value>, key: &str) -> Option<bool> {
    props.get(key).and_then(boolean)
}

fn opt_instant(props: &Map<String, Value>, key: &str) -> Option<Option<f64>> {
    props.get(key).map(|v| match v {
        Value::Null => None,
        other => number(other),
    })
}

fn opt_day(props: &Map<String, Value>, key: &str) -> Option<Option<i64>> {
    props.get(key).map(|v| match v {
        Value::Null => None,
        other => day_from_wire(other),
    })
}

fn opt_id(props: &Map<String, Value>, key: &str) -> Option<Option<String>> {
    props
        .get(key)
        .map(|v| if v.is_null() { None } else { first_id(v) })
}

fn opt_ids(props: &Map<String, Value>, key: &str) -> Option<Vec<String>> {
    props.get(key).and_then(ids)
}

/// Keep the last-seen wire value of every key so `get_task` can expose fields
/// we do not model yet.
fn merge_raw(raw: &mut Map<String, Value>, props: &Map<String, Value>) {
    for (key, value) in props {
        raw.insert(key.clone(), value.clone());
    }
}

pub fn apply_task(task: &mut Task, props: &Map<String, Value>) {
    merge_raw(&mut task.raw, props);

    if let Some(value) = props.get("tt").and_then(Value::as_str) {
        task.title = value.to_string();
    }
    if let Some(value) = props.get("nt") {
        if let Some(text) = notes::decode(value, &task.notes) {
            task.notes = text;
        } else {
            tracing::warn!(task = %task.id, "could not decode notes update; keeping previous text");
        }
    }

    if let Some(value) = opt_int(props, "ss") {
        task.status = value;
    }
    if let Some(value) = opt_int(props, "st") {
        task.start = value;
    }
    if let Some(value) = opt_int(props, "sb") {
        task.start_bucket = value;
    }
    if let Some(value) = opt_int(props, "tp") {
        task.type_ = value;
    }
    if let Some(value) = opt_bool(props, "tr") {
        task.trashed = value;
    }
    if let Some(value) = opt_bool(props, "lt") {
        task.leaves_tombstone = value;
    }
    if let Some(value) = opt_bool(props, "icp") {
        task.instance_creation_paused = value;
    }
    if let Some(value) = opt_int(props, "icc") {
        task.instance_creation_count = value;
    }
    if let Some(value) = opt_int(props, "do") {
        task.deadline_offset = value;
    }
    if let Some(value) = opt_int(props, "ix") {
        task.index = Some(value);
    }
    if let Some(value) = opt_int(props, "ti") {
        task.today_index = Some(value);
    }

    if let Some(value) = opt_instant(props, "cd") {
        task.creation_date = value;
    }
    if let Some(value) = opt_instant(props, "md") {
        task.user_modification_date = value;
    }
    if let Some(value) = opt_instant(props, "sp") {
        task.stop_date = value;
    }
    if let Some(value) = opt_instant(props, "lai") {
        task.last_reminder_interaction_date = value;
    }
    // `ato` is the reminder time as seconds since local midnight (the upstream
    // docs mis-label it as afterCompletionReference; `acrd` is that field).
    if props.contains_key("ato") {
        task.reminder_time = props.get("ato").and_then(reminder_offset);
    }

    if let Some(value) = opt_day(props, "sr") {
        task.start_date = value;
    }
    if let Some(value) = opt_day(props, "dd") {
        task.deadline = value;
    }
    if let Some(value) = opt_day(props, "dds") {
        task.deadline_suppression_date = value;
    }
    if let Some(value) = opt_day(props, "tir") {
        task.today_index_reference_date = value;
    }
    if let Some(value) = opt_day(props, "icsd") {
        task.instance_creation_start_date = value;
    }
    if let Some(value) = opt_day(props, "acrd") {
        task.after_completion_reference_date = value;
    }

    if let Some(value) = opt_id(props, "pr") {
        task.project = value;
    }
    if let Some(value) = opt_id(props, "ar") {
        task.area = value;
    }
    if let Some(value) = opt_id(props, "agr") {
        task.heading = value;
    }
    if let Some(value) = opt_ids(props, "tg") {
        task.tags = value;
    }
    if let Some(value) = opt_id(props, "rt") {
        task.repeating_template = value;
    }

    // Prefer the legacy rule in `rr`. Keep `rp` as a fallback for incoming
    // payloads that omit `rr`.
    if props.contains_key("rr") {
        task.recurrence_rule = props
            .get("rr")
            .filter(|v| !v.is_null())
            .map(|v| v.to_string());
    } else if props.contains_key("rp") {
        task.recurrence_rule = props
            .get("rp")
            .filter(|v| !v.is_null())
            .map(|v| v.to_string());
    }
    if let Some(value) = props.get("xx") {
        task.experimental = Some(value.clone());
    }
}

pub fn apply_area(area: &mut Area, props: &Map<String, Value>) {
    merge_raw(&mut area.raw, props);
    if let Some(value) = props.get("tt").and_then(Value::as_str) {
        area.title = value.to_string();
    }
    if let Some(value) = opt_int(props, "ix") {
        area.index = Some(value);
    }
    if let Some(value) = opt_ids(props, "tg") {
        area.tags = value;
    }
}

pub fn apply_tag(tag: &mut Tag, props: &Map<String, Value>) {
    merge_raw(&mut tag.raw, props);
    if let Some(value) = props.get("tt").and_then(Value::as_str) {
        tag.title = value.to_string();
    }
    if let Some(value) = props.get("sh") {
        tag.shortcut = value.as_str().map(ToOwned::to_owned);
    }
    if let Some(value) = props.get("pn") {
        tag.parent = first_id(value);
    }
    if let Some(value) = opt_int(props, "ix") {
        tag.index = Some(value);
    }
}

pub fn apply_checklist_item(item: &mut ChecklistItem, props: &Map<String, Value>) {
    merge_raw(&mut item.raw, props);
    if let Some(value) = props.get("tt").and_then(Value::as_str) {
        item.title = value.to_string();
    }
    if let Some(value) = opt_int(props, "ss") {
        item.status = value;
    }
    if let Some(value) = opt_int(props, "ix") {
        item.index = Some(value);
    }
    // On checklist items `ts` is the parent task reference (an array).
    if let Some(value) = props.get("ts") {
        item.task = first_id(value);
    }
    if let Some(value) = opt_bool(props, "lt") {
        item.leaves_tombstone = value;
    }
    if let Some(value) = opt_instant(props, "cd") {
        item.creation_date = value;
    }
    if let Some(value) = opt_instant(props, "md") {
        item.user_modification_date = value;
    }
    if let Some(value) = opt_instant(props, "sp") {
        item.stop_date = value;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn props(value: serde_json::Value) -> Map<String, Value> {
        value.as_object().unwrap().clone()
    }

    #[test]
    fn decodes_task7_snapshot() {
        let wire = json!({
            "tt": "买牛奶", "nt": {"_t":"tx","ch":0,"v":"两盒","t":1},
            "tp": 0, "ss": 3, "st": 1, "sb": 0,
            "ix": -381, "ti": -12, "tir": 1792368000,
            "md": 1495249071.2812, "cd": 1495000000.5, "sp": 1495249800.25,
            "sr": 1792368000, "dd": null, "dds": null, "do": 0,
            "tr": false, "pr": [], "agr": [], "ar": [],
            "tg": ["EhnXjpfU1sYL3gp9t3Z4Um"],
            "rr": null, "rp": null, "rt": [], "lai": null,
            "icc": 0, "icp": false, "icsd": null, "acrd": null,
            "lt": false, "rmd": null, "dl": [], "xx": {"sn":{},"_t":"oo"}
        });

        let mut task = Task::default();
        apply_task(&mut task, &props(wire));

        assert_eq!(task.title, "买牛奶");
        assert_eq!(task.notes, "两盒");
        assert_eq!(task.status, 3);
        assert_eq!(task.start, 1);
        assert_eq!(task.start_date, Some(1792368000 / 86_400));
        assert_eq!(task.index, Some(-381));
        assert_eq!(task.today_index, Some(-12));
        assert_eq!(task.tags, vec!["EhnXjpfU1sYL3gp9t3Z4Um".to_string()]);
        assert_eq!(task.project, None);
        assert_eq!(task.creation_date, Some(1495000000.5));
        assert_eq!(task.raw.get("tt").and_then(Value::as_str), Some("买牛奶"));
    }

    #[test]
    fn project_reference_accepts_array_and_string() {
        let mut task = Task::default();
        apply_task(&mut task, &props(json!({"pr": ["proj-id"]})));
        assert_eq!(task.project.as_deref(), Some("proj-id"));

        apply_task(&mut task, &props(json!({"pr": "other-id"})));
        assert_eq!(task.project.as_deref(), Some("other-id"));

        apply_task(&mut task, &props(json!({"pr": []})));
        assert_eq!(task.project, None);
    }

    #[test]
    fn null_clears_date() {
        let mut task = Task::default();
        apply_task(&mut task, &props(json!({"sr": 1792368000})));
        assert!(task.start_date.is_some());
        apply_task(&mut task, &props(json!({"sr": null})));
        assert_eq!(task.start_date, None);
    }

    #[test]
    fn decodes_and_clears_reminder_time() {
        let mut task = Task::default();
        apply_task(&mut task, &props(json!({"ato": 56700})));
        assert_eq!(task.reminder_time, Some(56_700));

        apply_task(&mut task, &props(json!({"ato": null})));
        assert_eq!(task.reminder_time, None);
    }

    #[test]
    fn repeater_is_kept_verbatim() {
        let rule = json!({"ia":0,"rrv":4,"fu":256,"ts":0,"sr":1792195200,"rc":0,"fa":1,"tp":1,"of":[],"ed":64092211200i64});
        let mut task = Task::default();
        apply_task(&mut task, &props(json!({"rr": rule})));
        let stored: Value = serde_json::from_str(task.recurrence_rule.as_deref().unwrap()).unwrap();
        assert_eq!(stored, rule);
    }
}
