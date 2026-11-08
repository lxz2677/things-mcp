//! End-to-end check of the stdio MCP server: spawn the real binary, speak
//! JSON-RPC over its stdin/stdout, and assert on `tools/list` + `tools/call`.

use std::io::{BufRead, BufReader, Write};
use std::process::{Command, Stdio};

use serde_json::{Value, json};
use things_mcp::store::Store;
use things_mcp::things::{ACTION_CREATE, IncomingItem};

fn seed(path: &std::path::Path) {
    let mut store = Store::open(path).unwrap();
    let items = [("inbox-1", "buy milk"), ("today-1", "ship the release")]
        .into_iter()
        .map(|(uuid, title)| IncomingItem {
            uuid: uuid.to_string(),
            action: ACTION_CREATE,
            kind: "Task7".to_string(),
            props: json!({"tt": title, "tp": 0, "ss": 0, "st": 0})
                .as_object()
                .unwrap()
                .clone(),
            server_index: 0,
        })
        .collect::<Vec<_>>();
    store.apply_items(&items).unwrap();
}

#[test]
fn stdio_server_lists_and_calls_tools() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("things.sqlite");
    seed(&db);

    let mut child = Command::new(env!("CARGO_BIN_EXE_things-mcp"))
        .arg("serve")
        .env("THINGS_DB", &db)
        .env("THINGS_REFRESH_SECS", "0")
        .env("RUST_LOG", "error")
        .env_remove("THINGS_EMAIL")
        .env_remove("THINGS_PASSWORD")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn things-mcp serve");

    {
        let stdin = child.stdin.as_mut().unwrap();
        for line in [
            json!({
                "jsonrpc": "2.0",
                "id": 1,
                "method": "initialize",
                "params": {
                    "protocolVersion": "2025-06-18",
                    "capabilities": {},
                    "clientInfo": {"name": "stdio-test", "version": "0.1"}
                }
            }),
            json!({"jsonrpc": "2.0", "method": "notifications/initialized"}),
            json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list", "params": {}}),
            json!({
                "jsonrpc": "2.0",
                "id": 3,
                "method": "tools/call",
                "params": {"name": "list_tasks", "arguments": {"list": "all"}}
            }),
            json!({
                "jsonrpc": "2.0",
                "id": 4,
                "method": "tools/call",
                "params": {
                    "name": "create_task",
                    "arguments": {"title": "preview", "dry_run": true}
                }
            }),
        ] {
            writeln!(stdin, "{line}").unwrap();
        }
        stdin.flush().unwrap();
    }

    let stdout = child.stdout.take().unwrap();
    let mut reader = BufReader::new(stdout);
    let mut responses: Vec<Value> = Vec::new();
    while responses.len() < 4 {
        let mut line = String::new();
        if reader.read_line(&mut line).unwrap() == 0 {
            break;
        }
        if line.trim().is_empty() {
            continue;
        }
        let value: Value = serde_json::from_str(&line).expect("valid JSON-RPC");
        if value.get("id").is_some() {
            responses.push(value);
        }
    }

    drop(child.stdin.take());
    let _ = child.wait();

    let initialize = responses
        .iter()
        .find(|value| value["id"] == 1)
        .expect("initialize response");
    assert_eq!(
        initialize["result"]["serverInfo"]["name"], "things-mcp",
        "server info"
    );

    let list = responses
        .iter()
        .find(|value| value["id"] == 2)
        .expect("tools/list response");
    let tools = list["result"]["tools"].as_array().expect("tools array");
    assert_eq!(tools.len(), 29, "tool count");
    let names: Vec<&str> = tools
        .iter()
        .filter_map(|tool| tool["name"].as_str())
        .collect();
    for expected in [
        "list_tasks",
        "get_task",
        "search",
        "list_projects",
        "list_areas",
        "list_tags",
        "list_logbook",
        "list_deadlines",
        "list_upcoming",
        "list_by_project",
        "list_by_area",
        "list_by_tag",
        "get_project",
        "get_area",
        "get_tag",
        "sync_now",
        "sync_status",
        "create_task",
        "create_project",
        "create_area",
        "create_tag",
        "update_task",
        "complete_task",
        "cancel_task",
        "reopen_task",
        "trash_task",
        "restore_task",
        "delete_task",
        "add_checklist_item",
    ] {
        assert!(names.contains(&expected), "missing tool {expected}");
    }

    let call = responses
        .iter()
        .find(|value| value["id"] == 3)
        .expect("tools/call response");
    let payload = &call["result"]["structuredContent"];
    assert_eq!(payload["count"], 2, "all list count: {payload}");
    assert_eq!(call["result"]["isError"], false);

    let dry_run = responses
        .iter()
        .find(|value| value["id"] == 4)
        .expect("create_task dry-run response");
    let payload = &dry_run["result"]["structuredContent"];
    assert_eq!(payload["status"], "dry-run");
    assert_eq!(payload["pushed"], false);
    assert_eq!(
        payload["payload"].as_object().unwrap().len(),
        34,
        "dry-run must expose the 34-key snapshot"
    );
    assert_eq!(payload["payload"]["tt"], "preview");
}
