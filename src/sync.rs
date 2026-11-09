//! Synchronisation engine: pull history pages and apply them to SQLite.
//!
//! Cursor semantics (verified against the reference implementation):
//!
//! * `current-item-index` is the server **head**, not the end of the page;
//! * the cursor advances by the number of top-level batches (`items.len()`),
//!   because one batch can expand into several entity items;
//! * never request `start-index` beyond the head: the backend answers HTTP 500;
//! * an empty page below the head means the pull stalled and must be retried.

use std::collections::BTreeMap;
use std::time::Duration;

use anyhow::{Result, bail};

use crate::store::{CompatCheck, Store};
use crate::things::{
    HistoryMeta, IncomingItem, SUPPORTED_SCHEMA, ThingsClient, ThingsError, parse_entity_name,
    supported_entity_max,
};

const MAX_PAGES: usize = 5_000;
const MAX_EMPTY_PAGES: usize = 3;

pub struct SyncEngine {
    client: ThingsClient,
    store: Store,
}

#[derive(Debug, Clone, Default)]
pub struct SyncReport {
    pub history_key: String,
    pub cursor: i64,
    pub head: i64,
    pub pages: usize,
    pub items: usize,
    pub upserted: u64,
    pub deleted: u64,
    pub skipped: u64,
    pub unknown: u64,
    pub full_reset: bool,
    pub history_replaced: bool,
    pub(crate) generation: i64,
}

impl SyncEngine {
    pub fn new(client: ThingsClient, store: Store) -> Self {
        Self { client, store }
    }

    pub fn client(&self) -> &ThingsClient {
        &self.client
    }

    pub fn store(&self) -> &Store {
        &self.store
    }

    pub fn store_mut(&mut self) -> &mut Store {
        &mut self.store
    }

    pub async fn sync(&mut self, full: bool) -> Result<SyncReport> {
        let mut check = self.store.begin_compat_check()?;
        let state = self.store.sync_state()?;
        let (history_key, meta, mut history_replaced) =
            resolve_history(&self.client, state.history_key.as_deref()).await?;
        if state.cursor > meta.latest_server_index {
            tracing::warn!(
                cursor = state.cursor,
                head = meta.latest_server_index,
                "local cursor is ahead of the server head; rebuilding the local replica"
            );
            history_replaced = true;
        }

        let mut report = SyncReport {
            history_key,
            head: meta.latest_server_index,
            full_reset: full,
            history_replaced,
            ..Default::default()
        };
        let pull_result = if full || history_replaced {
            let directory = tempfile::tempdir()?;
            let path = directory.path().join("replica.sqlite");
            let mut staged = Store::open(&path)?;
            staged.bind_history(&report.history_key)?;
            report.generation = staged.sync_state()?.generation;
            let result = pull_history(
                &self.client,
                &mut staged,
                &mut report,
                meta.latest_schema_version,
                &mut check,
            )
            .await;
            if result.is_ok() {
                drop(staged);
                self.store
                    .replace_replica(&path, &state, report.head, &check)?;
                report.generation = state.generation + 1;
            }
            result
        } else {
            if state.history_key.as_deref() != Some(report.history_key.as_str()) {
                self.store.bind_history(&report.history_key)?;
            }
            report.cursor = state.cursor;
            report.generation = state.generation;
            pull_history(
                &self.client,
                &mut self.store,
                &mut report,
                meta.latest_schema_version,
                &mut check,
            )
            .await
        };

        if let Err(error) = pull_result {
            if let Some(ThingsError::IncompatibleVersion(reason)) = error.downcast_ref() {
                self.store.record_incompatible(&check, reason)?;
            }
            return Err(error);
        }
        if !(full || history_replaced) {
            self.store
                .finish_sync(&report.history_key, report.generation, report.head, &check)?;
        }
        tracing::info!(
            pages = report.pages,
            items = report.items,
            cursor = report.cursor,
            head = report.head,
            "sync finished"
        );
        Ok(report)
    }
}

async fn resolve_history(
    client: &ThingsClient,
    stored: Option<&str>,
) -> Result<(String, HistoryMeta, bool)> {
    let history_key = match stored.filter(|key| !key.is_empty()) {
        Some(key) => key.to_owned(),
        None => client.own_history_key().await?.history_key,
    };
    anyhow::ensure!(
        !history_key.is_empty(),
        "server returned an empty history key"
    );

    match client.history_meta(&history_key).await {
        Ok(meta) => Ok((history_key, meta, false)),
        Err(ThingsError::NotFound(found)) => {
            tracing::warn!(path = %found, "history key not found; checking for a replaced history");
            let own = client.own_history_key().await?;
            if own.history_key == history_key || own.history_key.is_empty() {
                bail!("history {history_key} is not available on the server");
            }
            let meta = client.history_meta(&own.history_key).await?;
            Ok((own.history_key, meta, true))
        }
        Err(error) => Err(error.into()),
    }
}

async fn pull_history(
    client: &ThingsClient,
    store: &mut Store,
    report: &mut SyncReport,
    schema: i64,
    check: &mut CompatCheck,
) -> Result<()> {
    let history_key = &report.history_key;
    let mut cursor = report.cursor;
    let mut head = report.head;
    tracing::info!(history_key = %history_key, cursor, head, "sync started");
    check_schema(schema, check)?;

    let mut pages = 0usize;
    let mut empty_pages = 0usize;

    while cursor < head {
        if pages >= MAX_PAGES {
            bail!("pull exceeded {MAX_PAGES} pages; aborting to avoid a loop");
        }

        let page = client.items_page(history_key, cursor as usize).await?;

        check_schema(page.schema, check)?;
        check_items(&page.items, &mut check.entities)
            .map_err(|error| ThingsError::IncompatibleVersion(error.to_string()))?;

        if page.batches == 0 {
            empty_pages += 1;
            if empty_pages > MAX_EMPTY_PAGES {
                return Err(ThingsError::Stalled {
                    cursor: cursor as usize,
                    head,
                }
                .into());
            }
            tracing::debug!(
                cursor,
                head,
                "empty page below head; re-reading history meta"
            );
            let refreshed = client.history_meta(history_key).await?;
            check_schema(refreshed.latest_schema_version, check)?;
            head = head.max(refreshed.latest_server_index);
            tokio::time::sleep(Duration::from_millis(250)).await;
            continue;
        }
        empty_pages = 0;
        head = head.max(page.head);

        let next = cursor + page.batches as i64;
        match store.apply_history_page(cursor, &page.items, next, history_key, report.generation)? {
            Some(stats) => {
                report.upserted += stats.upserted;
                report.deleted += stats.deleted;
                report.skipped += stats.skipped;
                report.unknown += stats.unknown;
                report.items += page.items.len();
                pages += 1;
                cursor = next;
                tracing::debug!(
                    cursor,
                    head,
                    batches = page.batches,
                    items = page.items.len(),
                    "pulled page"
                );
                if pages.is_multiple_of(50) {
                    tracing::info!(cursor, head, items = report.items, "pulling history...");
                }
            }
            None => {
                // Another process (e.g. the MCP server) already advanced the
                // cursor; the page we fetched may have been applied by it.
                cursor = store.cursor()?;
                tracing::debug!(cursor, "cursor advanced by another process");
                continue;
            }
        }
    }

    head = head.max(cursor);

    report.cursor = cursor;
    report.head = head;
    report.pages = pages;
    Ok(())
}

fn check_schema(schema: i64, check: &mut CompatCheck) -> Result<()> {
    if schema != 0 {
        check.schema = Some(schema);
        if schema != SUPPORTED_SCHEMA {
            return Err(ThingsError::IncompatibleVersion(format!(
                "cloud schema {schema} (supported {SUPPORTED_SCHEMA})"
            ))
            .into());
        }
    }
    Ok(())
}

fn check_items(items: &[IncomingItem], entities: &mut BTreeMap<String, u32>) -> Result<()> {
    for item in items {
        if !(0..=2).contains(&item.action) {
            bail!(
                "unsupported action t={} for {} (expected 0|1|2)",
                item.action,
                item.uuid
            );
        }
        let (base, generation) = parse_entity_name(&item.kind);
        match supported_entity_max(base) {
            None => bail!("unknown entity kind `{}`", item.kind),
            Some(max) if generation > max => bail!(
                "entity `{}` is newer than supported (max {base}{max})",
                item.kind
            ),
            Some(_) => {
                entities
                    .entry(base.to_string())
                    .and_modify(|current| *current = (*current).max(generation))
                    .or_insert(generation);
            }
        }
    }
    Ok(())
}
