use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, Result, bail};
use clap::{ArgAction, Parser, Subcommand};
use serde::Serialize;
use tracing_subscriber::EnvFilter;

use things_mcp::config::Config;
use things_mcp::fmt;
use things_mcp::ids;
use things_mcp::store::{
    ListKind, SearchFilter, Store, count_tasks, list_areas, list_projects, list_tags, list_tasks,
    search_tasks, summaries, task_counts, task_detail,
};
use things_mcp::sync::SyncEngine;
use things_mcp::things::{ClientOptions, HttpTransport, TaskDraft, ThingsClient, When};
use things_mcp::write::{TaskPatch, WriteService};

#[derive(Parser, Debug)]
#[command(name = "things-mcp", version, about = "Headless Things Cloud client")]
struct Cli {
    /// Things account email (defaults to THINGS_EMAIL).
    #[arg(long, global = true, env = "THINGS_EMAIL")]
    email: Option<String>,

    /// SQLite database path (defaults to THINGS_DB or the per-user data dir).
    #[arg(long, global = true, env = "THINGS_DB")]
    db: Option<PathBuf>,

    /// Override the Things Cloud base URL (testing only).
    #[arg(long, global = true, env = "THINGS_BASE_URL")]
    base_url: Option<String>,

    #[arg(short, long, global = true, action = ArgAction::Count)]
    verbose: u8,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand, Debug)]
enum Command {
    /// Show account information and the current history key.
    Account {
        #[arg(long)]
        json: bool,
    },
    /// Pull new history items into the local replica.
    Sync {
        /// Discard the local replica and re-download everything.
        #[arg(long)]
        full: bool,
        #[arg(long)]
        json: bool,
    },
    /// Print task counts for every list (sanity check against the app).
    Lists {
        #[arg(long)]
        json: bool,
    },
    /// List tasks from one view.
    List {
        /// inbox|today|upcoming|anytime|someday|logbook|deadlines|repeating|trash|all|project-<id>|area-<id>|tag-<id>
        list: String,
        #[arg(long, default_value_t = 100)]
        limit: usize,
        #[arg(long, default_value_t = 0)]
        offset: usize,
    },
    /// Search tasks by title/notes with optional filters.
    Search {
        query: String,
        #[arg(long)]
        project: Option<String>,
        #[arg(long)]
        area: Option<String>,
        #[arg(long)]
        tag: Vec<String>,
        #[arg(long)]
        status: Option<String>,
        #[arg(long, default_value_t = false)]
        include_completed: bool,
        #[arg(long, default_value_t = 100)]
        limit: usize,
    },
    /// Show one task in detail.
    Show { id: String },
    /// List projects.
    Projects {
        #[arg(long)]
        area: Option<String>,
        #[arg(long, default_value = "open")]
        status: String,
        #[arg(long, default_value_t = 100)]
        limit: usize,
    },
    /// List areas.
    Areas {
        #[arg(long, default_value_t = 100)]
        limit: usize,
    },
    /// List tags.
    Tags {
        #[arg(long, default_value_t = 100)]
        limit: usize,
    },
    /// Counts + sample rows for every list; use this to compare with Things.app.
    Verify,
    /// Print the raw predicate diagnostics used to debug list mismatches.
    Diagnose {
        #[arg(long)]
        json: bool,
    },
    /// Audit decoded columns against the raw wire data and check references.
    Doctor {
        #[arg(long)]
        json: bool,
    },
    /// One-shot acceptance check: sync twice, require the second pull to be a
    /// no-op, then run the decode/reference audit.
    Check {
        #[arg(long)]
        json: bool,
    },
    /// Create a task. Requires THINGS_ENABLE_WRITES=1 unless --dry-run is used.
    Create {
        title: String,
        #[arg(long)]
        notes: Option<String>,
        #[arg(long, default_value = "inbox")]
        when: String,
        #[arg(long)]
        deadline: Option<String>,
        #[arg(long)]
        project: Option<String>,
        #[arg(long)]
        area: Option<String>,
        #[arg(long)]
        heading: Option<String>,
        #[arg(long = "tag")]
        tag: Vec<String>,
        #[arg(long)]
        dry_run: bool,
    },
    /// Create a project.
    CreateProject {
        title: String,
        #[arg(long)]
        notes: Option<String>,
        #[arg(long, default_value = "anytime")]
        when: String,
        #[arg(long)]
        deadline: Option<String>,
        #[arg(long)]
        area: Option<String>,
        #[arg(long = "tag")]
        tag: Vec<String>,
        #[arg(long)]
        dry_run: bool,
    },
    /// Create an area.
    CreateArea {
        title: String,
        #[arg(long)]
        dry_run: bool,
    },
    /// Create a tag.
    CreateTag {
        title: String,
        #[arg(long)]
        parent: Option<String>,
        #[arg(long)]
        shortcut: Option<String>,
        #[arg(long)]
        dry_run: bool,
    },
    /// Update a task; only the provided fields are sent.
    Update {
        id: String,
        #[arg(long)]
        title: Option<String>,
        #[arg(long)]
        notes: Option<String>,
        #[arg(long)]
        when: Option<String>,
        #[arg(long)]
        deadline: Option<String>,
        #[arg(long)]
        clear_deadline: bool,
        #[arg(long)]
        project: Option<String>,
        #[arg(long)]
        clear_project: bool,
        #[arg(long)]
        area: Option<String>,
        #[arg(long)]
        clear_area: bool,
        #[arg(long)]
        heading: Option<String>,
        #[arg(long)]
        clear_heading: bool,
        #[arg(long = "tag")]
        tags: Vec<String>,
        #[arg(long)]
        clear_tags: bool,
        #[arg(long)]
        dry_run: bool,
    },
    /// Mark a task completed (ss=3, st=1, sp, md).
    Complete {
        id: String,
        #[arg(long)]
        dry_run: bool,
    },
    /// Mark a task canceled.
    Cancel {
        id: String,
        #[arg(long)]
        dry_run: bool,
    },
    /// Reopen a completed/canceled task.
    Reopen {
        id: String,
        #[arg(long)]
        dry_run: bool,
    },
    /// Move a task to the trash (soft delete).
    Trash {
        id: String,
        #[arg(long)]
        dry_run: bool,
    },
    /// Restore a task from the trash.
    Restore {
        id: String,
        #[arg(long)]
        dry_run: bool,
    },
    /// Hard delete a task (`t=2`, empty payload).
    Delete {
        id: String,
        #[arg(long)]
        dry_run: bool,
    },
    /// Add a checklist item to a task.
    Checklist {
        task: String,
        title: String,
        #[arg(long)]
        dry_run: bool,
    },
    /// Run the MCP server over stdio.
    Serve {
        /// Seconds before a read tool triggers a background pull (0 = never).
        #[arg(long, default_value_t = 60, env = "THINGS_REFRESH_SECS")]
        refresh_secs: u64,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    init_tracing(cli.verbose);

    let config = Config::from_env(cli.email.clone(), cli.base_url.clone(), cli.db.clone())?;

    match cli.command {
        Command::Account { json } => account(&config, json).await,
        Command::Sync { full, json } => sync(&config, full, json).await,
        Command::Lists { json } => lists(&config, json),
        Command::List {
            list: name,
            limit,
            offset,
        } => list(&config, &name, limit, offset),
        Command::Search {
            query,
            project,
            area,
            tag,
            status,
            include_completed,
            limit,
        } => search(
            &config,
            &query,
            project,
            area,
            tag,
            status,
            include_completed,
            limit,
        ),
        Command::Show { id } => show(&config, &id),
        Command::Projects {
            area,
            status,
            limit,
        } => projects(&config, area.as_deref(), &status, limit),
        Command::Areas { limit } => areas(&config, limit),
        Command::Tags { limit } => tags(&config, limit),
        Command::Verify => verify(&config),
        Command::Diagnose { json } => diagnose(&config, json),
        Command::Doctor { json } => doctor(&config, json),
        Command::Check { json } => check(&config, json).await,
        Command::Create {
            title,
            notes,
            when,
            deadline,
            project,
            area,
            heading,
            tag,
            dry_run,
        } => {
            write_create(
                &config,
                &title,
                notes,
                &when,
                deadline.as_deref(),
                project,
                area,
                heading,
                tag,
                0,
                dry_run,
            )
            .await
        }
        Command::CreateProject {
            title,
            notes,
            when,
            deadline,
            area,
            tag,
            dry_run,
        } => {
            write_create(
                &config,
                &title,
                notes,
                &when,
                deadline.as_deref(),
                None,
                area,
                None,
                tag,
                1,
                dry_run,
            )
            .await
        }
        Command::CreateArea { title, dry_run } => write_create_area(&config, &title, dry_run).await,
        Command::CreateTag {
            title,
            parent,
            shortcut,
            dry_run,
        } => write_create_tag(&config, &title, parent, shortcut, dry_run).await,
        Command::Update {
            id,
            title,
            notes,
            when,
            deadline,
            clear_deadline,
            project,
            clear_project,
            area,
            clear_area,
            heading,
            clear_heading,
            tags,
            clear_tags,
            dry_run,
        } => {
            write_update(
                &config,
                &id,
                title,
                notes,
                when,
                deadline,
                clear_deadline,
                project,
                clear_project,
                area,
                clear_area,
                heading,
                clear_heading,
                tags,
                clear_tags,
                dry_run,
            )
            .await
        }
        Command::Complete { id, dry_run } => {
            write_status(&config, &id, StatusAction::Complete, dry_run).await
        }
        Command::Cancel { id, dry_run } => {
            write_status(&config, &id, StatusAction::Cancel, dry_run).await
        }
        Command::Reopen { id, dry_run } => {
            write_status(&config, &id, StatusAction::Reopen, dry_run).await
        }
        Command::Trash { id, dry_run } => {
            write_status(&config, &id, StatusAction::Trash, dry_run).await
        }
        Command::Restore { id, dry_run } => {
            write_status(&config, &id, StatusAction::Restore, dry_run).await
        }
        Command::Delete { id, dry_run } => {
            write_status(&config, &id, StatusAction::Delete, dry_run).await
        }
        Command::Checklist {
            task,
            title,
            dry_run,
        } => write_checklist(&config, &task, &title, dry_run).await,
        Command::Serve { refresh_secs } => things_mcp::mcp::run(config, refresh_secs).await,
    }
}

fn init_tracing(verbose: u8) {
    let default = if verbose > 0 {
        "things_mcp=debug,info"
    } else {
        "things_mcp=info,warn"
    };
    let filter = std::env::var("RUST_LOG").unwrap_or_else(|_| default.to_string());
    let _ = tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::new(filter))
        .with_writer(std::io::stderr)
        .with_target(false)
        .try_init();
}

async fn write_service(config: &Config, dry_run: bool) -> Result<WriteService> {
    let store = open_store(config)?;
    if dry_run {
        // Previewing a payload needs neither credentials nor the network.
        let client = ThingsClient::new(
            Arc::new(HttpTransport::new()?),
            ClientOptions::new("dry-run", "dry-run", "dry-run"),
        );
        return Ok(WriteService::new(SyncEngine::new(client, store), false));
    }

    if !things_mcp::write::writes_enabled_from_env() {
        bail!(
            "writes are disabled: set THINGS_ENABLE_WRITES=1 to push changes (--dry-run is always allowed)"
        );
    }

    let client = build_client(config, &store)?;
    client.login().await.context("login failed")?;
    Ok(WriteService::from_env(SyncEngine::new(client, store)))
}

#[allow(clippy::too_many_arguments)]
async fn write_create(
    config: &Config,
    title: &str,
    notes: Option<String>,
    when: &str,
    deadline: Option<&str>,
    project: Option<String>,
    area: Option<String>,
    heading: Option<String>,
    tags: Vec<String>,
    task_type: i64,
    dry_run: bool,
) -> Result<()> {
    let mut draft = TaskDraft::new(title);
    draft.task_type = task_type;
    draft.notes = notes.unwrap_or_default();
    draft.when = When::parse(when)?;
    draft.deadline = deadline.map(things_mcp::things::parse_day).transpose()?;
    draft.project = project;
    draft.area = area;
    draft.heading = heading;
    draft.tags = tags;

    let mut service = write_service(config, dry_run).await?;
    let outcome = service.create_task(draft, dry_run).await?;
    print_json(&outcome)
}

async fn write_create_area(config: &Config, title: &str, dry_run: bool) -> Result<()> {
    let mut service = write_service(config, dry_run).await?;
    print_json(&service.create_area(title, dry_run).await?)
}

async fn write_create_tag(
    config: &Config,
    title: &str,
    parent: Option<String>,
    shortcut: Option<String>,
    dry_run: bool,
) -> Result<()> {
    let mut service = write_service(config, dry_run).await?;
    print_json(
        &service
            .create_tag(title, parent.as_deref(), shortcut.as_deref(), dry_run)
            .await?,
    )
}

#[allow(clippy::too_many_arguments)]
async fn write_update(
    config: &Config,
    id: &str,
    title: Option<String>,
    notes: Option<String>,
    when: Option<String>,
    deadline: Option<String>,
    clear_deadline: bool,
    project: Option<String>,
    clear_project: bool,
    area: Option<String>,
    clear_area: bool,
    heading: Option<String>,
    clear_heading: bool,
    tags: Vec<String>,
    clear_tags: bool,
    dry_run: bool,
) -> Result<()> {
    let deadline = if clear_deadline {
        Some(None)
    } else {
        deadline
            .as_deref()
            .map(things_mcp::things::parse_day)
            .transpose()?
            .map(Some)
    };
    let project = if clear_project {
        Some(None)
    } else {
        project.map(Some)
    };
    let area = if clear_area {
        Some(None)
    } else {
        area.map(Some)
    };
    let heading = if clear_heading {
        Some(None)
    } else {
        heading.map(Some)
    };
    let tags = if clear_tags {
        Some(Vec::new())
    } else if tags.is_empty() {
        None
    } else {
        Some(tags)
    };

    let patch = TaskPatch {
        title,
        notes,
        when: when.as_deref().map(When::parse).transpose()?,
        deadline,
        project,
        area,
        heading,
        tags,
    };

    let mut service = write_service(config, dry_run).await?;
    print_json(&service.update_task(id, patch, dry_run).await?)
}

#[derive(Clone, Copy)]
enum StatusAction {
    Complete,
    Cancel,
    Reopen,
    Trash,
    Restore,
    Delete,
}

async fn write_status(
    config: &Config,
    id: &str,
    action: StatusAction,
    dry_run: bool,
) -> Result<()> {
    let mut service = write_service(config, dry_run).await?;
    let outcome = match action {
        StatusAction::Complete => service.complete_task(id, dry_run).await?,
        StatusAction::Cancel => service.cancel_task(id, dry_run).await?,
        StatusAction::Reopen => service.reopen_task(id, dry_run).await?,
        StatusAction::Trash => service.trash_task(id, false, dry_run).await?,
        StatusAction::Restore => service.trash_task(id, true, dry_run).await?,
        StatusAction::Delete => service.delete_task(id, dry_run).await?,
    };
    print_json(&outcome)
}

async fn write_checklist(config: &Config, task: &str, title: &str, dry_run: bool) -> Result<()> {
    let mut service = write_service(config, dry_run).await?;
    print_json(&service.add_checklist_item(task, title, dry_run).await?)
}

fn open_store(config: &Config) -> Result<Store> {
    Store::open(&config.db_path)
}

fn build_client(config: &Config, store: &Store) -> Result<ThingsClient> {
    let app_instance_id = match store.app_instance_id()? {
        Some(value) if !value.is_empty() => value,
        _ => {
            let value = ids::new_uuid();
            store.set_app_instance_id(&value)?;
            value
        }
    };

    let mut options = ClientOptions::new(config.email()?, config.password()?, app_instance_id);
    options.base_url = config.base_url.clone();

    Ok(ThingsClient::new(Arc::new(HttpTransport::new()?), options))
}

async fn account(config: &Config, json: bool) -> Result<()> {
    let store = open_store(config)?;
    let client = build_client(config, &store)?;
    let info = client.login().await.context("login failed")?;
    let own = client.own_history_key().await.ok();

    let value = serde_json::json!({
        "email": info.email,
        "status": info.status,
        "sla_version": info.sla_version,
        "history_key": info.history_key,
        "is_empty": own.as_ref().map(|key| key.is_empty),
        "latest_server_index": own.as_ref().map(|key| key.latest_server_index),
        "database": config.db_path.display().to_string(),
    });

    if json {
        print_json(&value)
    } else {
        println!("email:          {}", info.email);
        println!("status:         {}", info.status);
        println!("history-key:    {}", info.history_key);
        if let Some(own) = &own {
            println!("is-empty:       {}", own.is_empty);
            println!("server-index:   {}", own.latest_server_index);
        }
        println!("database:       {}", config.db_path.display());
        Ok(())
    }
}

async fn sync(config: &Config, full: bool, json: bool) -> Result<()> {
    let store = open_store(config)?;
    let client = build_client(config, &store)?;
    client.login().await.context("login failed")?;

    let mut engine = SyncEngine::new(client, store);
    let report = engine.sync(full).await?;

    if json {
        let value = serde_json::json!({
            "history_key": report.history_key,
            "cursor": report.cursor,
            "head": report.head,
            "pages": report.pages,
            "items": report.items,
            "upserted": report.upserted,
            "deleted": report.deleted,
            "skipped": report.skipped,
            "unknown": report.unknown,
            "full_reset": report.full_reset,
            "history_replaced": report.history_replaced,
            "tasks": engine.store().task_count()?,
        });
        print_json(&value)
    } else {
        println!("history-key:    {}", report.history_key);
        println!("cursor:         {} / head {}", report.cursor, report.head);
        println!("pages:          {}", report.pages);
        println!("items:          {}", report.items);
        println!(
            "applied:        {} upserted, {} deleted, {} skipped, {} unknown",
            report.upserted, report.deleted, report.skipped, report.unknown
        );
        if report.history_replaced {
            println!("note:           cloud history was replaced; local replica reset");
        }
        println!("tasks:          {}", engine.store().task_count()?);
        Ok(())
    }
}

fn lists(config: &Config, json: bool) -> Result<()> {
    let store = open_store(config)?;
    let today = fmt::today_epoch_day();
    let counts = task_counts(store.connection(), today)?;

    if json {
        let map: serde_json::Map<String, serde_json::Value> = counts
            .iter()
            .map(|(name, count)| (name.clone(), serde_json::json!(count)))
            .collect();
        print_json(&serde_json::json!({
            "database": config.db_path.display().to_string(),
            "last_sync_at": store.last_sync_at()?,
            "counts": map,
        }))
    } else {
        println!("database: {}", config.db_path.display());
        if let Some(last) = store.last_sync_at()? {
            println!("last sync: {last}");
        }
        if store.task_count()? == 0 {
            println!("\nlocal replica is empty; run `things-mcp sync` first");
        }
        println!();
        for (name, count) in counts {
            println!("{name:<12} {count}");
        }
        Ok(())
    }
}

fn list(config: &Config, name: &str, limit: usize, offset: usize) -> Result<()> {
    let kind = ListKind::parse(name)
        .with_context(|| format!("unknown list `{name}`; see `things-mcp list --help`"))?;
    let store = open_store(config)?;
    let conn = store.connection();
    let today = fmt::today_epoch_day();

    match &kind {
        ListKind::Projects => {
            let projects = list_projects(conn, None, Some("open"), limit)?;
            print_json(&serde_json::json!({
                "list": "projects",
                "count": projects.len(),
                "projects": projects,
            }))
        }
        ListKind::Areas => {
            let areas = list_areas(conn, limit)?;
            print_json(&serde_json::json!({
                "list": "areas",
                "count": areas.len(),
                "areas": areas,
            }))
        }
        _ => {
            let total = count_tasks(conn, &kind, today)?;
            let tasks = list_tasks(conn, &kind, today, limit, offset)?;
            let items = summaries(conn, &tasks, today)?;
            let seen = offset as i64 + items.len() as i64;
            print_json(&serde_json::json!({
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

#[allow(clippy::too_many_arguments)]
fn search(
    config: &Config,
    query: &str,
    project: Option<String>,
    area: Option<String>,
    tag: Vec<String>,
    status: Option<String>,
    include_completed: bool,
    limit: usize,
) -> Result<()> {
    let store = open_store(config)?;
    let today = fmt::today_epoch_day();
    let status = match status.as_deref() {
        None => None,
        Some("open") => Some(0),
        Some("canceled") => Some(2),
        Some("completed") => Some(3),
        Some(other) => bail!("unknown status `{other}` (open|canceled|completed)"),
    };

    let filter = SearchFilter {
        query: Some(query.to_string()),
        status,
        project_id: project,
        area_id: area,
        tag_ids: tag,
        include_completed,
        ..Default::default()
    };

    let tasks = search_tasks(store.connection(), &filter, limit, 0)?;
    let items = summaries(store.connection(), &tasks, today)?;
    print_json(&serde_json::json!({
        "query": query,
        "count": items.len(),
        "tasks": items,
    }))
}

fn show(config: &Config, id: &str) -> Result<()> {
    let store = open_store(config)?;
    let today = fmt::today_epoch_day();
    match task_detail(store.connection(), id, today)? {
        Some(detail) => print_json(&detail),
        None => bail!("task `{id}` not found in the local replica"),
    }
}

fn projects(config: &Config, area: Option<&str>, status: &str, limit: usize) -> Result<()> {
    let store = open_store(config)?;
    let projects = list_projects(store.connection(), area, Some(status), limit)?;
    print_json(&serde_json::json!({
        "count": projects.len(),
        "projects": projects,
    }))
}

fn areas(config: &Config, limit: usize) -> Result<()> {
    let store = open_store(config)?;
    let areas = list_areas(store.connection(), limit)?;
    print_json(&serde_json::json!({
        "count": areas.len(),
        "areas": areas,
    }))
}

fn tags(config: &Config, limit: usize) -> Result<()> {
    let store = open_store(config)?;
    let tags = list_tags(store.connection(), limit)?;
    print_json(&serde_json::json!({
        "count": tags.len(),
        "tags": tags,
    }))
}

fn diagnose(config: &Config, json: bool) -> Result<()> {
    let store = open_store(config)?;
    let today = fmt::today_epoch_day();
    let diagnostics = things_mcp::store::diagnose::run(store.connection(), today)?;

    if json {
        return print_json(&serde_json::json!({
            "database": config.db_path.display().to_string(),
            "today_epoch_day": today,
            "totals": diagnostics.totals,
            "candidates": diagnostics.candidates,
        }));
    }

    println!("database: {}", config.db_path.display());
    println!("today:    {today} (epoch day)");
    println!();
    println!("totals:");
    for (label, count) in &diagnostics.totals {
        println!("  {label:<28} {count}");
    }
    println!();
    println!("predicate candidates:");
    for (label, count) in &diagnostics.candidates {
        println!("  {label:<28} {count}");
    }
    Ok(())
}

async fn check(config: &Config, json: bool) -> Result<()> {
    let store = open_store(config)?;
    let client = build_client(config, &store)?;
    client.login().await.context("login failed")?;

    let mut engine = SyncEngine::new(client, store);
    let before_tasks = engine.store().task_count()?;

    let first = engine.sync(false).await?;
    let cursor_after_first = engine.store().cursor()?;
    let second = engine.sync(false).await?;
    let after_tasks = engine.store().task_count()?;

    let no_op = second.pages == 0
        && second.items == 0
        && second.upserted == 0
        && second.deleted == 0
        && engine.store().cursor()? == cursor_after_first;
    let data_stable = before_tasks == after_tasks;
    let audit = things_mcp::store::audit::audit(engine.store().connection())?;
    let audit_ok = audit.iter().all(|result| result.ok);

    if json {
        print_json(&serde_json::json!({
            "no_op": no_op,
            "data_stable": data_stable,
            "audit_ok": audit_ok,
            "tasks_before": before_tasks,
            "tasks_after": after_tasks,
            "first_sync": {
                "pages": first.pages,
                "items": first.items,
                "upserted": first.upserted,
                "deleted": first.deleted,
            },
            "second_sync": {
                "pages": second.pages,
                "items": second.items,
                "upserted": second.upserted,
                "deleted": second.deleted,
            },
            "cursor": engine.store().cursor()?,
            "head": engine.store().latest_server_index()?,
            "audit": audit,
        }))?;
    } else {
        println!("database:      {}", config.db_path.display());
        println!("history:       {}", second.history_key);
        println!(
            "cursor / head: {} / {}",
            engine.store().cursor()?,
            engine
                .store()
                .latest_server_index()?
                .map(|head| head.to_string())
                .unwrap_or_else(|| "?".to_string())
        );
        println!(
            "first sync:    pages {} items {} (upserted {}, deleted {})",
            first.pages, first.items, first.upserted, first.deleted
        );
        println!(
            "second sync:   pages {} items {} (upserted {}, deleted {})",
            second.pages, second.items, second.upserted, second.deleted
        );
        println!("tasks:         {before_tasks} -> {after_tasks}");
        println!();
        println!("no-op:         {}", if no_op { "PASS" } else { "FAIL" });
        println!(
            "data stable:   {}",
            if data_stable { "PASS" } else { "FAIL" }
        );
        println!("audit:         {}", if audit_ok { "PASS" } else { "FAIL" });
        if !audit_ok {
            for result in audit.iter().filter(|result| !result.ok) {
                println!("  {}: {} ({:?})", result.name, result.count, result.samples);
            }
        }
    }

    if !(no_op && data_stable && audit_ok) {
        anyhow::bail!("acceptance check failed");
    }
    Ok(())
}

fn doctor(config: &Config, json: bool) -> Result<()> {
    let store = open_store(config)?;
    let results = things_mcp::store::audit::audit(store.connection())?;

    if json {
        return print_json(&serde_json::json!({
            "database": config.db_path.display().to_string(),
            "checks": results,
        }));
    }

    println!("database: {}", config.db_path.display());
    println!();
    let mut failures = 0;
    for result in &results {
        let mark = if result.ok { "ok" } else { "FAIL" };
        if !result.ok {
            failures += 1;
        }
        println!("{:<32} {mark} ({})", result.name, result.count);
        for sample in &result.samples {
            println!("    sample: {sample}");
        }
    }
    println!();
    if failures == 0 {
        println!("all checks passed");
    } else {
        println!("{failures} check(s) failed");
    }
    Ok(())
}

fn verify(config: &Config) -> Result<()> {
    let store = open_store(config)?;
    let conn = store.connection();
    let today = fmt::today_epoch_day();
    let counts = task_counts(conn, today)?;

    println!("database:  {}", config.db_path.display());
    println!(
        "history:   {}",
        store
            .history_key()?
            .unwrap_or_else(|| "(not synced)".to_string())
    );
    println!(
        "cursor:    {} / head {}",
        store.cursor()?,
        store
            .latest_server_index()?
            .map(|head| head.to_string())
            .unwrap_or_else(|| "?".to_string())
    );
    println!("schema:    {}", store.schema_version()?);
    println!(
        "last sync: {}",
        store.last_sync_at()?.unwrap_or_else(|| "never".to_string())
    );
    println!("tasks:     {}", store.task_count()?);
    println!();

    let list_names = [
        "inbox",
        "today",
        "upcoming",
        "anytime",
        "someday",
        "logbook",
        "deadlines",
        "repeating",
        "trash",
        "projects",
        "areas",
        "tags",
    ];

    for (name, count) in &counts {
        println!("{name:<12} {count}");
    }
    println!();
    println!("samples (compare with Things.app):");

    for name in list_names.iter().take(9) {
        let Some(kind) = ListKind::parse(name) else {
            continue;
        };
        let tasks = list_tasks(conn, &kind, today, 3, 0)?;
        let titles: Vec<&str> = tasks.iter().map(|task| task.title.as_str()).collect();
        println!("  {name:<10} {:?}", titles);
    }

    // If a count disagrees with Things.app, these alternatives show which
    // predicate is wrong (see `diagnose` for the full report).
    let diagnostics = things_mcp::store::diagnose::run(conn, today)?;
    println!();
    println!("predicate candidates (for debugging list mismatches):");
    for (label, count) in &diagnostics.candidates {
        println!("  {label:<28} {count}");
    }

    Ok(())
}

fn print_json<T: Serialize>(value: &T) -> Result<()> {
    println!("{}", serde_json::to_string_pretty(value)?);
    Ok(())
}
