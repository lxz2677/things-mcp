//! End-to-end check of the stdio MCP server: spawn the real binary, speak
//! JSON-RPC over its stdin/stdout, and assert on `tools/list` + `tools/call`.

use std::io::{BufRead, BufReader, Write};
use std::process::{Command, Stdio};

use serde_json::{Value, json};
use things_mcp::store::Store;
use things_mcp::things::{ACTION_CREATE, IncomingItem};

fn seed(path: &std::path::Path) {
    let mut store = Store::open(path).unwrap();
    let items = [
        (
            "inbox-1",
            "Task7",
            json!({"tt":"buy milk","tp":0,"ss":0,"st":0}),
        ),
        (
            "today-1",
            "Task7",
            json!({"tt":"ship the release","tp":0,"ss":0,"st":0}),
        ),
        ("area-1", "Area3", json!({"tt":"Work","tg":["tag-1"]})),
        ("tag-1", "Tag4", json!({"tt":"Urgent"})),
    ]
    .into_iter()
    .map(|(uuid, kind, props)| IncomingItem {
        uuid: uuid.to_string(),
        action: ACTION_CREATE,
        kind: kind.to_string(),
        props: props.as_object().unwrap().clone(),
        server_index: 0,
    })
    .collect::<Vec<_>>();
    store.apply_items(&items).unwrap();
}

#[test]
fn stdio_server_lists_and_calls_tools() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("things.sqlite");
    let log_file = dir.path().join("mcp.log");
    seed(&db);

    let mut child = Command::new(env!("CARGO_BIN_EXE_things-mcp"))
        .arg("serve")
        .arg("--log-file")
        .arg(&log_file)
        .env("THINGS_DB", &db)
        .env("THINGS_BASE_URL", "http://127.0.0.1:9")
        .env("THINGS_REFRESH_SECS", "0")
        .env("RUST_LOG", "error")
        .env_remove("THINGS_EMAIL")
        .env_remove("THINGS_PASSWORD")
        .env_remove("THINGS_USERNAME")
        .env_remove("THINGS_TOKEN")
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
                    "arguments": {
                        "title": "preview",
                        "when": "tomorrow",
                        "reminder": "09:00",
                        "repeat": {"frequency": "daily"},
                        "dry_run": true
                    }
                }
            }),
            json!({
                "jsonrpc": "2.0",
                "id": 5,
                "method": "tools/call",
                "params": {"name": "get_area", "arguments": {"id": "area-1"}}
            }),
        ] {
            writeln!(stdin, "{line}").unwrap();
        }
        stdin.flush().unwrap();
    }

    let stdout = child.stdout.take().unwrap();
    let mut reader = BufReader::new(stdout);
    let mut responses: Vec<Value> = Vec::new();
    while responses.len() < 5 {
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
    assert!(child.wait().unwrap().success());

    let log = std::fs::read_to_string(log_file).unwrap();
    assert!(log.contains("MCP stdio ready"), "{log}");
    assert!(
        log.contains("MCP tool started") && log.contains("tool=list_tasks"),
        "{log}"
    );
    assert!(log.contains("MCP tool finished"), "{log}");
    assert!(
        log.contains("MCP stdio stopped") && log.contains("reason=Closed"),
        "{log}"
    );
    assert!(!log.contains("buy milk"), "{log}");

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
    assert_eq!(tools.len(), 46, "tool count");
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
        "update_tag",
        "delete_tag",
        "update_area",
        "delete_area",
        "update_task",
        "complete_task",
        "cancel_task",
        "reopen_task",
        "trash_task",
        "restore_task",
        "delete_task",
        "add_checklist_item",
        "update_checklist_item",
        "toggle_checklist_item",
        "delete_checklist_item",
        "create_heading",
        "update_heading",
        "delete_heading",
        "update_project",
        "complete_project",
        "cancel_project",
        "reopen_project",
        "trash_project",
        "restore_project",
        "delete_project",
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
    assert_eq!(payload["payload"]["ato"], 32_400);
    assert_eq!(payload["payload"].get("rp"), Some(&Value::Null));
    assert_eq!(payload["payload"]["rr"]["fu"], 16);
    assert_eq!(payload["payload"]["st"], 2, "a dated repeat is scheduled");
    assert!(payload["payload"]["sr"].is_i64());

    let area = responses
        .iter()
        .find(|value| value["id"] == 5)
        .expect("get_area response");
    let payload = &area["result"]["structuredContent"];
    assert_eq!(payload["title"], "Work");
    assert_eq!(payload["tags"], json!(["tag-1"]));
}

fn call_tool(db: &std::path::Path, name: &str, arguments: Value) -> Value {
    let mut child = Command::new(env!("CARGO_BIN_EXE_things-mcp"))
        .arg("serve")
        .env("THINGS_DB", db)
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
            json!({
                "jsonrpc": "2.0",
                "id": 2,
                "method": "tools/call",
                "params": {"name": name, "arguments": arguments}
            }),
        ] {
            writeln!(stdin, "{line}").unwrap();
        }
        stdin.flush().unwrap();
    }

    let stdout = child.stdout.take().unwrap();
    let mut reader = BufReader::new(stdout);
    let mut response = None;
    while response.is_none() {
        let mut line = String::new();
        if reader.read_line(&mut line).unwrap() == 0 {
            break;
        }
        let value: Value = serde_json::from_str(line.trim()).expect("valid JSON-RPC");
        if value["id"] == 2 {
            response = Some(value);
        }
    }

    drop(child.stdin.take());
    let _ = child.wait();

    response.expect("tools/call response")["result"].clone()
}

#[test]
fn incompatible_replica_returns_a_readable_tool_error() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("things.sqlite");
    seed(&db);

    {
        let store = Store::open(&db).unwrap();
        store
            .set_compat_incompatible(
                "cloud schema 302 (supported 301)",
                Some(302),
                &Default::default(),
            )
            .unwrap();
    }

    let result = call_tool(&db, "list_tasks", json!({"list": "inbox"}));

    assert_eq!(
        result["isError"], true,
        "must be a tool result, not a JSON-RPC error"
    );
    let payload = &result["structuredContent"];
    assert_eq!(payload["error"], "INCOMPATIBLE_VERSION");
    assert_eq!(payload["details"]["do_not_retry"], true);
    assert!(
        result["content"][0]["text"]
            .as_str()
            .unwrap_or_default()
            .contains("upgrade"),
        "the text content must tell the agent what to do"
    );
}

#[test]
fn update_project_can_set_and_clear_a_reminder() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("things.sqlite");
    Store::open(&db)
        .unwrap()
        .apply_items(&[IncomingItem {
            uuid: "project-1".into(),
            action: ACTION_CREATE,
            kind: "Task7".into(),
            props: json!({"tt":"Project","tp":1,"st":2,"sr":1794700800,"ato":32400})
                .as_object()
                .unwrap()
                .clone(),
            server_index: 0,
        }])
        .unwrap();

    let set = call_tool(
        &db,
        "update_project",
        json!({"id":"project-1","reminder":"15:45","dry_run":true}),
    );
    assert_eq!(set["isError"], false);
    assert_eq!(set["structuredContent"]["payload"]["ato"], 56_700);

    let clear = call_tool(
        &db,
        "update_project",
        json!({"id":"project-1","clear_reminder":true,"dry_run":true}),
    );
    assert_eq!(clear["isError"], false);
    assert_eq!(
        clear["structuredContent"]["payload"].get("ato"),
        Some(&Value::Null)
    );
}
