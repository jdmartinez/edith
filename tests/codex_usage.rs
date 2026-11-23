#[path = "../src/usage.rs"]
mod usage;

use std::{fs, io::Write};
use usage::UsageStore;

#[test]
fn imports_only_metrics_and_retains_them_across_retries_and_source_removal() -> anyhow::Result<()> {
    let fixture = tempfile::tempdir()?;
    let home = fixture.path().join("codex");
    let sessions = home.join("sessions");
    let archived = home.join("archived_sessions");
    fs::create_dir_all(&sessions)?;
    fs::create_dir_all(&archived)?;
    let rollout = sessions.join("rollout-test.jsonl");
    let first = r#"{"type":"session_meta","payload":{"id":"fixture-session"}}
{"type":"turn_context","payload":{"model":"fixture-model"}}
{"type":"event_msg","payload":{"type":"user_message","message":"SYNTHETIC_PROMPT_MARKER"}}
{"timestamp":"2025-06-01T12:00:00Z","type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":100,"cached_input_tokens":20,"output_tokens":10}}}}
"#;
    let next = r#"{"timestamp":"2025-06-01T13:00:00Z","type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":160,"cached_input_tokens":30,"output_tokens":14}}}}"#;
    fs::write(&rollout, format!("{first}{next}"))?;
    let database = fixture.path().join("usage.sqlite");
    let store = UsageStore::new(database.clone());
    store.scan(&home)?;
    let saved = store.saved()?;
    assert_eq!(saved.len(), 1);
    assert_eq!(
        (saved[0].input, saved[0].cached, saved[0].output),
        (100, 20, 10)
    );
    assert_eq!(saved[0].model, "fixture-model");

    writeln!(fs::OpenOptions::new().append(true).open(&rollout)?)?;
    store.scan(&home)?;
    fs::copy(&rollout, archived.join("rollout-test.jsonl"))?;
    store.scan(&home)?;
    fs::remove_dir_all(&home)?;
    let reopened = UsageStore::new(database.clone());
    reopened.scan(&home)?;
    let saved = reopened.saved()?;
    assert_eq!(saved.len(), 2);
    assert_eq!(
        (saved[1].input, saved[1].cached, saved[1].output),
        (60, 10, 4)
    );
    assert!(
        !fs::read(database)?
            .windows(23)
            .any(|bytes| bytes == b"SYNTHETIC_PROMPT_MARKER")
    );
    Ok(())
}

#[test]
fn cumulative_usage_does_not_count_an_earlier_last_only_event_again() -> anyhow::Result<()> {
    let fixture = tempfile::tempdir()?;
    let sessions = fixture.path().join("sessions");
    fs::create_dir(&sessions)?;
    fs::write(
        sessions.join("rollout.jsonl"),
        r#"{"type":"session_meta","payload":{"id":"fixture-session"}}
{"timestamp":"2025-06-01T12:00:00Z","type":"event_msg","payload":{"type":"token_count","info":{"last_token_usage":{"input_tokens":100,"output_tokens":10}}}}
{"timestamp":"2025-06-01T13:00:00Z","type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":130,"output_tokens":12},"last_token_usage":{"input_tokens":30,"output_tokens":2}}}}
{"timestamp":"2025-06-01T14:00:00Z","type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":130,"output_tokens":12},"last_token_usage":{"input_tokens":30,"output_tokens":2}}}}
"#,
    )?;
    let store = UsageStore::new(fixture.path().join("edith.sqlite"));
    store.scan(fixture.path())?;
    let events = store.saved()?;
    assert_eq!(events.len(), 2);
    assert_eq!(events.iter().map(|event| event.input).sum::<i64>(), 130);
    assert_eq!(events.iter().map(|event| event.output).sum::<i64>(), 12);
    Ok(())
}

#[test]
fn a_failed_write_rolls_back_the_file_and_can_be_retried() -> anyhow::Result<()> {
    let fixture = tempfile::tempdir()?;
    let home = fixture.path().join("codex");
    let sessions = home.join("sessions");
    fs::create_dir_all(&sessions)?;
    let database = fixture.path().join("edith.sqlite");
    let store = UsageStore::new(database.clone());
    store.scan(&home)?;

    let connection = rusqlite::Connection::open(database)?;
    connection.execute_batch(
        "CREATE TRIGGER fail_import BEFORE INSERT ON usage_events WHEN NEW.input = 60
         BEGIN SELECT RAISE(ABORT, 'synthetic write failure'); END;",
    )?;
    fs::write(
        sessions.join("rollout.jsonl"),
        r#"{"type":"session_meta","payload":{"id":"fixture-session"}}
{"timestamp":"2025-06-01T12:00:00Z","type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":100,"output_tokens":10}}}}
{"timestamp":"2025-06-01T13:00:00Z","type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":160,"output_tokens":14}}}}
"#,
    )?;
    assert!(store.scan(&home).is_err());
    assert!(store.saved()?.is_empty());

    connection.execute_batch("DROP TRIGGER fail_import;")?;
    store.scan(&home)?;
    assert_eq!(
        store
            .saved()?
            .iter()
            .map(|event| event.input)
            .collect::<Vec<_>>(),
        vec![100, 60]
    );
    Ok(())
}

#[test]
fn imports_claude_usage_into_an_existing_codex_database() -> anyhow::Result<()> {
    let fixture = tempfile::tempdir()?;
    let home = fixture.path();
    let database = home.join("edith.sqlite");
    let legacy = rusqlite::Connection::open(&database)?;
    legacy.execute_batch(
        "CREATE TABLE usage_events (
            identity BLOB NOT NULL PRIMARY KEY,
            timestamp INTEGER NOT NULL, model TEXT NOT NULL,
            input INTEGER NOT NULL, cached INTEGER NOT NULL, output INTEGER NOT NULL
         );
         INSERT INTO usage_events VALUES (X'01', 0, 'codex-model', 100, 20, 10);",
    )?;
    drop(legacy);

    let project = home.join("projects/project");
    fs::create_dir_all(&project)?;
    fs::write(
        project.join("session.jsonl"),
        r#"{"type":"assistant","timestamp":"2025-06-01T12:00:00Z","uuid":"entry-1","requestId":"req_1","message":{"id":"msg_1","model":"claude-sonnet","content":[{"type":"text","text":"SYNTHETIC_RESPONSE_MARKER"}],"usage":{"input_tokens":5,"cache_creation_input_tokens":30,"cache_read_input_tokens":65,"output_tokens":12}}}
"#,
    )?;
    let store = UsageStore::new(database.clone());
    assert_eq!(store.saved()?[0].agent, usage::UsageAgent::Codex);
    store.scan_claude(home)?;
    let saved = store.saved()?;
    assert_eq!(saved.len(), 2);
    assert_eq!(saved[0].agent, usage::UsageAgent::Codex);
    assert_eq!(
        (saved[0].input, saved[0].cached, saved[0].output),
        (100, 20, 10)
    );
    let claude = &saved[1];
    assert_eq!(claude.agent, usage::UsageAgent::Claude);
    assert_eq!(claude.model, "claude-sonnet");
    assert_eq!((claude.input, claude.cached, claude.output), (100, 65, 12));
    assert!(
        !fs::read(database)?
            .windows(b"SYNTHETIC_RESPONSE_MARKER".len())
            .any(|bytes| bytes == b"SYNTHETIC_RESPONSE_MARKER")
    );
    Ok(())
}

#[test]
fn claude_rescan_merges_replayed_messages_and_adds_one_new_event() -> anyhow::Result<()> {
    let fixture = tempfile::tempdir()?;
    let project = fixture.path().join("projects/project");
    let subagents = project.join("session/subagents");
    fs::create_dir_all(&subagents)?;
    let session = project.join("session.jsonl");
    fs::write(
        &session,
        r#"{"type":"assistant","timestamp":"2025-06-01T12:00:00Z","uuid":"entry-1","requestId":"req_1","message":{"id":"msg_1","model":"claude-sonnet","usage":{"input_tokens":40,"output_tokens":1}}}
{"type":"assistant","timestamp":"2025-06-01T12:00:01Z","uuid":"entry-2","requestId":"req_1","message":{"id":"msg_1","model":"claude-sonnet","usage":{"input_tokens":40,"output_tokens":12}}}
"#,
    )?;
    fs::copy(&session, subagents.join("agent.jsonl"))?;
    let database = fixture.path().join("edith.sqlite");
    let store = UsageStore::new(database.clone());
    let totals = |store: &UsageStore| -> anyhow::Result<_> {
        let events = store.saved()?;
        Ok((
            events.len(),
            events.iter().map(|event| event.input).sum::<i64>(),
            events.iter().map(|event| event.output).sum::<i64>(),
        ))
    };
    for _ in 0..2 {
        store.scan_claude(fixture.path())?;
        assert_eq!(totals(&store)?, (1, 40, 12));
    }

    let mut log = fs::OpenOptions::new().append(true).open(&session)?;
    write!(
        log,
        "{}",
        r#"{"type":"assistant","timestamp":"2025-06-01T13:00:00Z","uuid":"entry-3","requestId":"req_2","message":{"id":"msg_2","model":"claude-sonnet","usage":{"input_tokens":10,"output_tokens":4}}}"#,
    )?;
    store.scan_claude(fixture.path())?;
    assert_eq!(totals(&store)?, (1, 40, 12));
    writeln!(log)?;
    drop(log);
    let reopened = UsageStore::new(database);
    for _ in 0..2 {
        reopened.scan_claude(fixture.path())?;
        assert_eq!(totals(&reopened)?, (2, 50, 16));
    }
    fs::remove_dir_all(fixture.path().join("projects"))?;
    reopened.scan_claude(fixture.path())?;
    assert_eq!(totals(&reopened)?, (2, 50, 16));
    Ok(())
}
