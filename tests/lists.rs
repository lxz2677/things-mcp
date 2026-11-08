use serde_json::{Value, json};
use things_mcp::fmt;
use things_mcp::store::{
    ListKind, SearchFilter, Store, area_detail, list_tasks, project_detail, search_tasks,
    tag_detail, task_counts,
};
use things_mcp::things::{ACTION_CREATE, IncomingItem};

fn item(uuid: &str, kind: &str, props: Value) -> IncomingItem {
    IncomingItem {
        uuid: uuid.to_string(),
        action: ACTION_CREATE,
        kind: kind.to_string(),
        props: props.as_object().unwrap().clone(),
        server_index: 0,
    }
}

fn task(uuid: &str, props: Value) -> IncomingItem {
    item(uuid, "Task7", props)
}

fn seed(store: &mut Store, today: i64) {
    let items = vec![
        task(
            "a-inbox",
            json!({"tt": "inbox task", "tp": 0, "ss": 0, "st": 0, "ix": -10}),
        ),
        task(
            "b-project-task",
            json!({"tt": "project task", "tp": 0, "ss": 0, "st": 0, "pr": ["proj"], "ix": -20}),
        ),
        task(
            "c-today",
            json!({"tt": "today task", "tp": 0, "ss": 0, "st": 1,
                   "sr": today * 86_400, "tir": today * 86_400, "ix": -30}),
        ),
        task(
            "d-someday",
            json!({"tt": "someday task", "tp": 0, "ss": 0, "st": 2, "ix": -40}),
        ),
        task(
            "e-upcoming",
            json!({"tt": "upcoming task", "tp": 0, "ss": 0, "st": 2, "sr": (today + 1) * 86_400}),
        ),
        task(
            "f-completed",
            json!({"tt": "done task", "tp": 0, "ss": 3, "sp": 1_700_000_000.5}),
        ),
        task(
            "g-deadline",
            json!({"tt": "deadline task", "tp": 0, "ss": 0, "st": 2,
                   "dd": (today + 3) * 86_400, "sr": (today + 3) * 86_400}),
        ),
        // Recurrence scheduling noise: no title, no type.
        task(
            "h-noise",
            json!({"icc": 0, "icsd": today * 86_400, "ti": -1}),
        ),
        // A heading must not show up in Today even when it carries st/tir.
        task(
            "i-heading",
            json!({"tt": "heading", "tp": 2, "ss": 0, "st": 1, "tir": today * 86_400}),
        ),
        item("proj", "Task7", json!({"tt": "Proj", "tp": 1, "st": 1})),
        item("area-1", "Area3", json!({"tt": "Area", "ix": 0, "tg": []})),
        item(
            "tag-1",
            "Tag4",
            json!({"tt": "Tag", "ix": 0, "pn": [], "sh": null}),
        ),
    ];

    let mut items = items;
    items.push(task(
        "j-tagged",
        json!({"tt": "tagged task", "tp": 0, "ss": 0, "st": 0, "pr": ["proj"],
               "tg": ["tag-1"], "nt": {"_t":"tx","ch":0,"v":"milk and eggs","t":1}}),
    ));

    store.apply_items(&items).unwrap();
}

fn titles(store: &Store, kind: &ListKind, today: i64) -> Vec<String> {
    list_tasks(store.connection(), kind, today, 100, 0)
        .unwrap()
        .into_iter()
        .map(|task| task.title)
        .collect()
}

#[test]
fn list_semantics_match_the_documented_rules() {
    let mut store = Store::open_in_memory().unwrap();
    let today = fmt::today_epoch_day();
    seed(&mut store, today);

    let inbox = titles(&store, &ListKind::Inbox, today);
    assert_eq!(inbox, vec!["inbox task"]);

    let anytime = titles(&store, &ListKind::Anytime, today);
    assert_eq!(anytime, vec!["project task", "tagged task"]);

    let today_tasks = titles(&store, &ListKind::Today, today);
    assert_eq!(today_tasks, vec!["today task"]);

    let someday = titles(&store, &ListKind::Someday, today);
    assert_eq!(someday, vec!["someday task"]);

    let upcoming = titles(&store, &ListKind::Upcoming, today);
    assert_eq!(upcoming, vec!["upcoming task", "deadline task"]);

    let logbook = titles(&store, &ListKind::Logbook, today);
    assert_eq!(logbook, vec!["done task"]);

    let deadlines = titles(&store, &ListKind::Deadlines, today);
    assert_eq!(deadlines, vec!["deadline task"]);

    // Noise entities (no title) and headings are never listed as tasks.
    for list in [&ListKind::All, &ListKind::Inbox, &ListKind::Today] {
        assert!(
            !titles(&store, list, today).contains(&"heading".to_string()),
            "heading leaked into {}",
            list.as_str()
        );
        assert!(
            !titles(&store, list, today).contains(&String::new()),
            "untitled noise leaked into {}",
            list.as_str()
        );
    }
}

#[test]
fn counts_are_consistent() {
    let mut store = Store::open_in_memory().unwrap();
    let today = fmt::today_epoch_day();
    seed(&mut store, today);

    let counts: std::collections::HashMap<_, _> = task_counts(store.connection(), today)
        .unwrap()
        .into_iter()
        .collect();

    assert_eq!(counts["inbox"], 1);
    assert_eq!(counts["today"], 1);
    assert_eq!(counts["upcoming"], 2);
    assert_eq!(counts["anytime"], 2);
    assert_eq!(counts["someday"], 1);
    assert_eq!(counts["logbook"], 1);
    assert_eq!(counts["deadlines"], 1);
    assert_eq!(counts["projects"], 1);
    assert_eq!(counts["areas"], 1);
    assert_eq!(counts["tags"], 1);
}

#[test]
fn search_matches_title_and_notes_and_filters() {
    let mut store = Store::open_in_memory().unwrap();
    let today = fmt::today_epoch_day();
    seed(&mut store, today);

    let by_title = search_tasks(
        store.connection(),
        &SearchFilter {
            query: Some("tagged".into()),
            ..Default::default()
        },
        50,
        0,
    )
    .unwrap();
    assert_eq!(by_title.len(), 1);

    let by_note = search_tasks(
        store.connection(),
        &SearchFilter {
            query: Some("EGGS".into()),
            ..Default::default()
        },
        50,
        0,
    )
    .unwrap();
    assert_eq!(by_note.len(), 1);
    assert_eq!(by_note[0].notes, "milk and eggs");

    let by_tag = search_tasks(
        store.connection(),
        &SearchFilter {
            tag_ids: vec!["tag-1".into()],
            ..Default::default()
        },
        50,
        0,
    )
    .unwrap();
    assert_eq!(by_tag.len(), 1);

    // Completed tasks are excluded unless explicitly requested.
    let completed = search_tasks(
        store.connection(),
        &SearchFilter {
            query: Some("done".into()),
            ..Default::default()
        },
        50,
        0,
    )
    .unwrap();
    assert!(completed.is_empty());

    let completed = search_tasks(
        store.connection(),
        &SearchFilter {
            query: Some("done".into()),
            include_completed: true,
            ..Default::default()
        },
        50,
        0,
    )
    .unwrap();
    assert_eq!(completed.len(), 1);
}

#[test]
fn updates_merge_field_by_field_and_notes_patches_apply_once() {
    let mut store = Store::open_in_memory().unwrap();

    store
        .apply_items(&[task(
            "t1",
            json!({"tt": "first", "st": 0, "ss": 0, "nt": {"_t":"tx","ch":0,"v":"第一行正文\n第二行内容保留","t":1}}),
        )])
        .unwrap();

    store
        .apply_items(&[task(
            "t1",
            json!({"tt": "second", "nt": {"t":2,"ps":[{"p":0,"l":16,"r":"备注被修改过\n"}]}}),
        )])
        .unwrap();

    let task = store.get_task("t1").unwrap().unwrap();
    assert_eq!(task.title, "second");
    assert_eq!(task.notes, "备注被修改过\n第二行内容保留");
    // Fields absent from the update are preserved.
    assert_eq!(task.start, 0);
    assert_eq!(task.status, 0);
}

#[test]
fn apply_page_does_not_apply_when_another_writer_moved_the_cursor() {
    let mut store = Store::open_in_memory().unwrap();
    store.set_cursor(3).unwrap();

    let applied = store
        .apply_page(
            0,
            &[task(
                "stale",
                json!({"tt": "stale", "tp": 0, "ss": 0, "st": 0}),
            )],
            1,
        )
        .unwrap();

    assert!(applied.is_none(), "stale page must not be applied");
    assert_eq!(store.cursor().unwrap(), 3);
    assert_eq!(store.task_count().unwrap(), 0);
}

#[test]
fn project_area_and_tag_details_resolve_children() {
    let mut store = Store::open_in_memory().unwrap();
    let today = fmt::today_epoch_day();
    seed(&mut store, today);

    let project = project_detail(store.connection(), "proj", today, 100)
        .unwrap()
        .expect("project proj");
    assert_eq!(project.summary.title, "Proj");
    assert_eq!(project.summary.open_tasks, 2, "project task + tagged task");
    assert!(
        project
            .tasks
            .iter()
            .any(|task| task.title == "project task")
    );

    let area = area_detail(store.connection(), "area-1", today, 100)
        .unwrap()
        .expect("area area-1");
    assert_eq!(area.title, "Area");

    let tag = tag_detail(store.connection(), "tag-1", today, 100)
        .unwrap()
        .expect("tag tag-1");
    assert_eq!(tag.title, "Tag");
    assert_eq!(tag.tasks.len(), 1);
    assert_eq!(tag.tasks[0].title, "tagged task");

    assert!(
        project_detail(store.connection(), "missing", today, 100)
            .unwrap()
            .is_none()
    );
}

#[test]
fn two_connections_never_apply_the_same_page_twice() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("shared.sqlite");

    let mut first = Store::open(&path).unwrap();
    let mut second = Store::open(&path).unwrap();

    let page = [task(
        "shared-1",
        json!({"tt": "shared", "tp": 0, "ss": 0, "st": 0}),
    )];

    let applied = first.apply_page(0, &page, 1).unwrap();
    assert!(applied.is_some(), "first writer wins");

    let applied = second.apply_page(0, &page, 1).unwrap();
    assert!(applied.is_none(), "second writer must back off");

    assert_eq!(second.cursor().unwrap(), 1);
    assert_eq!(second.task_count().unwrap(), 1);
}
