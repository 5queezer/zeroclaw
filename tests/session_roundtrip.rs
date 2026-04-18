//! End-to-end: SessionStore create → append → reopen → load reproduces state.

use hrafn::session::{ChatMessage, SessionStore, ToolStatus};
use std::path::PathBuf;
use std::time::Duration;

#[test]
fn roundtrip_persists_across_reopen() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("s.db");

    let id = {
        let store = SessionStore::open(&db).unwrap();
        let m = store
            .create(&PathBuf::from("/tmp"), Some("title"), Some("p"), Some("m"))
            .unwrap();

        store
            .append(&m.id, &ChatMessage::User { text: "hi".into() })
            .unwrap();
        store
            .append(&m.id, &ChatMessage::Assistant { text: "hey".into() })
            .unwrap();
        store
            .append(
                &m.id,
                &ChatMessage::ToolCall {
                    name: "t".into(),
                    args: "{}".into(),
                    status: ToolStatus::Done(Duration::from_millis(1)),
                },
            )
            .unwrap();
        store
            .append(
                &m.id,
                &ChatMessage::ToolResult {
                    name: "t".into(),
                    output: "o".into(),
                },
            )
            .unwrap();
        m.id
    }; // Drop the store to ensure WAL is checkpointed / locks released.

    let store = SessionStore::open(&db).unwrap();
    let loaded = store.load(&id).unwrap();

    assert_eq!(loaded.messages.len(), 4);
    assert_eq!(loaded.meta.counts.user, 1);
    assert_eq!(loaded.meta.counts.assistant, 1);
    assert_eq!(loaded.meta.counts.tool_call, 1);
    assert_eq!(loaded.meta.counts.tool_result, 1);
    assert_eq!(loaded.meta.counts.total, 4);
    assert_eq!(loaded.meta.title.as_deref(), Some("title"));
    assert_eq!(loaded.meta.provider.as_deref(), Some("p"));
    assert_eq!(loaded.meta.model.as_deref(), Some("m"));

    // Order and seq numbers preserved.
    assert_eq!(loaded.messages[0].seq, 1);
    assert_eq!(loaded.messages[1].seq, 2);
    assert_eq!(loaded.messages[2].seq, 3);
    assert_eq!(loaded.messages[3].seq, 4);
    assert!(matches!(&loaded.messages[0].body, ChatMessage::User { text } if text == "hi"));
    assert!(matches!(&loaded.messages[1].body, ChatMessage::Assistant { text } if text == "hey"));
    assert!(matches!(&loaded.messages[2].body, ChatMessage::ToolCall { name, .. } if name == "t"));
    assert!(
        matches!(&loaded.messages[3].body, ChatMessage::ToolResult { name, output } if name == "t" && output == "o")
    );
}

#[test]
fn fuzzy_continue_picks_newest() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("s.db");
    let store = SessionStore::open(&db).unwrap();
    let _a = store
        .create(&PathBuf::from("/tmp"), Some("Fix auth bug"), None, None)
        .unwrap();
    std::thread::sleep(Duration::from_millis(5));
    let b = store
        .create(&PathBuf::from("/tmp"), Some("Add auth tests"), None, None)
        .unwrap();
    let hit = store.find_by_title_fuzzy("auth").unwrap().unwrap();
    assert_eq!(hit.id, b.id);
    assert!(store.find_by_title_fuzzy("nope").unwrap().is_none());
}

#[test]
fn most_recent_follows_updated_at() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("s.db");
    let store = SessionStore::open(&db).unwrap();
    let a = store
        .create(&PathBuf::from("/tmp"), Some("A"), None, None)
        .unwrap();
    std::thread::sleep(Duration::from_millis(5));
    let b = store
        .create(&PathBuf::from("/tmp"), Some("B"), None, None)
        .unwrap();
    assert_eq!(store.most_recent().unwrap().unwrap().id, b.id);

    // append to `a` — now it's more recent.
    std::thread::sleep(Duration::from_millis(5));
    store
        .append(
            &a.id,
            &ChatMessage::User {
                text: "bump".into(),
            },
        )
        .unwrap();
    assert_eq!(store.most_recent().unwrap().unwrap().id, a.id);
}
