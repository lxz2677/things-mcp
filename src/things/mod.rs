//! Things Cloud wire protocol types and decoders.

pub mod client;
pub mod encode;
pub mod model;
pub mod wire;

pub use client::{
    AccountInfo, ClientOptions, HistoryMeta, HttpTransport, IncomingItem, ItemsPage, OwnHistoryKey,
    RawReply, RawRequest, ThingsClient, ThingsError, Transport,
};
pub use encode::{
    AREA_KIND, CHECKLIST_KIND, TAG_KIND, TASK_CREATE_KEYS, TASK_KIND, TaskDraft, When,
    area_create_payload, cancel_update, checklist_create_payload, complete_update,
    container_update, deadline_update, delete_envelope, envelope, format_reminder, notes_object,
    notes_update, parse_day, parse_reminder, reopen_update, tag_create_payload, tags_update,
    task_create_payload, title_update, trash_update, validate_area_create,
    validate_checklist_create, validate_tag_create, validate_task_create, when_update,
};
pub use model::{
    ACTION_CREATE, ACTION_DELETE, ACTION_UPDATE, Area, ChecklistItem, EntityKind, START_INBOX,
    START_SOMEDAY, START_TODAY, STATUS_CANCELED, STATUS_COMPLETED, STATUS_OPEN, TYPE_HEADING,
    TYPE_PROJECT, TYPE_TODO, Tag, Task,
};
pub use wire::{apply_area, apply_checklist_item, apply_tag, apply_task, day_from_wire};
