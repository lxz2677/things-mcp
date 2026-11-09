# Changelog

Add each release under a `## vX.Y.Z` heading. The release workflow uses that section as the GitHub Release description.

## Unreleased

- Set or clear task reminders. `create_task` / `create_project` accept `reminder: "HH:MM"`; `update_task` accepts `reminder` or `clear_reminder`. The CLI uses `--remind` and `--clear-remind`.
- The wire field is `ato` (seconds since local midnight). Existing replicas are migrated and backfilled from the raw history on first open.
- List and task detail output include the reminder time; `doctor` validates the decode against the raw `ato` value.

## v0.2.0

- Rename and delete tags and areas from the CLI or MCP. You can also change a tag's parent or shortcut and replace an area's tags.
- Area details include the current tag IDs so callers can preserve them when making changes.
- Prevent tag hierarchy cycles, reject unknown tag IDs in area updates, and block deletion of areas with task or project references and tags with children.
- Choose where to install things-mcp. Use `--update` to install the latest stable release at the existing path.
- Use `serve --log-file PATH` to record MCP startup, tool calls, and shutdown.
