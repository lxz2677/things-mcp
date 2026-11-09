//! MCP server (stdio transport).

use std::sync::Arc;
use std::time::Instant;

use rmcp::handler::server::router::tool::ToolRouter;
use rmcp::handler::server::tool::ToolCallContext;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{
    CallToolRequestParams, CallToolResponse, CallToolResult, Implementation, ServerCapabilities,
    ServerConfig,
};
use rmcp::service::{QuitReason, RequestContext};
use rmcp::{ErrorData as McpError, ServiceExt, tool, tool_handler, tool_router};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::sync::Mutex;

use crate::config::Config;
use crate::fmt;
use crate::ids;
use crate::store::{
    ListKind, SearchFilter, Store, area_detail, count_tasks, list_areas, list_projects, list_tags,
    list_tasks, project_detail, search_tasks, summaries, tag_detail, task_counts, task_detail,
};
use crate::sync::{SyncEngine, SyncReport};
use crate::things::{
    ClientOptions, HttpTransport, RepeatSpec, SUPPORTED_SCHEMA, TaskDraft, ThingsClient,
    ThingsError, When, parse_day, parse_reminder, prepare_repeat,
};
use crate::write::{TaskPatch, WriteService};

macro_rules! store_or_return {
    ($server:expr) => {
        match $server.open_store() {
            Ok(store) => store,
            Err(result) => return Ok(result),
        }
    };
}

macro_rules! store_or_err {
    ($server:expr) => {
        match $server.open_store() {
            Ok(store) => store,
            Err(result) => return Err(result),
        }
    };
}

macro_rules! value_or_return {
    ($expr:expr) => {
        match $expr {
            Ok(value) => value,
            Err(result) => return Ok(result),
        }
    };
}

macro_rules! write_service_or_return {
    ($server:expr, $dry_run:expr) => {
        match $server.write_service($dry_run).await {
            Ok(service) => service,
            Err(result) => return Ok(result),
        }
    };
}

const DEFAULT_LIMIT: usize = 100;
const MAX_LIMIT: usize = 500;
pub const DIAGNOSTIC_TARGET: &str = "things_mcp_diagnostics";
const INSTRUCTIONS: &str = "\
Read tools use a local SQLite replica that is refreshed lazily. Call `sync_now` to force a pull.\n\
Write tools commit directly to Things Cloud by default. Set dry_run=true to preview the payload.\n\
Repeating tasks: repeat={frequency: daily|weekly|monthly|yearly, interval?, weekdays?, day?, month?, end_date?, type?: scheduled|after_completion}.\n\
Lists: inbox, today, upcoming, anytime, someday, logbook, deadlines, repeating, \
trash, all, project-<id>, area-<id>, tag-<id>.\n\
Task ids are base58 strings. Dates are YYYY-MM-DD, timestamps RFC-3339.";

struct ServerState {
    config: Config,
    refresh_secs: u64,
    sync_lock: Mutex<()>,
}

#[derive(Clone)]
pub struct ThingsServer {
    tool_router: ToolRouter<Self>,
    state: Arc<ServerState>,
}

impl ThingsServer {
    pub fn new(config: Config, refresh_secs: u64) -> Self {
        Self {
            tool_router: Self::tool_router(),
            state: Arc::new(ServerState {
                config,
                refresh_secs,
                sync_lock: Mutex::new(()),
            }),
        }
    }

    fn open_store_raw(&self) -> Result<Store, McpError> {
        Store::open(&self.state.config.db_path).map_err(internal)
    }

    /// Data tools go through this gate. Failures are returned as an MCP
    /// **tool result** (`isError: true`) so the agent can read the reason and
    /// stop, instead of a JSON-RPC protocol error it may never see.
    fn open_store(&self) -> Result<Store, CallToolResult> {
        let store = match self.open_store_raw() {
            Ok(store) => store,
            Err(error) => {
                return Err(tool_error(
                    "LOCAL_DB_ERROR",
                    error.message.to_string(),
                    json!({}),
                ));
            }
        };
        if let Err(error) = store.ensure_compatible() {
            return Err(self.incompatible_result(&store, &error));
        }
        Ok(store)
    }

    fn incompatible_result(&self, store: &Store, error: &anyhow::Error) -> CallToolResult {
        let compat = store.compat().ok();
        let reason = compat
            .as_ref()
            .and_then(|compat| compat.reason.clone())
            .unwrap_or_else(|| error.to_string());
        tool_error(
            "INCOMPATIBLE_VERSION",
            format!(
                "Things Cloud is incompatible with this build: {reason}. Do not retry; \
                 ask the user to upgrade things-mcp, then run sync."
            ),
            json!({
                "do_not_retry": true,
                "cloud_schema": compat.as_ref().and_then(|c| c.cloud_schema),
                "supported_schema": SUPPORTED_SCHEMA,
                "entities": compat.as_ref().map(|c| c.entities.clone()),
                "checked_at": compat.as_ref().and_then(|c| c.checked_at.clone()),
                "remediation": [
                    "Stop calling things-mcp tools.",
                    "Ask the user to upgrade things-mcp to a build that supports this cloud schema.",
                    "After upgrading, call sync_now once; the compatibility state clears on success."
                ],
            }),
        )
    }

    fn build_client(&self, store: &Store) -> Result<ThingsClient, McpError> {
        let app_instance_id = match store.app_instance_id().map_err(internal)? {
            Some(value) if !value.is_empty() => value,
            _ => {
                let value = ids::new_uuid();
                store.set_app_instance_id(&value).map_err(internal)?;
                value
            }
        };

        let mut options = ClientOptions::new(
            self.state.config.email().map_err(internal)?,
            self.state.config.password().map_err(internal)?,
            app_instance_id,
        );
        options.base_url = self.state.config.base_url.clone();

        Ok(ThingsClient::new(
            Arc::new(HttpTransport::new().map_err(internal)?),
            options,
        ))
    }

    fn needs_refresh(&self) -> Result<bool, McpError> {
        if self.state.refresh_secs == 0 {
            return Ok(false);
        }
        let store = self.open_store_raw()?;
        let Some(last) = store.last_sync_at().map_err(internal)? else {
            return Ok(true);
        };
        let Ok(last) = chrono::DateTime::parse_from_rfc3339(&last) else {
            return Ok(true);
        };
        let age = chrono::Utc::now().signed_duration_since(last.with_timezone(&chrono::Utc));
        Ok(age.num_seconds() >= self.state.refresh_secs as i64)
    }

    async fn refresh(&self, force: bool, full: bool) -> Result<Option<SyncReport>, McpError> {
        // Local-only mode: without credentials we never touch the network
        // implicitly, so read tools keep working on a pre-populated replica.
        if !force && (self.state.config.email.is_none() || self.state.config.password.is_none()) {
            return Ok(None);
        }

        let _guard = self.state.sync_lock.lock().await;
        if !force && !self.needs_refresh()? {
            return Ok(None);
        }

        let store = self.open_store_raw()?;
        let client = self.build_client(&store)?;
        client.login().await.map_err(things_error)?;
        let mut engine = SyncEngine::new(client, store);
        let report = engine.sync(full).await.map_err(anyhow_to_mcp)?;
        Ok(Some(report))
    }

    /// Best-effort freshness: network failures must not break local reads.
    async fn ensure_fresh(&self) -> Option<String> {
        match self.refresh(false, false).await {
            Ok(_) => None,
            Err(error) => {
                tracing::warn!(message = %error.message, "refresh failed; serving cached data");
                Some(error.message.to_string())
            }
        }
    }

    fn list_payload(
        &self,
        kind: &ListKind,
        limit: usize,
        offset: usize,
    ) -> Result<Value, CallToolResult> {
        let store = store_or_err!(self);
        let conn = store.connection();
        let today = fmt::today_epoch_day();
        let limit = clamp(limit);

        match kind {
            ListKind::Projects => {
                let projects =
                    list_projects(conn, None, Some("open"), limit).map_err(query_error)?;
                Ok(json!({
                    "list": "projects",
                    "count": projects.len(),
                    "projects": projects,
                }))
            }
            ListKind::Areas => {
                let areas = list_areas(conn, limit).map_err(query_error)?;
                Ok(json!({
                    "list": "areas",
                    "count": areas.len(),
                    "areas": areas,
                }))
            }
            _ => {
                let total = count_tasks(conn, kind, today).map_err(query_error)?;
                let tasks = list_tasks(conn, kind, today, limit, offset).map_err(query_error)?;
                let items = summaries(conn, &tasks, today).map_err(query_error)?;
                let seen = offset as i64 + items.len() as i64;
                Ok(json!({
                    "list": kind.as_str(),
                    "count": items.len(),
                    "total": total,
                    "has_more": seen < total,
                    "offset": offset,
                    "tasks": items,
                }))
            }
        }
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct ListTasksArgs {
    /// inbox | today | upcoming | anytime | someday | logbook | deadlines |
    /// repeating | trash | all | project-<id> | area-<id> | tag-<id>
    pub list: String,
    /// Maximum number of tasks to return (1-500, default 100).
    pub limit: Option<usize>,
    /// Number of tasks to skip (default 0).
    pub offset: Option<usize>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct GetTaskArgs {
    /// Task id (base58).
    pub id: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct SearchArgs {
    /// Case-insensitive text matched against title and notes.
    pub query: String,
    /// Only tasks in this project id.
    pub project_id: Option<String>,
    /// Only tasks in this area id.
    pub area_id: Option<String>,
    /// Only tasks carrying all of these tag ids.
    pub tag_ids: Option<Vec<String>>,
    /// open | canceled | completed.
    pub status: Option<String>,
    /// Include completed/canceled tasks (default false).
    pub include_completed: Option<bool>,
    /// Maximum number of tasks to return (1-500, default 100).
    pub limit: Option<usize>,
    /// Number of tasks to skip (default 0).
    pub offset: Option<usize>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct ListProjectsArgs {
    /// Only projects in this area id.
    pub area_id: Option<String>,
    /// open | completed | canceled | all (default open).
    pub status: Option<String>,
    /// Maximum number of projects to return (1-500, default 100).
    pub limit: Option<usize>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct LimitArgs {
    /// Maximum number of entries to return (1-500, default 100).
    pub limit: Option<usize>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct ListByIdArgs {
    /// Entity id (base58).
    pub id: String,
    /// Maximum number of tasks to return (1-500, default 100).
    pub limit: Option<usize>,
    /// Number of tasks to skip (default 0).
    pub offset: Option<usize>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct GetByIdArgs {
    /// Entity id (base58).
    pub id: String,
    /// Maximum number of child tasks to return (1-500, default 100).
    pub limit: Option<usize>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct CreateTaskArgs {
    /// Task title.
    pub title: String,
    /// Notes (plain text).
    pub notes: Option<String>,
    /// inbox | today | anytime | someday | evening | YYYY-MM-DD (default inbox).
    pub when: Option<String>,
    /// Deadline as YYYY-MM-DD.
    pub deadline: Option<String>,
    /// Project id (base58).
    pub project_id: Option<String>,
    /// Area id (base58).
    pub area_id: Option<String>,
    /// Heading id (base58).
    pub heading_id: Option<String>,
    /// Tag ids (base58).
    pub tags: Option<Vec<String>>,
    /// Reminder time in local time, `HH:MM`. Requires a dated `when`.
    pub reminder: Option<String>,
    /// Recurrence rule. Dated `when` values are used as the rule start; an
    /// undated task is scheduled on the first matching occurrence.
    pub repeat: Option<RepeatSpec>,
    /// Preview the exact wire payload without writing (default false).
    pub dry_run: Option<bool>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct CreateProjectArgs {
    /// Project title.
    pub title: String,
    pub notes: Option<String>,
    /// anytime | today | someday | YYYY-MM-DD (default anytime).
    pub when: Option<String>,
    pub deadline: Option<String>,
    pub area_id: Option<String>,
    pub tags: Option<Vec<String>>,
    /// Reminder time in local time, `HH:MM`. Requires a dated `when`.
    pub reminder: Option<String>,
    /// Recurrence rule.
    pub repeat: Option<RepeatSpec>,
    pub dry_run: Option<bool>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct CreateAreaArgs {
    pub title: String,
    pub dry_run: Option<bool>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct CreateTagArgs {
    pub title: String,
    pub parent_id: Option<String>,
    pub shortcut: Option<String>,
    pub dry_run: Option<bool>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct UpdateTagArgs {
    /// Tag id (base58).
    pub id: String,
    pub title: Option<String>,
    /// New parent tag id.
    pub parent_id: Option<String>,
    pub clear_parent: Option<bool>,
    pub shortcut: Option<String>,
    pub clear_shortcut: Option<bool>,
    pub dry_run: Option<bool>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct UpdateAreaArgs {
    /// Area id (base58).
    pub id: String,
    pub title: Option<String>,
    /// Tag ids attached to the area.
    pub tags: Option<Vec<String>>,
    pub clear_tags: Option<bool>,
    pub dry_run: Option<bool>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct UpdateTaskArgs {
    /// Task id (base58).
    pub id: String,
    pub title: Option<String>,
    pub notes: Option<String>,
    /// inbox | today | anytime | someday | evening | YYYY-MM-DD.
    pub when: Option<String>,
    pub deadline: Option<String>,
    pub clear_deadline: Option<bool>,
    pub project_id: Option<String>,
    pub clear_project: Option<bool>,
    pub area_id: Option<String>,
    pub clear_area: Option<bool>,
    pub heading_id: Option<String>,
    pub clear_heading: Option<bool>,
    pub tags: Option<Vec<String>>,
    pub clear_tags: Option<bool>,
    /// Reminder time in local time, `HH:MM`. The item needs a start date.
    pub reminder: Option<String>,
    pub clear_reminder: Option<bool>,
    /// Recurrence rule; requires a dated task or `when` in the same call.
    pub repeat: Option<RepeatSpec>,
    pub clear_repeat: Option<bool>,
    pub dry_run: Option<bool>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct UpdateChecklistArgs {
    /// Checklist item id (base58).
    pub id: String,
    /// New title: omitted leaves it unchanged.
    pub title: Option<String>,
    /// New position within the parent's checklist (`ix`).
    pub index: Option<i64>,
    pub dry_run: Option<bool>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct ToggleChecklistArgs {
    /// Checklist item id (base58).
    pub id: String,
    /// true = completed, false = reopen; omit to flip the current state.
    pub completed: Option<bool>,
    pub dry_run: Option<bool>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct IdDryRunArgs {
    /// Entity id (base58).
    pub id: String,
    pub dry_run: Option<bool>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct ChecklistArgs {
    /// Parent task id (base58).
    pub task_id: String,
    pub title: String,
    pub dry_run: Option<bool>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct SyncNowArgs {
    /// Discard the local replica and re-download the full history.
    #[serde(default)]
    pub full: bool,
}

impl ThingsServer {
    async fn write_service(&self, dry_run: bool) -> Result<WriteService, CallToolResult> {
        let store = match self.open_store_raw() {
            Ok(store) => store,
            Err(error) => {
                return Err(tool_error(
                    "LOCAL_DB_ERROR",
                    error.message.to_string(),
                    json!({}),
                ));
            }
        };
        if let Err(error) = store.ensure_compatible() {
            return Err(self.incompatible_result(&store, &error));
        }

        if dry_run {
            return Ok(WriteService::Preview(self.state.config.db_path.clone()));
        }

        let client = match self.build_client(&store) {
            Ok(client) => client,
            Err(error) => {
                return Err(tool_error(
                    "CONFIG_ERROR",
                    error.message.to_string(),
                    json!({}),
                ));
            }
        };
        if let Err(error) = client.login().await {
            return Err(tool_error(code_of(&error), error.to_string(), json!({})));
        }
        Ok(WriteService::live(SyncEngine::new(client, store)))
    }
}

#[allow(clippy::too_many_arguments)]
fn draft_from(
    title: String,
    notes: Option<String>,
    when: Option<String>,
    deadline: Option<String>,
    project: Option<String>,
    area: Option<String>,
    heading: Option<String>,
    tags: Option<Vec<String>>,
    reminder: Option<String>,
    repeat: Option<RepeatSpec>,
    task_type: i64,
) -> Result<TaskDraft, McpError> {
    let mut draft = TaskDraft::new(title);
    draft.task_type = task_type;
    draft.notes = notes.unwrap_or_default();
    let default_when = if task_type == 1 { "anytime" } else { "inbox" };
    draft.when = When::parse(when.as_deref().unwrap_or(default_when)).map_err(invalid_param)?;
    draft.deadline = deadline
        .as_deref()
        .map(parse_day)
        .transpose()
        .map_err(invalid_param)?;
    draft.project = project;
    draft.area = area;
    draft.heading = heading;
    draft.tags = tags.unwrap_or_default();
    draft.reminder_time = reminder
        .as_deref()
        .map(parse_reminder)
        .transpose()
        .map_err(invalid_param)?;
    if let Some(spec) = repeat {
        let start = prepare_repeat(&mut draft.when, &spec, fmt::today_epoch_day())
            .map_err(invalid_param)?;
        draft.repeater = Some(spec.build(start).map_err(invalid_param)?);
    }
    Ok(draft)
}

#[tool_router]
impl ThingsServer {
    #[tool(
        description = "List tasks from a Things view or container. Lists: inbox, today, upcoming, anytime, someday, logbook, deadlines, repeating, trash, all, project-<id>, area-<id>, tag-<id>."
    )]
    async fn list_tasks(
        &self,
        Parameters(args): Parameters<ListTasksArgs>,
    ) -> Result<CallToolResult, McpError> {
        let warning = self.ensure_fresh().await;
        let kind = parse_list(&args.list)?;
        let payload = value_or_return!(self.list_payload(
            &kind,
            args.limit.unwrap_or(DEFAULT_LIMIT),
            args.offset.unwrap_or(0),
        ));
        Ok(CallToolResult::structured(with_warning(payload, warning)))
    }

    #[tool(
        description = "Get one task in detail: notes, project/area/heading, tags, checklist items, dates, and the rehearsal rule."
    )]
    async fn get_task(
        &self,
        Parameters(args): Parameters<GetTaskArgs>,
    ) -> Result<CallToolResult, McpError> {
        let warning = self.ensure_fresh().await;
        let store = store_or_return!(self);
        let today = fmt::today_epoch_day();
        let detail = task_detail(store.connection(), &args.id, today)
            .map_err(internal)?
            .ok_or_else(|| {
                McpError::internal_error(
                    format!("task {} not found in the local replica", args.id),
                    Some(json!({"code": "NOT_FOUND"})),
                )
            })?;
        let payload = serde_json::to_value(detail).map_err(internal)?;
        Ok(CallToolResult::structured(with_warning(payload, warning)))
    }

    #[tool(
        description = "Search tasks by text in title/notes, with optional project, area, tag, and status filters."
    )]
    async fn search(
        &self,
        Parameters(args): Parameters<SearchArgs>,
    ) -> Result<CallToolResult, McpError> {
        let warning = self.ensure_fresh().await;
        let store = store_or_return!(self);
        let today = fmt::today_epoch_day();
        let status = match args.status.as_deref() {
            None => None,
            Some("open") => Some(0),
            Some("canceled") => Some(2),
            Some("completed") => Some(3),
            Some(other) => {
                return Err(McpError::invalid_params(
                    format!("unknown status `{other}` (open|canceled|completed)"),
                    None,
                ));
            }
        };

        let filter = SearchFilter {
            query: Some(args.query.clone()),
            status,
            project_id: args.project_id,
            area_id: args.area_id,
            tag_ids: args.tag_ids.unwrap_or_default(),
            include_completed: args.include_completed.unwrap_or(false),
            ..Default::default()
        };

        let limit = clamp(args.limit.unwrap_or(DEFAULT_LIMIT));
        let offset = args.offset.unwrap_or(0);
        let tasks = search_tasks(store.connection(), &filter, limit, offset).map_err(internal)?;
        let items = summaries(store.connection(), &tasks, today).map_err(internal)?;
        let payload = json!({
            "query": args.query,
            "count": items.len(),
            "offset": offset,
            "tasks": items,
        });
        Ok(CallToolResult::structured(with_warning(payload, warning)))
    }

    #[tool(
        description = "List projects with open/completed task counts, optionally filtered by area."
    )]
    async fn list_projects(
        &self,
        Parameters(args): Parameters<ListProjectsArgs>,
    ) -> Result<CallToolResult, McpError> {
        let warning = self.ensure_fresh().await;
        let store = store_or_return!(self);
        let limit = clamp(args.limit.unwrap_or(DEFAULT_LIMIT));
        let projects = list_projects(
            store.connection(),
            args.area_id.as_deref(),
            Some(args.status.as_deref().unwrap_or("open")),
            limit,
        )
        .map_err(internal)?;
        let payload = json!({"count": projects.len(), "projects": projects});
        Ok(CallToolResult::structured(with_warning(payload, warning)))
    }

    #[tool(description = "List areas with project and open-task counts.")]
    async fn list_areas(
        &self,
        Parameters(args): Parameters<LimitArgs>,
    ) -> Result<CallToolResult, McpError> {
        let warning = self.ensure_fresh().await;
        let store = store_or_return!(self);
        let areas = list_areas(
            store.connection(),
            clamp(args.limit.unwrap_or(DEFAULT_LIMIT)),
        )
        .map_err(internal)?;
        let payload = json!({"count": areas.len(), "areas": areas});
        Ok(CallToolResult::structured(with_warning(payload, warning)))
    }

    #[tool(description = "List tags (flat, with parent reference and open-task counts).")]
    async fn list_tags(
        &self,
        Parameters(args): Parameters<LimitArgs>,
    ) -> Result<CallToolResult, McpError> {
        let warning = self.ensure_fresh().await;
        let store = store_or_return!(self);
        let tags = list_tags(
            store.connection(),
            clamp(args.limit.unwrap_or(DEFAULT_LIMIT)),
        )
        .map_err(internal)?;
        let payload = json!({"count": tags.len(), "tags": tags});
        Ok(CallToolResult::structured(with_warning(payload, warning)))
    }

    #[tool(description = "List completed and canceled tasks (Logbook).")]
    async fn list_logbook(
        &self,
        Parameters(args): Parameters<ListTasksArgs2>,
    ) -> Result<CallToolResult, McpError> {
        let warning = self.ensure_fresh().await;
        let payload = value_or_return!(self.list_payload(
            &ListKind::Logbook,
            args.limit.unwrap_or(DEFAULT_LIMIT),
            args.offset.unwrap_or(0),
        ));
        Ok(CallToolResult::structured(with_warning(payload, warning)))
    }

    #[tool(description = "List open tasks that have a deadline.")]
    async fn list_deadlines(
        &self,
        Parameters(args): Parameters<ListTasksArgs2>,
    ) -> Result<CallToolResult, McpError> {
        let warning = self.ensure_fresh().await;
        let payload = value_or_return!(self.list_payload(
            &ListKind::Deadlines,
            args.limit.unwrap_or(DEFAULT_LIMIT),
            args.offset.unwrap_or(0),
        ));
        Ok(CallToolResult::structured(with_warning(payload, warning)))
    }

    #[tool(description = "List tasks scheduled for a future date (Upcoming).")]
    async fn list_upcoming(
        &self,
        Parameters(args): Parameters<ListTasksArgs2>,
    ) -> Result<CallToolResult, McpError> {
        let warning = self.ensure_fresh().await;
        let payload = value_or_return!(self.list_payload(
            &ListKind::Upcoming,
            args.limit.unwrap_or(DEFAULT_LIMIT),
            args.offset.unwrap_or(0),
        ));
        Ok(CallToolResult::structured(with_warning(payload, warning)))
    }

    #[tool(description = "List tasks of one project.")]
    async fn list_by_project(
        &self,
        Parameters(args): Parameters<ListByIdArgs>,
    ) -> Result<CallToolResult, McpError> {
        let warning = self.ensure_fresh().await;
        let payload = value_or_return!(self.list_payload(
            &ListKind::Project(args.id),
            args.limit.unwrap_or(DEFAULT_LIMIT),
            args.offset.unwrap_or(0),
        ));
        Ok(CallToolResult::structured(with_warning(payload, warning)))
    }

    #[tool(description = "List tasks of one area.")]
    async fn list_by_area(
        &self,
        Parameters(args): Parameters<ListByIdArgs>,
    ) -> Result<CallToolResult, McpError> {
        let warning = self.ensure_fresh().await;
        let payload = value_or_return!(self.list_payload(
            &ListKind::Area(args.id),
            args.limit.unwrap_or(DEFAULT_LIMIT),
            args.offset.unwrap_or(0),
        ));
        Ok(CallToolResult::structured(with_warning(payload, warning)))
    }

    #[tool(description = "List tasks carrying one tag.")]
    async fn list_by_tag(
        &self,
        Parameters(args): Parameters<ListByIdArgs>,
    ) -> Result<CallToolResult, McpError> {
        let warning = self.ensure_fresh().await;
        let payload = value_or_return!(self.list_payload(
            &ListKind::Tag(args.id),
            args.limit.unwrap_or(DEFAULT_LIMIT),
            args.offset.unwrap_or(0),
        ));
        Ok(CallToolResult::structured(with_warning(payload, warning)))
    }

    #[tool(description = "Get one project: notes, headings, and its open tasks.")]
    async fn get_project(
        &self,
        Parameters(args): Parameters<GetByIdArgs>,
    ) -> Result<CallToolResult, McpError> {
        let warning = self.ensure_fresh().await;
        let store = store_or_return!(self);
        let today = fmt::today_epoch_day();
        let limit = clamp(args.limit.unwrap_or(DEFAULT_LIMIT));
        let detail = project_detail(store.connection(), &args.id, today, limit)
            .map_err(internal)?
            .ok_or_else(|| not_found("project", &args.id))?;
        let payload = serde_json::to_value(detail).map_err(internal)?;
        Ok(CallToolResult::structured(with_warning(payload, warning)))
    }

    #[tool(description = "Get an area's tag IDs, projects, and open tasks.")]
    async fn get_area(
        &self,
        Parameters(args): Parameters<GetByIdArgs>,
    ) -> Result<CallToolResult, McpError> {
        let warning = self.ensure_fresh().await;
        let store = store_or_return!(self);
        let today = fmt::today_epoch_day();
        let limit = clamp(args.limit.unwrap_or(DEFAULT_LIMIT));
        let detail = area_detail(store.connection(), &args.id, today, limit)
            .map_err(internal)?
            .ok_or_else(|| not_found("area", &args.id))?;
        let payload = serde_json::to_value(detail).map_err(internal)?;
        Ok(CallToolResult::structured(with_warning(payload, warning)))
    }

    #[tool(description = "Get one tag: parent, child tags, and tagged open tasks.")]
    async fn get_tag(
        &self,
        Parameters(args): Parameters<GetByIdArgs>,
    ) -> Result<CallToolResult, McpError> {
        let warning = self.ensure_fresh().await;
        let store = store_or_return!(self);
        let today = fmt::today_epoch_day();
        let limit = clamp(args.limit.unwrap_or(DEFAULT_LIMIT));
        let detail = tag_detail(store.connection(), &args.id, today, limit)
            .map_err(internal)?
            .ok_or_else(|| not_found("tag", &args.id))?;
        let payload = serde_json::to_value(detail).map_err(internal)?;
        Ok(CallToolResult::structured(with_warning(payload, warning)))
    }

    #[tool(
        description = "Create a task in Things Cloud. `repeat` adds a recurrence rule (daily/weekly/monthly/yearly, interval, weekdays, end date, after-completion); an undated repeat is scheduled on its first occurrence. Set dry_run=true to preview the 34-key wire payload."
    )]
    async fn create_task(
        &self,
        Parameters(args): Parameters<CreateTaskArgs>,
    ) -> Result<CallToolResult, McpError> {
        let dry_run = args.dry_run.unwrap_or(false);
        let draft = draft_from(
            args.title,
            args.notes,
            args.when,
            args.deadline,
            args.project_id,
            args.area_id,
            args.heading_id,
            args.tags,
            args.reminder,
            args.repeat,
            0,
        )?;
        let mut service = write_service_or_return!(self, dry_run);
        let outcome = service.create_task(draft).await.map_err(write_error)?;
        Ok(CallToolResult::structured(
            serde_json::to_value(outcome).map_err(internal)?,
        ))
    }

    #[tool(description = "Create a project. Same payload rules as create_task with type=project.")]
    async fn create_project(
        &self,
        Parameters(args): Parameters<CreateProjectArgs>,
    ) -> Result<CallToolResult, McpError> {
        let dry_run = args.dry_run.unwrap_or(false);
        let draft = draft_from(
            args.title,
            args.notes,
            args.when,
            args.deadline,
            None,
            args.area_id,
            None,
            args.tags,
            args.reminder,
            args.repeat,
            1,
        )?;
        let mut service = write_service_or_return!(self, dry_run);
        let outcome = service.create_task(draft).await.map_err(write_error)?;
        Ok(CallToolResult::structured(
            serde_json::to_value(outcome).map_err(internal)?,
        ))
    }

    #[tool(description = "Create an area.")]
    async fn create_area(
        &self,
        Parameters(args): Parameters<CreateAreaArgs>,
    ) -> Result<CallToolResult, McpError> {
        let dry_run = args.dry_run.unwrap_or(false);
        let mut service = write_service_or_return!(self, dry_run);
        let outcome = service
            .create_area(&args.title)
            .await
            .map_err(write_error)?;
        Ok(CallToolResult::structured(
            serde_json::to_value(outcome).map_err(internal)?,
        ))
    }

    #[tool(description = "Create a tag, optionally nested under parent_id.")]
    async fn create_tag(
        &self,
        Parameters(args): Parameters<CreateTagArgs>,
    ) -> Result<CallToolResult, McpError> {
        let dry_run = args.dry_run.unwrap_or(false);
        let mut service = write_service_or_return!(self, dry_run);
        let outcome = service
            .create_tag(
                &args.title,
                args.parent_id.as_deref(),
                args.shortcut.as_deref(),
            )
            .await
            .map_err(write_error)?;
        Ok(CallToolResult::structured(
            serde_json::to_value(outcome).map_err(internal)?,
        ))
    }

    #[tool(
        description = "Update a task. Only the provided fields are sent as a minimal delta (plus md). `repeat` sets a recurrence rule; `clear_repeat` removes it."
    )]
    async fn update_task(
        &self,
        Parameters(args): Parameters<UpdateTaskArgs>,
    ) -> Result<CallToolResult, McpError> {
        let dry_run = args.dry_run.unwrap_or(false);
        let deadline = if args.clear_deadline.unwrap_or(false) {
            Some(None)
        } else {
            args.deadline
                .as_deref()
                .map(parse_day)
                .transpose()
                .map_err(invalid_param)?
                .map(Some)
        };
        let project = if args.clear_project.unwrap_or(false) {
            Some(None)
        } else {
            args.project_id.map(Some)
        };
        let area = if args.clear_area.unwrap_or(false) {
            Some(None)
        } else {
            args.area_id.map(Some)
        };
        let heading = if args.clear_heading.unwrap_or(false) {
            Some(None)
        } else {
            args.heading_id.map(Some)
        };
        let tags = if args.clear_tags.unwrap_or(false) {
            Some(Vec::new())
        } else {
            args.tags
        };
        let repeat = if args.clear_repeat.unwrap_or(false) {
            Some(None)
        } else {
            args.repeat.map(Some)
        };
        let reminder_time = if args.clear_reminder.unwrap_or(false) {
            Some(None)
        } else {
            args.reminder
                .as_deref()
                .map(parse_reminder)
                .transpose()
                .map_err(invalid_param)?
                .map(Some)
        };

        let patch = TaskPatch {
            title: args.title,
            notes: args.notes,
            when: args
                .when
                .as_deref()
                .map(When::parse)
                .transpose()
                .map_err(invalid_param)?,
            deadline,
            project,
            area,
            heading,
            tags,
            reminder_time,
            repeat,
        };

        let mut service = write_service_or_return!(self, dry_run);
        let outcome = service
            .update_task(&args.id, patch)
            .await
            .map_err(write_error)?;
        Ok(CallToolResult::structured(
            serde_json::to_value(outcome).map_err(internal)?,
        ))
    }

    #[tool(description = "Mark a task completed (ss=3, st=1, sp, md).")]
    async fn complete_task(
        &self,
        Parameters(args): Parameters<IdDryRunArgs>,
    ) -> Result<CallToolResult, McpError> {
        let dry_run = args.dry_run.unwrap_or(false);
        let mut service = write_service_or_return!(self, dry_run);
        let outcome = service.complete_task(&args.id).await.map_err(write_error)?;
        Ok(CallToolResult::structured(
            serde_json::to_value(outcome).map_err(internal)?,
        ))
    }

    #[tool(description = "Mark a task canceled (ss=2, sp, md).")]
    async fn cancel_task(
        &self,
        Parameters(args): Parameters<IdDryRunArgs>,
    ) -> Result<CallToolResult, McpError> {
        let dry_run = args.dry_run.unwrap_or(false);
        let mut service = write_service_or_return!(self, dry_run);
        let outcome = service.cancel_task(&args.id).await.map_err(write_error)?;
        Ok(CallToolResult::structured(
            serde_json::to_value(outcome).map_err(internal)?,
        ))
    }

    #[tool(description = "Reopen a completed or canceled task.")]
    async fn reopen_task(
        &self,
        Parameters(args): Parameters<IdDryRunArgs>,
    ) -> Result<CallToolResult, McpError> {
        let dry_run = args.dry_run.unwrap_or(false);
        let mut service = write_service_or_return!(self, dry_run);
        let outcome = service.reopen_task(&args.id).await.map_err(write_error)?;
        Ok(CallToolResult::structured(
            serde_json::to_value(outcome).map_err(internal)?,
        ))
    }

    #[tool(description = "Move a task to the trash (soft delete, tr=true).")]
    async fn trash_task(
        &self,
        Parameters(args): Parameters<IdDryRunArgs>,
    ) -> Result<CallToolResult, McpError> {
        let dry_run = args.dry_run.unwrap_or(false);
        let mut service = write_service_or_return!(self, dry_run);
        let outcome = service
            .trash_task(&args.id, false)
            .await
            .map_err(write_error)?;
        Ok(CallToolResult::structured(
            serde_json::to_value(outcome).map_err(internal)?,
        ))
    }

    #[tool(description = "Restore a task from the trash.")]
    async fn restore_task(
        &self,
        Parameters(args): Parameters<IdDryRunArgs>,
    ) -> Result<CallToolResult, McpError> {
        let dry_run = args.dry_run.unwrap_or(false);
        let mut service = write_service_or_return!(self, dry_run);
        let outcome = service
            .trash_task(&args.id, true)
            .await
            .map_err(write_error)?;
        Ok(CallToolResult::structured(
            serde_json::to_value(outcome).map_err(internal)?,
        ))
    }

    #[tool(description = "Permanently delete a task (t=2, empty payload). This cannot be undone.")]
    async fn delete_task(
        &self,
        Parameters(args): Parameters<IdDryRunArgs>,
    ) -> Result<CallToolResult, McpError> {
        let dry_run = args.dry_run.unwrap_or(false);
        let mut service = write_service_or_return!(self, dry_run);
        let outcome = service.delete_task(&args.id).await.map_err(write_error)?;
        Ok(CallToolResult::structured(
            serde_json::to_value(outcome).map_err(internal)?,
        ))
    }

    #[tool(description = "Add a checklist item to a task (ChecklistItem3, 9-key snapshot).")]
    async fn add_checklist_item(
        &self,
        Parameters(args): Parameters<ChecklistArgs>,
    ) -> Result<CallToolResult, McpError> {
        let dry_run = args.dry_run.unwrap_or(false);
        let mut service = write_service_or_return!(self, dry_run);
        let outcome = service
            .add_checklist_item(&args.task_id, &args.title)
            .await
            .map_err(write_error)?;
        Ok(CallToolResult::structured(
            serde_json::to_value(outcome).map_err(internal)?,
        ))
    }

    #[tool(description = "Rename a checklist item or move it to a new position (`ix`).")]
    async fn update_checklist_item(
        &self,
        Parameters(args): Parameters<UpdateChecklistArgs>,
    ) -> Result<CallToolResult, McpError> {
        let dry_run = args.dry_run.unwrap_or(false);
        let mut service = write_service_or_return!(self, dry_run);
        let outcome = service
            .update_checklist_item(&args.id, args.title, args.index)
            .await
            .map_err(write_error)?;
        Ok(CallToolResult::structured(
            serde_json::to_value(outcome).map_err(internal)?,
        ))
    }

    #[tool(
        description = "Complete or reopen a checklist item. Omit `completed` to flip its current state."
    )]
    async fn toggle_checklist_item(
        &self,
        Parameters(args): Parameters<ToggleChecklistArgs>,
    ) -> Result<CallToolResult, McpError> {
        let dry_run = args.dry_run.unwrap_or(false);
        let mut service = write_service_or_return!(self, dry_run);
        let outcome = service
            .toggle_checklist_item(&args.id, args.completed)
            .await
            .map_err(write_error)?;
        Ok(CallToolResult::structured(
            serde_json::to_value(outcome).map_err(internal)?,
        ))
    }

    #[tool(description = "Delete a checklist item.")]
    async fn delete_checklist_item(
        &self,
        Parameters(args): Parameters<IdDryRunArgs>,
    ) -> Result<CallToolResult, McpError> {
        let dry_run = args.dry_run.unwrap_or(false);
        let mut service = write_service_or_return!(self, dry_run);
        let outcome = service
            .delete_checklist_item(&args.id)
            .await
            .map_err(write_error)?;
        Ok(CallToolResult::structured(
            serde_json::to_value(outcome).map_err(internal)?,
        ))
    }

    #[tool(
        description = "Rename a tag, move it under another parent, or change/clear its shortcut."
    )]
    async fn update_tag(
        &self,
        Parameters(args): Parameters<UpdateTagArgs>,
    ) -> Result<CallToolResult, McpError> {
        let dry_run = args.dry_run.unwrap_or(false);
        let parent = if args.clear_parent.unwrap_or(false) {
            Some(None)
        } else {
            args.parent_id.map(Some)
        };
        let shortcut = if args.clear_shortcut.unwrap_or(false) {
            Some(None)
        } else {
            args.shortcut.map(Some)
        };

        let mut service = write_service_or_return!(self, dry_run);
        let outcome = service
            .update_tag(&args.id, args.title, parent, shortcut)
            .await
            .map_err(write_error)?;
        Ok(CallToolResult::structured(
            serde_json::to_value(outcome).map_err(internal)?,
        ))
    }

    #[tool(
        description = "Permanently delete a tag with no child tags. Tasks keep their data but lose the tag reference."
    )]
    async fn delete_tag(
        &self,
        Parameters(args): Parameters<IdDryRunArgs>,
    ) -> Result<CallToolResult, McpError> {
        let dry_run = args.dry_run.unwrap_or(false);
        let mut service = write_service_or_return!(self, dry_run);
        let outcome = service.delete_tag(&args.id).await.map_err(write_error)?;
        Ok(CallToolResult::structured(
            serde_json::to_value(outcome).map_err(internal)?,
        ))
    }

    #[tool(description = "Rename an area or replace its tag set.")]
    async fn update_area(
        &self,
        Parameters(args): Parameters<UpdateAreaArgs>,
    ) -> Result<CallToolResult, McpError> {
        let dry_run = args.dry_run.unwrap_or(false);
        let tags = if args.clear_tags.unwrap_or(false) {
            Some(Vec::new())
        } else {
            args.tags
        };
        let mut service = write_service_or_return!(self, dry_run);
        let outcome = service
            .update_area(&args.id, args.title, tags)
            .await
            .map_err(write_error)?;
        Ok(CallToolResult::structured(
            serde_json::to_value(outcome).map_err(internal)?,
        ))
    }

    #[tool(
        description = "Permanently delete an area that has no tasks or projects referencing it."
    )]
    async fn delete_area(
        &self,
        Parameters(args): Parameters<IdDryRunArgs>,
    ) -> Result<CallToolResult, McpError> {
        let dry_run = args.dry_run.unwrap_or(false);
        let mut service = write_service_or_return!(self, dry_run);
        let outcome = service.delete_area(&args.id).await.map_err(write_error)?;
        Ok(CallToolResult::structured(
            serde_json::to_value(outcome).map_err(internal)?,
        ))
    }

    #[tool(description = "Pull changes from Things Cloud and update the local replica now.")]
    async fn sync_now(
        &self,
        Parameters(args): Parameters<SyncNowArgs>,
    ) -> Result<CallToolResult, McpError> {
        let report = match self.refresh(true, args.full).await {
            Ok(Some(report)) => report,
            Ok(None) => {
                return Err(McpError::internal_error(
                    "sync did not run",
                    Some(json!({"code": "SYNC_SKIPPED"})),
                ));
            }
            Err(error) => {
                if error.message.to_lowercase().contains("incompatible") {
                    return Ok(tool_error(
                        "INCOMPATIBLE_VERSION",
                        error.message.to_string(),
                        json!({"do_not_retry": true}),
                    ));
                }
                return Err(error);
            }
        };
        let store = self.open_store_raw()?;
        let payload = json!({
            "history_key": report.history_key,
            "cursor": report.cursor,
            "head": report.head,
            "pages": report.pages,
            "items": report.items,
            "upserted": report.upserted,
            "deleted": report.deleted,
            "skipped": report.skipped,
            "unknown": report.unknown,
            "history_replaced": report.history_replaced,
            "tasks": store.task_count().map_err(internal)?,
        });
        Ok(CallToolResult::structured(payload))
    }

    #[tool(description = "Show sync state: history key, cursor, last sync, and list counts.")]
    async fn sync_status(&self) -> Result<CallToolResult, McpError> {
        let store = self.open_store_raw()?;
        let today = fmt::today_epoch_day();
        let counts = task_counts(store.connection(), today).map_err(internal)?;
        let counts: serde_json::Map<String, Value> = counts
            .into_iter()
            .map(|(name, count)| (name, json!(count)))
            .collect();

        let payload = json!({
            "database": self.state.config.db_path.display().to_string(),
            "history_key": store.history_key().map_err(internal)?,
            "cursor": store.cursor().map_err(internal)?,
            "head": store.latest_server_index().map_err(internal)?,
            "schema": store.schema_version().map_err(internal)?,
            "compat": serde_json::to_value(store.compat().map_err(internal)?).unwrap_or_default(),
            "last_sync_at": store.last_sync_at().map_err(internal)?,
            "tasks": store.task_count().map_err(internal)?,
            "counts": counts,
        });
        Ok(CallToolResult::structured(payload))
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct ListTasksArgs2 {
    /// Maximum number of tasks to return (1-500, default 100).
    pub limit: Option<usize>,
    /// Number of tasks to skip (default 0).
    pub offset: Option<usize>,
}

#[tool_handler]
impl rmcp::ServerHandler for ThingsServer {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(Implementation::new("things-mcp", env!("CARGO_PKG_VERSION")))
            .with_instructions(INSTRUCTIONS)
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        context: RequestContext<rmcp::RoleServer>,
    ) -> Result<CallToolResponse, McpError> {
        let tool = request.name.clone();
        let id = context.id.clone();
        let started = Instant::now();
        let pid = std::process::id();
        tracing::info!(target: DIAGNOSTIC_TARGET, pid, %id, %tool, "MCP tool started");
        let result = self
            .tool_router
            .call(ToolCallContext::new(self, request, context))
            .await;
        let elapsed_ms = started.elapsed().as_millis();
        match &result {
            Ok(response) => {
                let is_error = matches!(response, CallToolResponse::Complete(result) if result.is_error == Some(true));
                tracing::info!(target: DIAGNOSTIC_TARGET, pid, %id, %tool, elapsed_ms, is_error, "MCP tool finished");
            }
            Err(error) => {
                let kind = error.data.as_ref().and_then(|data| data["code"].as_str());
                tracing::info!(target: DIAGNOSTIC_TARGET, pid, %id, %tool, elapsed_ms, rpc_code = error.code.0, ?kind, "MCP tool failed");
            }
        }
        result
    }
}

fn clamp(limit: usize) -> usize {
    limit.clamp(1, MAX_LIMIT)
}

fn parse_list(value: &str) -> Result<ListKind, McpError> {
    ListKind::parse(value).ok_or_else(|| {
        McpError::invalid_params(
            format!("unknown list `{value}`"),
            Some(json!({
                "allowed": ["inbox","today","upcoming","anytime","someday","logbook","deadlines","repeating","trash","all","project-<id>","area-<id>","tag-<id>"]
            })),
        )
    })
}

fn not_found(kind: &str, id: &str) -> McpError {
    McpError::internal_error(
        format!("{kind} {id} not found in the local replica"),
        Some(json!({ "code": "NOT_FOUND" })),
    )
}

fn query_error(error: impl std::fmt::Display) -> CallToolResult {
    tool_error("QUERY_FAILED", error.to_string(), json!({}))
}

fn tool_error(code: &str, message: impl Into<String>, details: Value) -> CallToolResult {
    CallToolResult::structured_error(json!({
        "error": code,
        "message": message.into(),
        "details": details,
    }))
}

fn invalid_param(error: impl std::fmt::Display) -> McpError {
    McpError::invalid_params(error.to_string(), None)
}

fn write_error(error: anyhow::Error) -> McpError {
    let message = error.to_string();
    let code = if let Some(inner) = error.downcast_ref::<ThingsError>() {
        code_of(inner)
    } else {
        "WRITE_FAILED"
    };
    McpError::internal_error(message, Some(json!({ "code": code })))
}

fn internal(error: impl std::fmt::Display) -> McpError {
    McpError::internal_error(error.to_string(), None)
}

fn code_of(error: &ThingsError) -> &'static str {
    match error {
        ThingsError::Unauthorized => "AUTH_FAILED",
        ThingsError::NotFound(_) => "NOT_FOUND",
        ThingsError::RateLimited => "RATE_LIMITED",
        ThingsError::Stalled { .. } => "SYNC_STALLED",
        ThingsError::Server { .. } => "SERVER_ERROR",
        ThingsError::Protocol(_) => "PROTOCOL_ERROR",
        ThingsError::Conflict(_) => "SYNC_CONFLICT",
        ThingsError::IncompatibleVersion(_) => "INCOMPATIBLE_VERSION",
        ThingsError::Transport(_) => "NETWORK_ERROR",
    }
}

fn things_error(error: ThingsError) -> McpError {
    McpError::internal_error(error.to_string(), Some(json!({ "code": code_of(&error) })))
}

fn anyhow_to_mcp(error: anyhow::Error) -> McpError {
    let code = error
        .downcast_ref::<ThingsError>()
        .map(code_of)
        .unwrap_or("SYNC_FAILED");
    McpError::internal_error(error.to_string(), Some(json!({ "code": code })))
}

fn with_warning(mut payload: Value, warning: Option<String>) -> Value {
    if let Some(warning) = warning {
        payload["warning"] = Value::String(warning);
    }
    payload
}

/// Run the stdio MCP server until the client disconnects.
pub async fn run(config: Config, refresh_secs: u64) -> anyhow::Result<()> {
    let pid = std::process::id();
    tracing::info!(target: DIAGNOSTIC_TARGET, pid, version = env!("CARGO_PKG_VERSION"), "MCP stdio starting");
    let server = ThingsServer::new(config, refresh_secs);
    let service = server
        .serve(rmcp::transport::stdio())
        .await
        .inspect_err(|_| {
            tracing::error!(target: DIAGNOSTIC_TARGET, pid, "MCP stdio handshake failed");
        })?;
    tracing::info!(target: DIAGNOSTIC_TARGET, pid, "MCP stdio ready");
    match service.waiting().await {
        Ok(QuitReason::JoinError(error)) | Err(error) => {
            tracing::error!(target: DIAGNOSTIC_TARGET, pid, panic = error.is_panic(), "MCP stdio task failed");
            return Err(error.into());
        }
        Ok(reason) => {
            tracing::info!(target: DIAGNOSTIC_TARGET, pid, ?reason, "MCP stdio stopped");
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::things::{ACTION_CREATE, IncomingItem};

    fn seed(path: &std::path::Path) {
        let mut store = Store::open(path).unwrap();
        let items = vec![
            IncomingItem {
                uuid: "inbox-1".into(),
                action: ACTION_CREATE,
                kind: "Task7".into(),
                props: json!({"tt": "buy milk", "tp": 0, "ss": 0, "st": 0})
                    .as_object()
                    .unwrap()
                    .clone(),
                server_index: 0,
            },
            IncomingItem {
                uuid: "today-1".into(),
                action: ACTION_CREATE,
                kind: "Task7".into(),
                props: json!({"tt": "ship it", "tp": 0, "ss": 0, "st": 1})
                    .as_object()
                    .unwrap()
                    .clone(),
                server_index: 0,
            },
        ];
        store.apply_items(&items).unwrap();
    }

    fn server(path: &std::path::Path) -> ThingsServer {
        ThingsServer::new(
            Config {
                email: None,
                password: None,
                base_url: "http://127.0.0.1:9".into(),
                db_path: path.to_path_buf(),
            },
            0,
        )
    }

    #[test]
    fn list_payload_reads_the_local_replica() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("things.sqlite");
        seed(&db);

        let server = server(&db);
        let payload = server.list_payload(&ListKind::Inbox, 100, 0).unwrap();
        assert_eq!(payload["count"], 1);
        assert_eq!(payload["tasks"][0]["title"], "buy milk");
    }

    #[test]
    fn sync_status_reports_meta() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("things.sqlite");
        seed(&db);

        let server = server(&db);
        let store = server.open_store().unwrap();
        let today = fmt::today_epoch_day();
        let counts = task_counts(store.connection(), today).unwrap();
        let inbox = counts
            .iter()
            .find(|(name, _)| name == "inbox")
            .map(|(_, count)| *count);
        assert_eq!(inbox, Some(1));
        assert_eq!(store.task_count().unwrap(), 2);
    }

    #[test]
    fn list_payload_reports_total_and_has_more() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("things.sqlite");
        seed(&db);

        let server = server(&db);
        let first = server.list_payload(&ListKind::All, 1, 0).unwrap();
        assert_eq!(first["total"], 2);
        assert_eq!(first["count"], 1);
        assert_eq!(first["has_more"], true);

        let second = server.list_payload(&ListKind::All, 1, 1).unwrap();
        assert_eq!(second["count"], 1);
        assert_eq!(second["has_more"], false);
    }

    #[test]
    fn incompatible_replica_blocks_data_tools() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("things.sqlite");
        seed(&db);

        {
            let store = Store::open(&db).unwrap();
            store
                .set_compat_incompatible(
                    "cloud schema 302 (supported 301)",
                    Some(302),
                    &std::collections::BTreeMap::new(),
                )
                .unwrap();
        }

        let server = server(&db);
        assert!(server.open_store().is_err(), "data tools must be blocked");
        assert!(
            server.open_store_raw().is_ok(),
            "diagnostics must still work"
        );
    }

    #[test]
    fn unknown_list_is_rejected() {
        assert!(parse_list("nope").is_err());
        assert!(parse_list("project-abc").is_ok());
    }
}
