# Changelog

Add each release under a `## vX.Y.Z` heading. The release workflow uses that section as the GitHub Release description.

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
