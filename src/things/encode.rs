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
use schemars::JsonSchema;
use serde::Deserialize;
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

    /// The concrete calendar day for this `when`, if it has one.
    pub fn day(&self, today: i64) -> Option<i64> {
        match self {
            Self::Today | Self::Evening => Some(today),
            Self::Date(day) => Some(*day),
            Self::Inbox | Self::Anytime | Self::Someday => None,
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
    /// Reminder time as seconds since local midnight (wire `ato`).
    pub reminder_time: Option<i64>,
    /// Legacy recurrence rule object for `rr`.
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
            reminder_time: None,
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

/// Parse `HH:MM` (or `HH:MM:SS`) into seconds since local midnight.
pub fn parse_reminder(value: &str) -> Result<i64> {
    let parts: Vec<&str> = value.trim().split(':').collect();
    if parts.len() < 2 || parts.len() > 3 {
        bail!("invalid reminder time `{value}` (expected HH:MM)");
    }
    let hour: i64 = parts[0]
        .parse()
        .with_context(|| format!("invalid reminder hour in `{value}`"))?;
    let minute: i64 = parts[1]
        .parse()
        .with_context(|| format!("invalid reminder minute in `{value}`"))?;
    let second: i64 = if parts.len() == 3 {
        parts[2]
            .parse()
            .with_context(|| format!("invalid reminder second in `{value}`"))?
    } else {
        0
    };
    if !(0..24).contains(&hour) || !(0..60).contains(&minute) || !(0..60).contains(&second) {
        bail!("invalid reminder time `{value}` (out of range)");
    }
    Ok(hour * 3600 + minute * 60 + second)
}

/// Format seconds since local midnight as `HH:MM`.
pub fn format_reminder(seconds: i64) -> String {
    format!("{:02}:{:02}", seconds / 3600, (seconds % 3600) / 60)
}

/// Parse `YYYY-MM-DD` into an epoch day.
pub fn parse_day(value: &str) -> Result<i64> {
    let date = NaiveDate::parse_from_str(value.trim(), "%Y-%m-%d")
        .with_context(|| format!("invalid date `{value}` (expected YYYY-MM-DD)"))?;
    Ok(date.num_days_from_ce() as i64 - UNIX_EPOCH_CE)
}

// ---------------------------------------------------------------- repeating

/// `ed` sentinel for "never ends" (4001-01-01).
const NEVER_ENDS: i64 = 64_092_211_200;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum RepeatType {
    #[default]
    Scheduled,
    AfterCompletion,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Frequency {
    Daily,
    Weekly,
    Monthly,
    Yearly,
}

fn default_interval() -> u32 {
    1
}

/// User-facing recurrence description, mapped to the wire `rr` object.
#[derive(Debug, Clone, serde::Serialize, Deserialize, JsonSchema)]
pub struct RepeatSpec {
    #[serde(rename = "type", default)]
    pub repeat_type: RepeatType,
    pub frequency: Frequency,
    /// Every N days/weeks/months/years (wire `fa`, default 1).
    #[serde(default = "default_interval")]
    pub interval: u32,
    /// Weekly: `mon`, `wed`, ... (defaults to the start day's weekday).
    #[serde(default)]
    pub weekdays: Vec<String>,
    /// Monthly / yearly day: 1-31, or -1 for the last day of the month.
    pub day: Option<i32>,
    /// Yearly month: 1-12.
    pub month: Option<u32>,
    /// `YYYY-MM-DD`; omitted means the rule never ends.
    pub end_date: Option<String>,
}

impl RepeatSpec {
    /// Build the wire rule object for a task starting at `start_day`.
    pub fn build(&self, start_day: i64) -> Result<Value> {
        if !(1..=999).contains(&self.interval) {
            bail!("repeat interval must be between 1 and 999");
        }
        let start = fmt::epoch_day_to_date(start_day).context("invalid repeat start day")?;

        let (fu, of, first_day) = match self.frequency {
            Frequency::Daily => (16, json!([{"dy": 0}]), start_day),
            Frequency::Weekly => {
                let mut weekdays = if self.weekdays.is_empty() {
                    vec![weekday_number(start.weekday())]
                } else {
                    self.weekdays
                        .iter()
                        .map(|name| parse_weekday(name))
                        .collect::<Result<Vec<_>>>()?
                };
                weekdays.sort_unstable();
                weekdays.dedup();
                let first = first_weekly(start, &weekdays);
                let offsets: Vec<Value> = weekdays.iter().map(|wd| json!({"wd": wd})).collect();
                (256, Value::Array(offsets), first)
            }
            Frequency::Monthly => {
                let day = self.day.unwrap_or(start.day() as i32);
                if !matches!(day, -1 | 1..=31) {
                    bail!("monthly repeat day must be 1-31 or -1 (got {day})");
                }
                let first = first_monthly(start, day)?;
                (8, json!([{"dy": wire_day(day)}]), first)
            }
            Frequency::Yearly => {
                let month = self.month.unwrap_or(start.month());
                let day = self.day.unwrap_or(start.day() as i32);
                if !(1..=12).contains(&month) {
                    bail!("yearly repeat month must be 1-12 (got {month})");
                }
                if !matches!(day, -1 | 1..=31) {
                    bail!("yearly repeat day must be 1-31 or -1 (got {day})");
                }
                let first = first_yearly(start, month, day)?;
                (4, json!([{"mo": month - 1, "dy": wire_day(day)}]), first)
            }
        };

        let end = match &self.end_date {
            Some(value) => {
                let end = parse_day(value)?;
                let first_possible_day = match self.repeat_type {
                    RepeatType::Scheduled => first_day,
                    RepeatType::AfterCompletion => start_day,
                };
                anyhow::ensure!(
                    end >= first_possible_day,
                    "repeat end date is before the first possible occurrence"
                );
                fmt::day_to_wire(end)
            }
            None => NEVER_ENDS,
        };
        let (tp, ia, of) = match self.repeat_type {
            RepeatType::Scheduled => (0, fmt::day_to_wire(first_day), of),
            // Completed-based repeats have no fixed first occurrence.
            RepeatType::AfterCompletion => (1, 0, json!([])),
        };

        let rule = json!({
            "ia": ia,
            "rrv": 4,
            "fu": fu,
            "ts": 0,
            "sr": fmt::day_to_wire(start_day),
            "rc": 0,
            "fa": self.interval,
            "tp": tp,
            "of": of,
            "ed": end,
        });
        validate_repeater(&rule)?;
        Ok(rule)
    }

    /// Parse a compact CLI spec:
    /// `daily`, `weekly`, `weekly:mon,wed`, `monthly:15`, `monthly:last`,
    /// `yearly:12-25`, optionally prefixed with `after-completion:`.
    pub fn parse(spec: &str, interval: u32, end_date: Option<String>) -> Result<Self> {
        let spec = spec.trim().to_ascii_lowercase();
        let (repeat_type, spec) = match spec.strip_prefix("after-completion:") {
            Some(rest) => (RepeatType::AfterCompletion, rest),
            None => (RepeatType::Scheduled, spec.as_str()),
        };
        let (frequency, detail) = match spec.split_once(':') {
            Some((frequency, detail)) => (frequency, Some(detail)),
            None => (spec, None),
        };

        let mut parsed = Self {
            repeat_type,
            frequency: Frequency::Daily,
            interval,
            weekdays: Vec::new(),
            day: None,
            month: None,
            end_date,
        };

        match frequency {
            "daily" | "every-day" => parsed.frequency = Frequency::Daily,
            "weekly" | "every-week" => {
                parsed.frequency = Frequency::Weekly;
                if let Some(detail) = detail {
                    if detail.trim().is_empty() {
                        bail!("weekly repeat needs at least one weekday");
                    }
                    parsed.weekdays = detail
                        .split(',')
                        .map(|name| name.trim().to_string())
                        .collect();
                }
            }
            "monthly" => {
                parsed.frequency = Frequency::Monthly;
                if let Some(detail) = detail {
                    parsed.day = Some(if detail == "last" {
                        -1
                    } else {
                        detail
                            .parse()
                            .with_context(|| format!("invalid monthly day `{detail}`"))?
                    });
                }
            }
            "yearly" => {
                parsed.frequency = Frequency::Yearly;
                let detail = detail.context("yearly repeat requires `yearly:MONTH-DAY`")?;
                let (month, day) = detail
                    .split_once('-')
                    .context("yearly repeat requires `yearly:MONTH-DAY`")?;
                parsed.month = Some(
                    month
                        .parse()
                        .with_context(|| format!("invalid yearly month `{month}`"))?,
                );
                parsed.day = Some(if day == "last" {
                    -1
                } else {
                    day.parse()
                        .with_context(|| format!("invalid yearly day `{day}`"))?
                });
            }
            other => bail!("unsupported repeat `{other}` (expected daily|weekly|monthly|yearly)"),
        }
        Ok(parsed)
    }

    /// First calendar day matching the rule, on or after `from_day`.
    pub fn first_occurrence(&self, from_day: i64) -> Result<i64> {
        let from = fmt::epoch_day_to_date(from_day).context("invalid repeat start day")?;
        let day = match self.frequency {
            Frequency::Daily => from_day,
            Frequency::Weekly => {
                let weekdays = if self.weekdays.is_empty() {
                    vec![weekday_number(from.weekday())]
                } else {
                    self.weekdays
                        .iter()
                        .map(|name| parse_weekday(name))
                        .collect::<Result<Vec<_>>>()?
                };
                first_weekly(from, &weekdays)
            }
            Frequency::Monthly => first_monthly(from, self.day.unwrap_or(from.day() as i32))?,
            Frequency::Yearly => first_yearly(
                from,
                self.month.unwrap_or(from.month()),
                self.day.unwrap_or(from.day() as i32),
            )?,
        };
        Ok(day)
    }
}

/// Ensure a repeating task has a concrete start day, adjusting `when` if needed.
pub fn prepare_repeat(when: &mut When, spec: &RepeatSpec, today: i64) -> Result<i64> {
    // After-completion repeats start with the task itself, so an undated task
    // starts today. Scheduled repeats jump to their first matching occurrence.
    if spec.repeat_type == RepeatType::AfterCompletion {
        return match when {
            When::Today | When::Evening => Ok(today),
            When::Date(day) => Ok(*day),
            _ => {
                *when = When::Date(today);
                Ok(today)
            }
        };
    }

    match when {
        When::Today | When::Evening => Ok(today),
        When::Date(day) => Ok(*day),
        _ => {
            let first = spec.first_occurrence(today)?;
            *when = When::Date(first);
            Ok(first)
        }
    }
}

fn parse_weekday(name: &str) -> Result<u32> {
    match name.trim().to_ascii_lowercase().as_str() {
        "sun" | "sunday" => Ok(0),
        "mon" | "monday" => Ok(1),
        "tue" | "tues" | "tuesday" => Ok(2),
        "wed" | "wednesday" => Ok(3),
        "thu" | "thur" | "thurs" | "thursday" => Ok(4),
        "fri" | "friday" => Ok(5),
        "sat" | "saturday" => Ok(6),
        other => bail!("invalid weekday `{other}` (expected sun..sat)"),
    }
}

fn weekday_number(weekday: chrono::Weekday) -> u32 {
    weekday.num_days_from_sunday()
}

fn wire_day(day: i32) -> i32 {
    if day == -1 { -1 } else { day - 1 }
}

fn epoch_day(date: NaiveDate) -> i64 {
    date.num_days_from_ce() as i64 - 719_163
}

fn first_weekly(start: NaiveDate, weekdays: &[u32]) -> i64 {
    let current = weekday_number(start.weekday());
    let delta = weekdays
        .iter()
        .map(|wd| (wd + 7 - current) % 7)
        .min()
        .unwrap_or(0);
    epoch_day(start + chrono::Duration::days(delta as i64))
}

fn add_months(year: i32, month: u32, offset: i32) -> (i32, u32) {
    let total = year * 12 + (month as i32 - 1) + offset;
    (total.div_euclid(12), (total.rem_euclid(12) + 1) as u32)
}

fn last_day_of_month(year: i32, month: u32) -> Option<NaiveDate> {
    let (next_year, next_month) = add_months(year, month, 1);
    NaiveDate::from_ymd_opt(next_year, next_month, 1).map(|date| date - chrono::Duration::days(1))
}

fn first_monthly(start: NaiveDate, day: i32) -> Result<i64> {
    for offset in 0..=24 {
        let (year, month) = add_months(start.year(), start.month(), offset);
        let candidate = if day == -1 {
            last_day_of_month(year, month)
        } else {
            NaiveDate::from_ymd_opt(year, month, day as u32)
        };
        if let Some(date) = candidate
            && date >= start
        {
            return Ok(epoch_day(date));
        }
    }
    bail!("could not compute the first monthly occurrence")
}

fn first_yearly(start: NaiveDate, month: u32, day: i32) -> Result<i64> {
    for offset in 0..=8 {
        let year = start.year() + offset;
        let candidate = if day == -1 {
            last_day_of_month(year, month)
        } else {
            NaiveDate::from_ymd_opt(year, month, day as u32)
        };
        if let Some(date) = candidate
            && date >= start
        {
            return Ok(epoch_day(date));
        }
    }
    bail!("could not compute the first yearly occurrence")
}

/// Validate a wire `rr` object before it can be pushed.
pub fn validate_repeater(rule: &Value) -> Result<()> {
    let obj = rule.as_object().context("repeater must be an object")?;
    for key in ["ia", "rrv", "fu", "ts", "sr", "rc", "fa", "tp", "of", "ed"] {
        if !obj.contains_key(key) {
            bail!("repeater is missing `{key}`");
        }
    }
    for key in ["ia", "rrv", "fu", "ts", "sr", "rc", "fa", "tp", "ed"] {
        if obj[key].as_i64().is_none() {
            bail!("repeater `{key}` must be an integer");
        }
    }
    if obj["rrv"].as_i64() != Some(4) {
        bail!("unsupported repeater version (expected rrv=4)");
    }
    if !matches!(obj["fu"].as_i64(), Some(4 | 8 | 16 | 256)) {
        bail!("unsupported repeater frequency unit `{}`", obj["fu"]);
    }
    if !matches!(obj["tp"].as_i64(), Some(0 | 1)) {
        bail!("repeater type must be 0 (scheduled) or 1 (after completion)");
    }
    let amplitude = obj["fa"].as_i64().unwrap_or(0);
    if !(1..=999).contains(&amplitude) {
        bail!("repeater interval must be between 1 and 999");
    }
    let offsets = obj["of"]
        .as_array()
        .context("repeater `of` must be an array")?;
    for offset in offsets {
        let offset = offset
            .as_object()
            .context("repeater offsets must be objects")?;
        for key in offset.keys() {
            if !matches!(key.as_str(), "dy" | "wd" | "mo" | "wdo") {
                bail!("unsupported repeater offset key `{key}`");
            }
        }
    }
    Ok(())
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
        "rp": Value::Null,
        "rt": [],
        "lai": Value::Null,
        "icc": 0,
        "icp": false,
        "icsd": Value::Null,
        "acrd": Value::Null,
        "ato": draft.reminder_time,
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
    anyhow::ensure!(
        obj["rr"].is_null() || obj["rr"].is_object(),
        "rr must be null or a recurrence object"
    );
    anyhow::ensure!(obj["rp"].is_null(), "rp must be null in Task7 creates");
    if !obj["ato"].is_null() {
        anyhow::ensure!(
            super::wire::reminder_offset(&obj["ato"]).is_some(),
            "reminder time must be a whole number of seconds between 0 and 86399"
        );
        anyhow::ensure!(
            !obj["sr"].is_null() || !obj["rr"].is_null(),
            "a reminder requires a start date; set when to today, tomorrow, evening, or YYYY-MM-DD"
        );
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
    fn repeat_rules_match_the_wire_format() {
        let start = parse_day("2026-11-08").unwrap(); // Sunday
        let s = fmt::day_to_wire(start);

        let daily = RepeatSpec {
            repeat_type: RepeatType::Scheduled,
            frequency: Frequency::Daily,
            interval: 1,
            weekdays: Vec::new(),
            day: None,
            month: None,
            end_date: None,
        }
        .build(start)
        .unwrap();
        assert_eq!(daily["fu"], 16);
        assert_eq!(daily["tp"], 0);
        assert_eq!(daily["sr"], s);
        assert_eq!(daily["ia"], s);
        assert_eq!(daily["fa"], 1);
        assert_eq!(daily["of"], json!([{"dy": 0}]));
        assert_eq!(daily["ed"], 64_092_211_200i64);

        let weekly = RepeatSpec {
            frequency: Frequency::Weekly,
            weekdays: vec!["mon".into(), "wed".into()],
            ..daily_spec()
        }
        .build(start)
        .unwrap();
        assert_eq!(weekly["fu"], 256);
        assert_eq!(weekly["of"], json!([{"wd": 1}, {"wd": 3}]));
        assert_eq!(weekly["ia"], fmt::day_to_wire(start + 1)); // next Monday

        let monthly = RepeatSpec {
            frequency: Frequency::Monthly,
            day: Some(15),
            ..daily_spec()
        }
        .build(start)
        .unwrap();
        assert_eq!(monthly["fu"], 8);
        assert_eq!(monthly["of"], json!([{"dy": 14}]));
        assert_eq!(monthly["ia"], fmt::day_to_wire(start + 7));

        let last = RepeatSpec {
            frequency: Frequency::Monthly,
            day: Some(-1),
            ..daily_spec()
        }
        .build(start)
        .unwrap();
        assert_eq!(last["of"], json!([{"dy": -1}]));
        assert_eq!(last["ia"], fmt::day_to_wire(start + 22)); // 2026-11-30

        let yearly = RepeatSpec {
            frequency: Frequency::Yearly,
            month: Some(12),
            day: Some(25),
            ..daily_spec()
        }
        .build(start)
        .unwrap();
        assert_eq!(yearly["fu"], 4);
        assert_eq!(yearly["of"], json!([{"mo": 11, "dy": 24}]));
        assert_eq!(
            yearly["ia"],
            fmt::day_to_wire(parse_day("2026-12-25").unwrap())
        );

        let after = RepeatSpec {
            repeat_type: RepeatType::AfterCompletion,
            frequency: Frequency::Weekly,
            interval: 2,
            end_date: Some("2027-01-01".into()),
            ..daily_spec()
        }
        .build(start)
        .unwrap();
        assert_eq!(after["tp"], 1);
        assert_eq!(after["ia"], 0);
        assert_eq!(after["of"], json!([]));
        assert_eq!(after["fa"], 2);
        assert_eq!(
            after["ed"],
            fmt::day_to_wire(parse_day("2027-01-01").unwrap())
        );
    }

    #[test]
    fn repeat_spec_parses_compact_cli_syntax() {
        let spec = RepeatSpec::parse("weekly:mon,wed", 1, None).unwrap();
        assert_eq!(spec.frequency, Frequency::Weekly);
        assert_eq!(spec.weekdays, vec!["mon", "wed"]);

        let spec = RepeatSpec::parse("after-completion:daily", 3, None).unwrap();
        assert_eq!(spec.repeat_type, RepeatType::AfterCompletion);
        assert_eq!(spec.interval, 3);

        let spec = RepeatSpec::parse("monthly:last", 1, None).unwrap();
        assert_eq!(spec.day, Some(-1));

        let spec = RepeatSpec::parse("yearly:12-25", 1, None).unwrap();
        assert_eq!(spec.month, Some(12));
        assert_eq!(spec.day, Some(25));

        assert!(RepeatSpec::parse("weekly", 1, None).is_ok());
        assert!(RepeatSpec::parse("yearly", 1, None).is_err());
        assert!(RepeatSpec::parse("hourly", 1, None).is_err());
    }

    #[test]
    fn after_completion_repeat_starts_today() {
        let mut when = When::Someday;
        let spec = RepeatSpec {
            repeat_type: RepeatType::AfterCompletion,
            frequency: Frequency::Weekly,
            interval: 2,
            weekdays: vec!["mon".into()],
            day: None,
            month: None,
            end_date: None,
        };
        let start = prepare_repeat(&mut when, &spec, 20765).unwrap();
        assert_eq!(start, 20765);
        assert_eq!(when, When::Date(20765));

        let rule = spec.build(start).unwrap();
        assert_eq!(rule["tp"], 1);
        assert_eq!(rule["ia"], 0);
    }

    #[test]
    fn repeat_validation_rejects_bad_rules() {
        let mut rule = RepeatSpec {
            frequency: Frequency::Daily,
            ..daily_spec()
        }
        .build(20765)
        .unwrap();
        validate_repeater(&rule).unwrap();

        rule["fu"] = json!(32);
        assert!(validate_repeater(&rule).is_err());

        rule["fu"] = json!(16);
        rule["fa"] = json!(0);
        assert!(validate_repeater(&rule).is_err());
    }

    #[test]
    fn repeat_end_date_cannot_precede_the_first_occurrence() {
        let start = parse_day("2026-11-08").unwrap(); // Sunday
        let mut weekly = RepeatSpec::parse("weekly:mon", 1, Some("2026-11-08".into())).unwrap();
        let error = weekly.build(start).unwrap_err();
        assert!(error.to_string().contains("before the first"), "{error}");
        weekly.end_date = Some("2026-11-09".into());
        let rule = weekly.build(start).unwrap();
        assert_eq!(rule["ed"], rule["ia"]);

        let after_completion =
            RepeatSpec::parse("after-completion:daily", 1, Some("2026-11-07".into())).unwrap();
        assert!(after_completion.build(start).is_err());
    }

    fn daily_spec() -> RepeatSpec {
        RepeatSpec {
            repeat_type: RepeatType::Scheduled,
            frequency: Frequency::Daily,
            interval: 1,
            weekdays: Vec::new(),
            day: None,
            month: None,
            end_date: None,
        }
    }

    #[test]
    fn reminder_times_roundtrip() {
        assert_eq!(parse_reminder("15:45").unwrap(), 56_700);
        assert_eq!(parse_reminder("9:05").unwrap(), 32_700);
        assert_eq!(parse_reminder("23:00:30").unwrap(), 82_830);
        assert!(parse_reminder("24:00").is_err());
        assert!(parse_reminder("12:60").is_err());
        assert!(parse_reminder("noon").is_err());
        assert_eq!(format_reminder(56_700), "15:45");
        assert_eq!(format_reminder(0), "00:00");
    }

    #[test]
    fn create_payload_carries_the_reminder() {
        let mut draft = TaskDraft::new("remind me");
        draft.reminder_time = Some(parse_reminder("09:00").unwrap());

        let undated = task_create_payload(&draft, today(), 1_700_000_000.0);
        assert!(validate_task_create(&undated).is_err());

        draft.when = When::Today;
        let payload = task_create_payload(&draft, today(), 1_700_000_000.0);
        assert_eq!(payload["ato"], 32_400);
        validate_task_create(&payload).unwrap();

        let payload = task_create_payload(&TaskDraft::new("no reminder"), today(), 1.0);
        assert!(payload["ato"].is_null());
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
    fn safety_gate_rejects_non_null_rp_on_create() {
        let mut payload = task_create_payload(&TaskDraft::new("x"), today(), 1.0);
        payload["rp"] = json!({"rrv": 4});
        let error = validate_task_create(&payload).unwrap_err();
        assert_eq!(error.to_string(), "rp must be null in Task7 creates");
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
