//! Entity model.
//!
//! Field names follow the local model used by the reference implementation
//! (`state/memory`), while values keep the wire semantics documented in
//! `docs/04-data-model-storage.md`:
//!
//! * `status`: `0 = open`, `2 = canceled`, `3 = completed`
//! * `start`:  `0 = inbox`, `1 = started (today/anytime)`, `2 = deferred (someday/upcoming)`
//! * `type`:   `0 = todo`, `1 = project`, `2 = heading`
//! * instants are fractional Unix seconds; day-grained values are epoch days

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

pub const STATUS_OPEN: i64 = 0;
pub const STATUS_CANCELED: i64 = 2;
pub const STATUS_COMPLETED: i64 = 3;

pub const START_INBOX: i64 = 0;
pub const START_TODAY: i64 = 1;
pub const START_SOMEDAY: i64 = 2;

pub const TYPE_TODO: i64 = 0;
pub const TYPE_PROJECT: i64 = 1;
pub const TYPE_HEADING: i64 = 2;

pub const ACTION_CREATE: i64 = 0;
pub const ACTION_UPDATE: i64 = 1;
pub const ACTION_DELETE: i64 = 2;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EntityKind {
    Task,
    Area,
    Tag,
    ChecklistItem,
    Settings,
    Tombstone,
    Unknown,
}

impl EntityKind {
    /// Entity names on the wire carry a schema suffix (`Task7`, `Area3`, ...).
    pub fn from_wire(name: &str) -> Self {
        let base = name.trim_end_matches(|c: char| c.is_ascii_digit());
        match base {
            "Task" => Self::Task,
            "Area" => Self::Area,
            "Tag" => Self::Tag,
            "ChecklistItem" => Self::ChecklistItem,
            "Settings" => Self::Settings,
            "Tombstone" => Self::Tombstone,
            _ => Self::Unknown,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Task => "task",
            Self::Area => "area",
            Self::Tag => "tag",
            Self::ChecklistItem => "checklist_item",
            Self::Settings => "settings",
            Self::Tombstone => "tombstone",
            Self::Unknown => "unknown",
        }
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Task {
    pub id: String,
    /// Wire entity name this row was built from (`Task7`, `Task6`, ...).
    pub kind: String,
    #[serde(rename = "type")]
    pub type_: i64,
    pub status: i64,
    pub trashed: bool,
    pub title: String,
    pub notes: String,

    pub creation_date: Option<f64>,
    pub user_modification_date: Option<f64>,
    pub stop_date: Option<f64>,
    pub last_reminder_interaction_date: Option<f64>,

    pub start: i64,
    pub start_date: Option<i64>,
    pub start_bucket: i64,
    pub deadline: Option<i64>,
    pub deadline_suppression_date: Option<i64>,
    pub deadline_offset: i64,

    pub index: Option<i64>,
    pub today_index: Option<i64>,
    pub today_index_reference_date: Option<i64>,

    pub project: Option<String>,
    pub area: Option<String>,
    pub heading: Option<String>,
    pub tags: Vec<String>,

    /// Raw `rr` JSON object as sent on the wire (kept verbatim, never re-encoded).
    pub recurrence_rule: Option<String>,
    pub repeating_template: Option<String>,
    pub instance_creation_start_date: Option<i64>,
    pub instance_creation_paused: bool,
    pub instance_creation_count: i64,
    pub after_completion_reference_date: Option<i64>,
    pub next_instance_start_date: Option<i64>,
    pub leaves_tombstone: bool,
    pub experimental: Option<Value>,

    /// Every wire property ever seen for this entity (merged across updates).
    pub raw: Map<String, Value>,
}

impl Task {
    pub fn is_open(&self) -> bool {
        self.status == STATUS_OPEN && !self.trashed
    }

    pub fn is_completed(&self) -> bool {
        self.status == STATUS_COMPLETED
    }

    pub fn status_str(&self) -> &'static str {
        if self.trashed {
            return "trashed";
        }
        match self.status {
            STATUS_COMPLETED => "completed",
            STATUS_CANCELED => "canceled",
            _ => "open",
        }
    }

    pub fn type_str(&self) -> &'static str {
        match self.type_ {
            TYPE_PROJECT => "project",
            TYPE_HEADING => "heading",
            _ => "todo",
        }
    }

    /// Human-facing "view" derived from `start` and the date fields.
    pub fn view_str(&self, today: i64) -> &'static str {
        match self.start {
            START_INBOX => {
                if self.project.is_some() || self.area.is_some() {
                    "anytime"
                } else {
                    "inbox"
                }
            }
            START_TODAY => match self.start_date {
                Some(d) if d <= today => "today",
                Some(_) => "upcoming",
                None if self
                    .today_index_reference_date
                    .is_some_and(|day| day <= today) =>
                {
                    "today"
                }
                None => "anytime",
            },
            START_SOMEDAY => match self.start_date {
                Some(d) if d > today => "upcoming",
                Some(_) => "today",
                None => "someday",
            },
            _ => "unknown",
        }
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Area {
    pub id: String,
    pub kind: String,
    pub title: String,
    pub visible: bool,
    pub index: Option<i64>,
    pub tags: Vec<String>,
    pub raw: Map<String, Value>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Tag {
    pub id: String,
    pub kind: String,
    pub title: String,
    pub shortcut: Option<String>,
    pub parent: Option<String>,
    pub index: Option<i64>,
    pub raw: Map<String, Value>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct ChecklistItem {
    pub id: String,
    pub kind: String,
    pub title: String,
    pub status: i64,
    pub index: Option<i64>,
    pub creation_date: Option<f64>,
    pub user_modification_date: Option<f64>,
    pub stop_date: Option<f64>,
    pub task: Option<String>,
    pub leaves_tombstone: bool,
    pub raw: Map<String, Value>,
}

impl ChecklistItem {
    pub fn is_completed(&self) -> bool {
        self.status == STATUS_COMPLETED
    }
}
