//! Synchronisation engine: pull history pages and apply them to SQLite.
//!
//! Cursor semantics (verified against the reference implementation):
//!
//! * `current-item-index` is the server **head**, not the end of the page;
//! * the cursor advances by the number of top-level batches (`items.len()`),
//!   because one batch can expand into several entity items;
//! * never request `start-index` beyond the head: the backend answers HTTP 500;
//! * an empty page below the head means the pull stalled and must be retried.

use std::time::Duration;

use anyhow::{Result, bail};

use crate::fmt;
use crate::store::Store;
use crate::things::{HistoryMeta, ThingsClient, ThingsError};

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
}

impl SyncEngine {
    pub fn new(client: ThingsClient, store: Store) -> Self {
        Self { client, store }
    }

    pub fn store(&self) -> &Store {
        &self.store
    }

    pub fn store_mut(&mut self) -> &mut Store {
        &mut self.store
    }

    pub async fn sync(&mut self, full: bool) -> Result<SyncReport> {
        if full {
            self.store.reset()?;
        }

        let (history_key, meta, history_replaced) = self.resolve_history().await?;
        let mut head = meta.latest_server_index;
        let mut cursor = self.store.cursor()?;

        if self.store.schema_version()? == 0 && meta.latest_schema_version != 0 {
            self.store.set_schema_version(meta.latest_schema_version)?;
        }

        let mut report = SyncReport {
            history_key: history_key.clone(),
            head,
            full_reset: full,
            history_replaced,
            ..Default::default()
        };

        // A cursor beyond the head means the cloud history was replaced.
        if cursor > head {
            tracing::warn!(
                cursor,
                head,
                "local cursor is ahead of the server head; resetting the local replica"
            );
            self.store.reset()?;
            cursor = 0;
            report.history_replaced = true;
        }

        tracing::info!(history_key = %history_key, cursor, head, "sync started");

        let mut pages = 0usize;
        let mut empty_pages = 0usize;

        while cursor < head {
            if pages >= MAX_PAGES {
                bail!("pull exceeded {MAX_PAGES} pages; aborting to avoid a loop");
            }

            let page = self
                .client
                .items_page(&history_key, cursor as usize)
                .await?;

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
                let refreshed = self.client.history_meta(&history_key).await?;
                head = head.max(refreshed.latest_server_index);
                tokio::time::sleep(Duration::from_millis(250)).await;
                continue;
            }
            empty_pages = 0;

            let next = cursor + page.batches as i64;
            match self.store.apply_page(cursor, &page.items, next)? {
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
                    if pages % 50 == 0 {
                        tracing::info!(cursor, head, items = report.items, "pulling history...");
                    }
                }
                None => {
                    // Another process (e.g. the MCP server) already advanced the
                    // cursor; the page we fetched may have been applied by it.
                    cursor = self.store.cursor()?;
                    tracing::debug!(cursor, "cursor advanced by another process");
                    continue;
                }
            }

            head = head.max(page.head);
        }

        self.store.set_latest_server_index(head)?;
        self.store.set_last_sync_at(&fmt::now_rfc3339())?;
        tracing::info!(pages, items = report.items, cursor, head, "sync finished");

        report.cursor = cursor;
        report.head = head;
        report.pages = pages;
        Ok(report)
    }

    async fn resolve_history(&mut self) -> Result<(String, HistoryMeta, bool)> {
        let stored = self.store.history_key()?;
        let history_key = match stored.filter(|key| !key.is_empty()) {
            Some(key) => key,
            None => {
                let own = self.client.own_history_key().await?;
                self.store.set_history_key(&own.history_key)?;
                own.history_key
            }
        };

        match self.client.history_meta(&history_key).await {
            Ok(meta) => Ok((history_key, meta, false)),
            Err(ThingsError::NotFound(found)) => {
                tracing::warn!(path = %found, "history key not found; checking for a replaced history");
                let own = self.client.own_history_key().await?;
                if own.history_key == history_key {
                    bail!("history {history_key} is not available on the server");
                }
                self.store.reset()?;
                self.store.set_history_key(&own.history_key)?;
                let meta = self.client.history_meta(&own.history_key).await?;
                Ok((own.history_key, meta, true))
            }
            Err(error) => Err(error.into()),
        }
    }
}
