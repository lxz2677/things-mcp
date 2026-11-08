//! Headless Things Cloud client, exposed as an MCP server.
//!
//! This crate is a Rust port of the logic in `pdurlej/things-cloud-sdk`:
//! the wire protocol client, the entity model, the list/query semantics, and
//! (later) the MCP tool surface.
//!
//! Persistence is SQLite (WAL) so that a long-running MCP server and ad-hoc
//! CLI commands can share the same state safely.

pub mod config;
pub mod fmt;
pub mod ids;
pub mod mcp;
pub mod notes;
pub mod store;
pub mod sync;
pub mod testing;
pub mod things;
