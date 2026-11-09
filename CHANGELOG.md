# Changelog

Add each release under a `## vX.Y.Z` heading. The release workflow uses that section as the GitHub Release description.

## v0.2.0

- Rename and delete tags and areas from the CLI or MCP. You can also change a tag's parent or shortcut and replace an area's tags.
- Area details include the current tag IDs so callers can preserve them when making changes.
- Prevent tag hierarchy cycles, reject unknown tag IDs in area updates, and block deletion of areas with task or project references and tags with children.
- Choose where to install things-mcp. Use `--update` to install the latest stable release at the existing path.
