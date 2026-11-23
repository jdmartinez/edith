#[path = "../src/usage.rs"]
mod usage;

use std::{fs, io::Write};
use usage::{UsageAgent, UsageStore};

#[test]
fn erases_selected_metrics_without_reimporting_deleted_records() -> anyhow::Result<()> {
    let fixture = tempfile::tempdir()?;
    let home = fixture.path();
    let sessions = home.join("sessions");
    let archived = home.join("archived_sessions");
    let projects = home.join("projects");
    for directory in [&sessions, &archived, &projects] {
        fs::create_dir(directory)?;
    }
    let rollout = sessions.join("original.jsonl");
    fs::write(
        &rollout,
        r#"{"type":"session_meta","payload":{"id":"fixture-session"}}
{"timestamp":"2025-06-01T12:00:00Z","type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":100,"output_tokens":10}}}}
"#,
    )?;
    let claude = projects.join("session.jsonl");
    fs::write(
        &claude,
        r#"{"type":"assistant","timestamp":"2025-06-01T12:01:00Z","uuid":"entry-1","message":{"model":"fixture","usage":{"input_tokens":8,"output_tokens":1}}}
"#,
    )?;
    let database = home.join("usage.sqlite");
    let store = UsageStore::new(database.clone());
    store.scan(home)?;
    store.scan_claude(home)?;
    assert_eq!(store.saved()?.len(), 2);

    store.erase(Some(UsageAgent::Codex))?;
    assert_eq!(store.saved()?[0].agent, UsageAgent::Claude);
    fs::copy(&rollout, archived.join("copy.jsonl"))?;
    writeln!(
        fs::OpenOptions::new().append(true).open(&rollout)?,
        "{}",
        serde_json::json!({
            "timestamp": "2025-06-01T13:00:00Z",
            "type": "event_msg",
            "payload": {
                "type": "token_count",
                "info": { "total_token_usage": { "input_tokens": 130, "output_tokens": 12 } }
            }
        }),
    )?;
    let store = UsageStore::new(database);
    store.scan(home)?;
    let events = store.saved()?;
    assert_eq!(events.len(), 2);
    assert_eq!(
        (events[1].agent, events[1].input),
        (UsageAgent::Codex, Some(30))
    );

    store.erase(None)?;
    store.erase(None)?;
    fs::rename(&rollout, archived.join("moved.jsonl"))?;
    fs::rename(&claude, projects.join("moved.jsonl"))?;
    store.scan(home)?;
    store.scan_claude(home)?;
    assert!(store.saved()?.is_empty());
    Ok(())
}

#[test]
fn erasure_failure_rolls_back_tombstones() -> anyhow::Result<()> {
    let fixture = tempfile::tempdir()?;
    let home = fixture.path();
    let projects = home.join("projects");
    fs::create_dir(&projects)?;
    fs::write(
        projects.join("session.jsonl"),
        r#"{"type":"assistant","timestamp":"2025-06-01T12:01:00Z","uuid":"entry-1","message":{"model":"fixture","usage":{"input_tokens":8,"output_tokens":1}}}
"#,
    )?;
    let database = home.join("usage.sqlite");
    let store = UsageStore::new(database.clone());
    store.scan_claude(home)?;
    let connection = rusqlite::Connection::open(database)?;
    connection.execute_batch(
        "CREATE TRIGGER fail_erase BEFORE DELETE ON usage_events
         BEGIN SELECT RAISE(ABORT, 'synthetic erase failure'); END;",
    )?;
    assert!(store.erase(None).is_err());
    assert_eq!(store.saved()?.len(), 1);
    let tombstones: i64 =
        connection.query_row("SELECT count(*) FROM erased_usage_events", [], |row| {
            row.get(0)
        })?;
    assert_eq!(tombstones, 0);
    connection.execute_batch("DROP TRIGGER fail_erase;")?;
    store.erase(None)?;
    assert!(store.saved()?.is_empty());
    Ok(())
}

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
        (Some(100), Some(20), Some(10))
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
        (Some(60), Some(10), Some(4))
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
    assert_eq!(
        events.iter().filter_map(|event| event.input).sum::<i64>(),
        130
    );
    assert_eq!(
        events.iter().filter_map(|event| event.output).sum::<i64>(),
        12
    );
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
            .filter_map(|event| event.input)
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
        (Some(100), Some(20), Some(10))
    );
    let claude = &saved[1];
    assert_eq!(claude.agent, usage::UsageAgent::Claude);
    assert_eq!(claude.model, "claude-sonnet");
    assert_eq!(
        (claude.input, claude.cached, claude.output),
        (Some(100), Some(65), Some(12))
    );
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
            events.iter().filter_map(|event| event.input).sum::<i64>(),
            events.iter().filter_map(|event| event.output).sum::<i64>(),
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

#[test]
fn imports_only_copilot_usage_deltas_and_preserves_sparse_counters() -> anyhow::Result<()> {
    let fixture = tempfile::tempdir()?;
    let home = fixture.path();
    let session = home.join("session-state/session");
    let copy = home.join("session-state/copy");
    fs::create_dir_all(&session)?;
    fs::create_dir_all(&copy)?;
    let delta = r#"{"type":"session.usage_record","id":"event-1","timestamp":"2025-06-01T12:00:00Z","data":{"usage":{"model":"fixture-model","accounting":{"usageId":"usage-1"},"inputTokens":20,"cacheReadTokens":0,"outputTokens":3,"cacheWriteTokens":1,"duration":42},"content":"SYNTHETIC_COPILOT_PROMPT"}}"#;
    let sparse = r#"{"type":"session.usage_record","id":"event-2","timestamp":"2025-06-01T13:00:00Z","data":{"usage":{"model":"fixture-model","outputTokens":0,"reasoningTokens":0,"cacheReadTokens":-1}}}"#;
    let shutdown = r#"{"type":"session.shutdown","id":"event-3","timestamp":"2025-06-01T14:00:00Z","data":{"usage":{"model":"fixture-model","inputTokens":200}}}"#;
    let source = session.join("events.jsonl");
    fs::write(&source, format!("{delta}\n{shutdown}\n{sparse}"))?;
    fs::write(session.join("other.jsonl"), format!("{sparse}\n"))?;
    fs::write(
        copy.join("events.jsonl"),
        format!("{}\n", delta.replace("event-1", "copy-event")),
    )?;
    let database = home.join("edith.sqlite");
    let legacy = rusqlite::Connection::open(&database)?;
    legacy.execute_batch(
        "CREATE TABLE usage_events (
            identity BLOB NOT NULL PRIMARY KEY, timestamp INTEGER NOT NULL, model TEXT NOT NULL,
            input INTEGER NOT NULL, cached INTEGER NOT NULL, output INTEGER NOT NULL,
            agent TEXT NOT NULL DEFAULT 'codex'
         );
         INSERT INTO usage_events VALUES (X'01', 0, 'codex-model', 10, 2, 3, 'codex');
         INSERT INTO usage_events VALUES (X'02', 1, 'claude-model', 11, 0, 4, 'claude');",
    )?;
    drop(legacy);
    let store = UsageStore::new(database.clone());
    store.scan_copilot(home)?;
    assert_eq!(store.saved()?.len(), 3);

    writeln!(fs::OpenOptions::new().append(true).open(&source)?)?;
    store.scan_copilot(home)?;
    store.scan_copilot(home)?;
    fs::remove_dir_all(home.join("session-state"))?;
    let reopened = UsageStore::new(database.clone());
    reopened.scan_copilot(home)?;
    let saved = reopened.saved()?;
    assert_eq!(saved.len(), 4);
    assert_eq!(
        (saved[0].agent, saved[0].cached),
        (usage::UsageAgent::Codex, Some(2))
    );
    assert_eq!(
        (saved[1].agent, saved[1].cached),
        (usage::UsageAgent::Claude, Some(0))
    );
    let saved = &saved[2..];
    assert!(
        saved
            .iter()
            .all(|event| event.agent == usage::UsageAgent::Copilot)
    );
    let first = &saved[0];
    assert_eq!(
        (first.input, first.cached, first.output),
        (Some(20), Some(0), Some(3))
    );
    assert_eq!(
        (first.cache_write, first.reasoning, first.api_duration_ms),
        (Some(1), None, Some(42))
    );
    let second = &saved[1];
    assert_eq!(
        (second.input, second.cached, second.output),
        (None, None, Some(0))
    );
    assert_eq!(
        (second.cache_write, second.reasoning, second.api_duration_ms),
        (None, Some(0), None)
    );
    assert!(
        !fs::read(database)?
            .windows(b"SYNTHETIC_COPILOT_PROMPT".len())
            .any(|bytes| bytes == b"SYNTHETIC_COPILOT_PROMPT")
    );
    Ok(())
}
