//! MCP server (stdio transport).
//!
//! Read-only phase: the tools expose the local SQLite replica. Mutating tools
//! (`create_task`, `update_task`, ...) land in Phase 2 once the gold-standard
//! create payloads and the push safety gate are in place.

use std::sync::Arc;

use rmcp::handler::server::router::tool::ToolRouter;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{CallToolResult, Implementation, ServerCapabilities, ServerConfig};
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
use crate::things::{ClientOptions, HttpTransport, ThingsClient, ThingsError};

const DEFAULT_LIMIT: usize = 100;
const MAX_LIMIT: usize = 500;
const INSTRUCTIONS: &str = "\
Things Cloud read-only tools. Everything is served from a local SQLite replica \
that is refreshed lazily before tool calls; call `sync_now` to force a pull.\n\
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
    #[allow(dead_code, reason = "read by the tool_handler macro")]
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

    fn open_store(&self) -> Result<Store, McpError> {
        Store::open(&self.state.config.db_path).map_err(internal)
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
        let store = self.open_store()?;
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

        let store = self.open_store()?;
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
    ) -> Result<Value, McpError> {
        let store = self.open_store()?;
        let conn = store.connection();
        let today = fmt::today_epoch_day();
        let limit = clamp(limit);

        match kind {
            ListKind::Projects => {
                let projects = list_projects(conn, None, Some("open"), limit).map_err(internal)?;
                Ok(json!({
                    "list": "projects",
                    "count": projects.len(),
                    "projects": projects,
                }))
            }
            ListKind::Areas => {
                let areas = list_areas(conn, limit).map_err(internal)?;
                Ok(json!({
                    "list": "areas",
                    "count": areas.len(),
                    "areas": areas,
                }))
            }
            _ => {
                let total = count_tasks(conn, kind, today).map_err(internal)?;
                let tasks = list_tasks(conn, kind, today, limit, offset).map_err(internal)?;
                let items = summaries(conn, &tasks, today).map_err(internal)?;
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
pub struct SyncNowArgs {
    /// Discard the local replica and re-download the full history.
    #[serde(default)]
    pub full: bool,
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
        let payload = self.list_payload(
            &kind,
            args.limit.unwrap_or(DEFAULT_LIMIT),
            args.offset.unwrap_or(0),
        )?;
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
        let store = self.open_store()?;
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
        let store = self.open_store()?;
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
        let store = self.open_store()?;
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
        let store = self.open_store()?;
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
        let store = self.open_store()?;
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
        let payload = self.list_payload(
            &ListKind::Logbook,
            args.limit.unwrap_or(DEFAULT_LIMIT),
            args.offset.unwrap_or(0),
        )?;
        Ok(CallToolResult::structured(with_warning(payload, warning)))
    }

    #[tool(description = "List open tasks that have a deadline.")]
    async fn list_deadlines(
        &self,
        Parameters(args): Parameters<ListTasksArgs2>,
    ) -> Result<CallToolResult, McpError> {
        let warning = self.ensure_fresh().await;
        let payload = self.list_payload(
            &ListKind::Deadlines,
            args.limit.unwrap_or(DEFAULT_LIMIT),
            args.offset.unwrap_or(0),
        )?;
        Ok(CallToolResult::structured(with_warning(payload, warning)))
    }

    #[tool(description = "List tasks scheduled for a future date (Upcoming).")]
    async fn list_upcoming(
        &self,
        Parameters(args): Parameters<ListTasksArgs2>,
    ) -> Result<CallToolResult, McpError> {
        let warning = self.ensure_fresh().await;
        let payload = self.list_payload(
            &ListKind::Upcoming,
            args.limit.unwrap_or(DEFAULT_LIMIT),
            args.offset.unwrap_or(0),
        )?;
        Ok(CallToolResult::structured(with_warning(payload, warning)))
    }

    #[tool(description = "List tasks of one project.")]
    async fn list_by_project(
        &self,
        Parameters(args): Parameters<ListByIdArgs>,
    ) -> Result<CallToolResult, McpError> {
        let warning = self.ensure_fresh().await;
        let payload = self.list_payload(
            &ListKind::Project(args.id),
            args.limit.unwrap_or(DEFAULT_LIMIT),
            args.offset.unwrap_or(0),
        )?;
        Ok(CallToolResult::structured(with_warning(payload, warning)))
    }

    #[tool(description = "List tasks of one area.")]
    async fn list_by_area(
        &self,
        Parameters(args): Parameters<ListByIdArgs>,
    ) -> Result<CallToolResult, McpError> {
        let warning = self.ensure_fresh().await;
        let payload = self.list_payload(
            &ListKind::Area(args.id),
            args.limit.unwrap_or(DEFAULT_LIMIT),
            args.offset.unwrap_or(0),
        )?;
        Ok(CallToolResult::structured(with_warning(payload, warning)))
    }

    #[tool(description = "List tasks carrying one tag.")]
    async fn list_by_tag(
        &self,
        Parameters(args): Parameters<ListByIdArgs>,
    ) -> Result<CallToolResult, McpError> {
        let warning = self.ensure_fresh().await;
        let payload = self.list_payload(
            &ListKind::Tag(args.id),
            args.limit.unwrap_or(DEFAULT_LIMIT),
            args.offset.unwrap_or(0),
        )?;
        Ok(CallToolResult::structured(with_warning(payload, warning)))
    }

    #[tool(description = "Get one project: notes, headings, and its open tasks.")]
    async fn get_project(
        &self,
        Parameters(args): Parameters<GetByIdArgs>,
    ) -> Result<CallToolResult, McpError> {
        let warning = self.ensure_fresh().await;
        let store = self.open_store()?;
        let today = fmt::today_epoch_day();
        let limit = clamp(args.limit.unwrap_or(DEFAULT_LIMIT));
        let detail = project_detail(store.connection(), &args.id, today, limit)
            .map_err(internal)?
            .ok_or_else(|| not_found("project", &args.id))?;
        let payload = serde_json::to_value(detail).map_err(internal)?;
        Ok(CallToolResult::structured(with_warning(payload, warning)))
    }

    #[tool(description = "Get one area: its projects and open tasks.")]
    async fn get_area(
        &self,
        Parameters(args): Parameters<GetByIdArgs>,
    ) -> Result<CallToolResult, McpError> {
        let warning = self.ensure_fresh().await;
        let store = self.open_store()?;
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
        let store = self.open_store()?;
        let today = fmt::today_epoch_day();
        let limit = clamp(args.limit.unwrap_or(DEFAULT_LIMIT));
        let detail = tag_detail(store.connection(), &args.id, today, limit)
            .map_err(internal)?
            .ok_or_else(|| not_found("tag", &args.id))?;
        let payload = serde_json::to_value(detail).map_err(internal)?;
        Ok(CallToolResult::structured(with_warning(payload, warning)))
    }

    #[tool(description = "Pull changes from Things Cloud and update the local replica now.")]
    async fn sync_now(
        &self,
        Parameters(args): Parameters<SyncNowArgs>,
    ) -> Result<CallToolResult, McpError> {
        let report = self.refresh(true, args.full).await?.ok_or_else(|| {
            McpError::internal_error("sync did not run", Some(json!({"code": "SYNC_SKIPPED"})))
        })?;
        let store = self.open_store()?;
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
        let store = self.open_store()?;
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
    let server = ThingsServer::new(config, refresh_secs);
    let service = server.serve(rmcp::transport::stdio()).await?;
    service.waiting().await?;
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
    fn unknown_list_is_rejected() {
        assert!(parse_list("nope").is_err());
        assert!(parse_list("project-abc").is_ok());
    }
}
