# Changelog

Add each release under a `## vX.Y.Z` heading. The release workflow uses that section as the GitHub Release description.

## Unreleased

- Detect incompatible Things Cloud versions at sync time: `schema` must be 301, entity generations are capped at Task7/Area3/Tag4/ChecklistItem3/Settings5/Tombstone2, and unknown entity kinds or item actions fail the sync.
- The compatibility state is persisted. Every CLI/MCP data operation then returns an `INCOMPATIBLE_VERSION` warning and stops without touching the replica; `sync`, `verify`, `doctor`, and `account` remain available to re-check after upgrading.
- `verify`, `doctor`, and `sync_status` show the compatibility state and the last check time; `doctor` adds a `compat.status` audit check.
- Incompatibility is returned to MCP clients as a tool result (`isError: true`) with a structured `INCOMPATIBLE_VERSION` payload and remediation text, so agents can read the reason and stop instead of relying on a JSON-RPC error.

## Unreleased

- Create and update repeating tasks: daily/weekly/monthly/yearly, every N periods, weekday sets, an optional end date, and after-completion repeats. `create_task` / `update_task` take `repeat`; `update_task` takes `clear_repeat`; the CLI uses `--repeat`, `--repeat-interval`, `--repeat-end`, and `--clear-repeat`. Rules are validated before pushing and written to both `rr` and `rp`.

## Unreleased

- Manage checklist items end to end: rename/reorder, complete/reopen (toggle), and delete. New MCP tools `update_checklist_item`, `toggle_checklist_item`, `delete_checklist_item`; new CLI commands `update-checklist`, `toggle-checklist`, `delete-checklist`.

## v0.2.1

- Set a local reminder time (`HH:MM`) when creating or updating a task or project with a start date. Use `--remind` in the CLI or `reminder` in MCP. For example, create a task with `--when tomorrow --remind 09:00`.
- Clear a reminder with `--clear-remind` or `clear_reminder: true`. Removing the start date also clears its reminder.
- Task lists and details show reminder times. Existing local databases pick up previously synced reminders on first open without a full sync, and `doctor` checks that the stored times match the synced data.

## v0.2.0

- Rename and delete tags and areas from the CLI or MCP. You can also change a tag's parent or shortcut and replace an area's tags.
- Area details include the current tag IDs so callers can preserve them when making changes.
- Prevent tag hierarchy cycles, reject unknown tag IDs in area updates, and block deletion of areas with task or project references and tags with children.
- Choose where to install things-mcp. Use `--update` to install the latest stable release at the existing path.
- Use `serve --log-file PATH` to record MCP startup, tool calls, and shutdown.
