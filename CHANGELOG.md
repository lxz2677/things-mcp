# Changelog

Add each release under a `## vX.Y.Z` heading. The release workflow uses that section as the GitHub Release description.

## v0.3.1

- Fix a full-sync failure on older replicas where the column order could trigger `NOT NULL constraint failed: task.instance_creation_paused`.
- Clear the local cache and sync cursor on first use after upgrading. The next sync rebuilds the cache from Things Cloud. Later starts of the same version reuse the rebuilt cache.

## v0.3.0

- Create, change, and clear repeat rules on tasks and projects through the CLI and MCP. Rules support daily, weekly, monthly, yearly, and after-completion schedules, with intervals, selected weekdays, and optional end dates. Updates that set a repeat rule require a start date. The end date cannot precede the first scheduled occurrence or the start date of an after-completion rule.
- Rename, reorder, complete, reopen, and delete checklist items with the CLI's `update-checklist`, `toggle-checklist`, and `delete-checklist` commands or the matching MCP tools. Reordering a checklist item or heading preserves its modification time.
- Manage projects and headings with dedicated CLI commands and MCP tools. Projects support updates, status changes, trash and restore, and deletion. Headings support creation, renaming, reordering, and deletion. Permanent deletion requires an empty project or heading.
- **Migration for existing callers.** Use project commands and tools for project IDs. For project reminders, use `update-project --remind` or `--clear-remind`, or pass `reminder` or `clear_reminder: true` to `update_project`. Checklist items can be added to to-dos.
- Detect incompatible Things Cloud schemas, entity versions, and actions during sync. On the first sync, an older unvalidated replica replays its history before being marked compatible. An in-flight sync cannot clear a newly recorded incompatibility.
- CLI and MCP data operations stop when an incompatibility is detected. MCP returns a structured `INCOMPATIBLE_VERSION` tool result with upgrade guidance, including when the first write detects the issue. Check the stored status with `verify`, `doctor`, or `sync_status` and run `sync` or `sync_now` to recheck.

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
