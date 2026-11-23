use std::{
    borrow::Cow,
    fs::{self, File},
    io::{self, BufRead, BufReader, Read, Seek, SeekFrom},
    path::{Path, PathBuf},
    time::Duration,
};

use anyhow::Result;
use chrono::DateTime;
use rusqlite::{Connection, OpenFlags, OptionalExtension, TransactionBehavior, params};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

#[path = "usage/copilot.rs"]
mod copilot;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum UsageAgent {
    Codex,
    Claude,
    Copilot,
}

impl UsageAgent {
    fn key(self) -> &'static str {
        match self {
            Self::Codex => "codex",
            Self::Claude => "claude",
            Self::Copilot => "copilot",
        }
    }
}

#[derive(Debug)]
pub struct UsageEvent {
    pub agent: UsageAgent,
    pub timestamp: i64,
    pub model: String,
    pub input: Option<i64>,
    pub cached: Option<i64>,
    pub output: Option<i64>,
    pub cache_write: Option<i64>,
    pub reasoning: Option<i64>,
    pub api_duration_ms: Option<i64>,
}

pub struct UsageStore {
    database: PathBuf,
}

impl UsageStore {
    pub fn new(database: PathBuf) -> Self {
        Self { database }
    }

    pub fn saved(&self) -> Result<Vec<UsageEvent>> {
        if !self.database.try_exists()? {
            return Ok(Vec::new());
        }
        let mut database =
            Connection::open_with_flags(&self.database, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
        database.busy_timeout(Duration::from_secs(5))?;
        let snapshot = database.transaction()?;
        let agent_column = agent_column(&snapshot, "usage_events")?;
        let extra_columns = if has_column(&snapshot, "api_duration_ms")? {
            "cache_write, reasoning, api_duration_ms"
        } else {
            "NULL, NULL, NULL"
        };
        let mut query = snapshot.prepare(&format!(
            "SELECT timestamp, model, input, cached, output, {agent_column}, {extra_columns}
             FROM usage_events ORDER BY timestamp",
        ))?;
        let events = query.query_map([], |row| {
            let agent = match row.get::<_, String>(5)?.as_str() {
                "codex" => UsageAgent::Codex,
                "claude" => UsageAgent::Claude,
                "copilot" => UsageAgent::Copilot,
                _ => return Err(rusqlite::Error::InvalidQuery),
            };
            Ok(UsageEvent {
                agent,
                timestamp: row.get(0)?,
                model: row.get(1)?,
                input: row.get(2)?,
                cached: row.get(3)?,
                output: row.get(4)?,
                cache_write: row.get(6)?,
                reasoning: row.get(7)?,
                api_duration_ms: row.get(8)?,
            })
        })?;
        Ok(events.collect::<rusqlite::Result<_>>()?)
    }

    pub fn scan(&self, codex_home: &Path) -> Result<()> {
        self.scan_directories(
            vec![
                codex_home.join("sessions"),
                codex_home.join("archived_sessions"),
            ],
            UsageAgent::Codex,
        )
    }

    pub fn scan_claude(&self, claude_home: &Path) -> Result<()> {
        self.scan_directories(vec![claude_home.join("projects")], UsageAgent::Claude)
    }

    pub fn scan_copilot(&self, copilot_home: &Path) -> Result<()> {
        self.scan_directories(
            vec![copilot_home.join("session-state")],
            UsageAgent::Copilot,
        )
    }

    pub fn erase(&self, agent: Option<UsageAgent>) -> Result<()> {
        let mut database = self.open_for_import()?;
        database.pragma_update(None, "secure_delete", true)?;
        let transaction = database.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let agent = agent.map(UsageAgent::key);
        // Keep only the erased identities so an archived or rewritten log cannot restore them.
        // Source checkpoints still provide the baseline for the next cumulative token record.
        transaction.execute(
            "INSERT OR IGNORE INTO erased_usage_events (identity)
             SELECT identity FROM usage_events WHERE ?1 IS NULL OR agent = ?1",
            [agent],
        )?;
        transaction.execute(
            "DELETE FROM usage_events WHERE ?1 IS NULL OR agent = ?1",
            [agent],
        )?;
        transaction.commit()?;
        Ok(())
    }

    fn open_for_import(&self) -> Result<Connection> {
        if let Some(parent) = self.database.parent() {
            fs::create_dir_all(parent)?;
        }
        let mut database = Connection::open(&self.database)?;
        database.busy_timeout(Duration::from_secs(5))?;
        database.execute_batch("PRAGMA journal_mode = WAL;")?;
        let schema = database.transaction_with_behavior(TransactionBehavior::Immediate)?;
        if !has_column(&schema, "api_duration_ms")? {
            let legacy = has_column(&schema, "identity")?;
            if legacy {
                schema.execute_batch("ALTER TABLE usage_events RENAME TO legacy_usage_events;")?;
            }
            schema.execute_batch(
                "CREATE TABLE usage_events (
                    identity BLOB NOT NULL PRIMARY KEY,
                    timestamp INTEGER NOT NULL,
                    model TEXT NOT NULL,
                    input INTEGER,
                    cached INTEGER,
                    output INTEGER,
                    agent TEXT NOT NULL DEFAULT 'codex',
                    cache_write INTEGER,
                    reasoning INTEGER,
                    api_duration_ms INTEGER
                 );",
            )?;
            if legacy {
                let agent = agent_column(&schema, "legacy_usage_events")?;
                schema.execute_batch(&format!(
                    "INSERT INTO usage_events (identity, timestamp, model, input, cached, output, agent)
                     SELECT identity, timestamp, model, input, cached, output, {agent} FROM legacy_usage_events;
                     DROP TABLE legacy_usage_events;",
                ))?;
            }
        }
        schema.execute_batch(
            "CREATE TABLE IF NOT EXISTS usage_checkpoints (
                source BLOB NOT NULL PRIMARY KEY,
                state TEXT NOT NULL
             );
             CREATE TABLE IF NOT EXISTS erased_usage_events (
                identity BLOB NOT NULL PRIMARY KEY
             ) WITHOUT ROWID;",
        )?;
        schema.commit()?;
        Ok(database)
    }

    fn scan_directories(&self, mut directories: Vec<PathBuf>, agent: UsageAgent) -> Result<()> {
        let mut database = self.open_for_import()?;

        let mut failure = None;
        while let Some(directory) = directories.pop() {
            let entries = match fs::read_dir(directory) {
                Ok(entries) => entries,
                Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
                Err(error) => {
                    failure = Some(error.into());
                    continue;
                }
            };
            for entry in entries {
                let result = (|| -> Result<()> {
                    let entry = entry?;
                    let kind = entry.file_type()?;
                    let path = entry.path();
                    if kind.is_dir() {
                        directories.push(path);
                    } else if kind.is_file() && is_usage_log(&path, agent) {
                        import_file(&mut database, &path, agent)?;
                    }
                    Ok(())
                })();
                if let Err(error) = result {
                    failure = Some(error);
                }
            }
        }
        failure.map_or(Ok(()), Err)
    }
}

fn is_usage_log(path: &Path, agent: UsageAgent) -> bool {
    match agent {
        UsageAgent::Copilot => path.file_name().is_some_and(|name| name == "events.jsonl"),
        _ => path
            .extension()
            .is_some_and(|extension| extension == "jsonl"),
    }
}

fn agent_column(database: &Connection, table: &str) -> rusqlite::Result<&'static str> {
    let present: bool = database.query_row(
        "SELECT EXISTS(SELECT 1 FROM pragma_table_info(?1) WHERE name = 'agent')",
        [table],
        |row| row.get(0),
    )?;
    Ok(if present { "agent" } else { "'codex'" })
}

fn has_column(database: &Connection, name: &str) -> rusqlite::Result<bool> {
    database.query_row(
        "SELECT EXISTS(SELECT 1 FROM pragma_table_info('usage_events') WHERE name = ?1)",
        [name],
        |row| row.get(0),
    )
}

#[derive(Default, Deserialize, Serialize)]
struct Checkpoint {
    offset: u64,
    anchor: Vec<u8>,
    session: String,
    model: String,
    total: Option<Tokens>,
}

#[derive(Clone, Copy, Default, Deserialize, Serialize, PartialEq)]
struct Tokens {
    #[serde(rename = "input_tokens")]
    input: i64,
    #[serde(rename = "cached_input_tokens", default)]
    cached: i64,
    #[serde(rename = "output_tokens")]
    output: i64,
}

impl Tokens {
    fn read(value: &Value) -> Option<Self> {
        let tokens: Self = serde_json::from_value(value.clone()).ok()?;
        (tokens.input >= 0
            && tokens.cached >= 0
            && tokens.output >= 0
            && tokens.cached <= tokens.input)
            .then_some(tokens)
    }

    fn since(self, previous: Self) -> Option<Self> {
        let delta = Self {
            input: self.input - previous.input,
            cached: self.cached - previous.cached,
            output: self.output - previous.output,
        };
        (delta.input >= 0 && delta.cached >= 0 && delta.output >= 0 && delta.cached <= delta.input)
            .then_some(delta)
    }
}

const ANCHOR_SIZE: usize = 256;

fn anchor(reader: &mut BufReader<File>, offset: u64) -> io::Result<Vec<u8>> {
    let length = offset.min(ANCHOR_SIZE as u64);
    let mut bytes = vec![0; length as usize];
    reader.seek(SeekFrom::Start(offset - length))?;
    reader.read_exact(&mut bytes)?;
    Ok(Sha256::digest(bytes).to_vec())
}

fn import_file(database: &mut Connection, path: &Path, agent: UsageAgent) -> Result<()> {
    let mut reader = match File::open(path) {
        Ok(file) => BufReader::new(file),
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error.into()),
    };
    let transaction = database.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let path_bytes = path.as_os_str().as_encoded_bytes();
    // Keep existing Codex and Claude checkpoints. New Copilot checkpoints store a digest.
    let source = match agent {
        UsageAgent::Codex => Cow::Borrowed(path_bytes),
        UsageAgent::Claude => Cow::Owned([b"claude\0", path_bytes].concat()),
        UsageAgent::Copilot => {
            let mut digest = Sha256::new();
            digest.update(b"copilot\0");
            digest.update(path_bytes);
            Cow::Owned(digest.finalize().to_vec())
        }
    };
    let source = source.as_ref();
    let previous: Option<String> = transaction
        .query_row(
            "SELECT state FROM usage_checkpoints WHERE source = ?1",
            [source],
            |row| row.get(0),
        )
        .optional()?;
    let mut state: Checkpoint = previous
        .as_deref()
        .map(serde_json::from_str)
        .transpose()?
        .unwrap_or_default();
    if state.offset != 0
        && (reader.get_ref().metadata()?.len() < state.offset
            || anchor(&mut reader, state.offset)? != state.anchor)
    {
        state = Checkpoint::default();
    }
    reader.seek(SeekFrom::Start(state.offset))?;
    let start = state.offset;
    let mut line = Vec::new();
    let mut insert = transaction.prepare(
        "INSERT INTO usage_events (identity, timestamp, model, input, cached, output, agent, cache_write, reasoning, api_duration_ms)
         SELECT ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10
         WHERE NOT EXISTS (SELECT 1 FROM erased_usage_events WHERE identity = ?1)
         ON CONFLICT (identity) DO UPDATE SET
            timestamp = min(usage_events.timestamp, excluded.timestamp),
            input = max(usage_events.input, excluded.input),
            cached = max(usage_events.cached, excluded.cached),
            output = max(usage_events.output, excluded.output)
         WHERE excluded.agent = 'claude' AND usage_events.agent = 'claude'
            AND (excluded.timestamp < usage_events.timestamp
                 OR excluded.input > usage_events.input
                 OR excluded.cached > usage_events.cached
                 OR excluded.output > usage_events.output)",
    )?;
    loop {
        line.clear();
        reader.read_until(b'\n', &mut line)?;
        if line.last() != Some(&b'\n') {
            break;
        }
        state.offset += line.len() as u64;
        let event = match agent {
            UsageAgent::Codex => normalize(&line, &mut state),
            UsageAgent::Claude => normalize_claude(&line),
            UsageAgent::Copilot => copilot::normalize(&line),
        };
        if let Some((identity, event)) = event {
            insert.execute(params![
                identity,
                event.timestamp,
                event.model,
                event.input,
                event.cached,
                event.output,
                event.agent.key(),
                event.cache_write,
                event.reasoning,
                event.api_duration_ms
            ])?;
        }
    }
    drop(insert);
    if state.offset != start || (state.offset == 0 && previous.is_some()) {
        state.anchor = anchor(&mut reader, state.offset)?;
        transaction.execute(
            "INSERT INTO usage_checkpoints VALUES (?1, ?2)
             ON CONFLICT (source) DO UPDATE SET state = excluded.state",
            params![source, serde_json::to_string(&state)?],
        )?;
    }
    transaction.commit()?;
    Ok(())
}

fn identifier(value: &Value) -> Option<&str> {
    let id = value.as_str()?;
    (!id.is_empty()
        && id.len() <= 128
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"-_./:".contains(&b)))
    .then_some(id)
}

fn normalize(line: &[u8], state: &mut Checkpoint) -> Option<(Vec<u8>, UsageEvent)> {
    let record: Value = serde_json::from_slice(line).ok()?;
    let payload = &record["payload"];
    match record["type"].as_str()? {
        "session_meta" => {
            if let Some(session) = identifier(&payload["id"]) {
                state.session = session.to_owned();
            }
            return None;
        }
        "turn_context" => {
            state.model = identifier(&payload["model"]).unwrap_or_default().to_owned();
            return None;
        }
        "event_msg" if payload["type"] == "token_count" && !state.session.is_empty() => {}
        _ => return None,
    }

    let timestamp = DateTime::parse_from_rfc3339(record["timestamp"].as_str()?).ok()?;
    let info = &payload["info"];
    let total = Tokens::read(&info["total_token_usage"]);
    let last = Tokens::read(&info["last_token_usage"]);
    let tokens = match total {
        Some(total) => {
            let delta = total.since(state.total.unwrap_or_default());
            state.total = Some(total);
            delta.or(last).unwrap_or(total)
        }
        None => {
            let last = last?;
            let previous = state.total.unwrap_or_default();
            state.total = Some(Tokens {
                input: previous.input.saturating_add(last.input),
                cached: previous.cached.saturating_add(last.cached),
                output: previous.output.saturating_add(last.output),
            });
            last
        }
    };
    if tokens == Tokens::default() {
        return None;
    }

    // A moved or archived copy retains the session and event identity.
    let identity = serde_json::to_vec(&(
        state.session.as_str(),
        timestamp.to_utc().to_rfc3339(),
        total.or(last),
    ))
    .ok()?;
    let model = identifier(&info["model"])
        .or_else(|| identifier(&payload["model"]))
        .or_else(|| (!state.model.is_empty()).then_some(state.model.as_str()))
        .unwrap_or("Modelo desconocido");
    Some((
        Sha256::digest(identity).to_vec(),
        UsageEvent {
            agent: UsageAgent::Codex,
            timestamp: timestamp.timestamp(),
            model: model.to_owned(),
            input: Some(tokens.input),
            cached: Some(tokens.cached),
            output: Some(tokens.output),
            cache_write: None,
            reasoning: None,
            api_duration_ms: None,
        },
    ))
}

fn normalize_claude(line: &[u8]) -> Option<(Vec<u8>, UsageEvent)> {
    let record: Value = serde_json::from_slice(line).ok()?;
    if record["type"] != "assistant" {
        return None;
    }

    let message = &record["message"];
    let model = identifier(&message["model"])?;
    let usage = &message["usage"];
    let uncached = usage["input_tokens"].as_u64()?;
    let output = usage["output_tokens"].as_i64().filter(|&n| n >= 0)?;
    let cache_tokens = |key: &str| usage.get(key).map_or(Some(0), Value::as_u64);
    let cached = cache_tokens("cache_read_input_tokens")?;
    let written = cache_tokens("cache_creation_input_tokens")?;
    let input = i64::try_from(uncached.checked_add(cached)?.checked_add(written)?).ok()?;
    if input == 0 && output == 0 {
        return None;
    }

    let timestamp = DateTime::parse_from_rfc3339(record["timestamp"].as_str()?).ok()?;
    // Content blocks and transcript copies can repeat the same API response.
    // Its token counters may grow as streaming completes.
    let identity = if let Some(message_id) = identifier(&message["id"]) {
        serde_json::to_vec(&(
            "claude",
            "message",
            message_id,
            identifier(&record["requestId"]),
        ))
    } else {
        serde_json::to_vec(&("claude", "entry", identifier(&record["uuid"])?))
    }
    .ok()?;
    Some((
        Sha256::digest(identity).to_vec(),
        UsageEvent {
            agent: UsageAgent::Claude,
            timestamp: timestamp.timestamp(),
            model: model.to_owned(),
            input: Some(input),
            cached: Some(i64::try_from(cached).ok()?),
            output: Some(output),
            cache_write: None,
            reasoning: None,
            api_duration_ms: None,
        },
    ))
}
