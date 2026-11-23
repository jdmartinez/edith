use std::{
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

#[derive(Debug)]
pub struct UsageEvent {
    pub timestamp: i64,
    pub model: String,
    pub input: i64,
    pub cached: i64,
    pub output: i64,
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
        let database =
            Connection::open_with_flags(&self.database, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
        database.busy_timeout(Duration::from_secs(5))?;
        let mut query = database.prepare(
            "SELECT timestamp, model, input, cached, output FROM usage_events ORDER BY timestamp",
        )?;
        let events = query.query_map([], |row| {
            Ok(UsageEvent {
                timestamp: row.get(0)?,
                model: row.get(1)?,
                input: row.get(2)?,
                cached: row.get(3)?,
                output: row.get(4)?,
            })
        })?;
        Ok(events.collect::<rusqlite::Result<_>>()?)
    }

    pub fn scan(&self, codex_home: &Path) -> Result<()> {
        if let Some(parent) = self.database.parent() {
            fs::create_dir_all(parent)?;
        }
        let mut database = Connection::open(&self.database)?;
        database.busy_timeout(Duration::from_secs(5))?;
        database.execute_batch(
            "PRAGMA journal_mode = WAL;
             BEGIN;
             CREATE TABLE IF NOT EXISTS usage_events (
                identity BLOB NOT NULL PRIMARY KEY,
                timestamp INTEGER NOT NULL,
                model TEXT NOT NULL,
                input INTEGER NOT NULL,
                cached INTEGER NOT NULL,
                output INTEGER NOT NULL
             );
             CREATE TABLE IF NOT EXISTS usage_checkpoints (
                source BLOB NOT NULL PRIMARY KEY,
                state TEXT NOT NULL
             );
             COMMIT;",
        )?;

        let mut directories = vec![
            codex_home.join("sessions"),
            codex_home.join("archived_sessions"),
        ];
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
                    } else if kind.is_file()
                        && path
                            .extension()
                            .is_some_and(|extension| extension == "jsonl")
                    {
                        import_file(&mut database, &path)?;
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

fn import_file(database: &mut Connection, path: &Path) -> Result<()> {
    let mut reader = match File::open(path) {
        Ok(file) => BufReader::new(file),
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error.into()),
    };
    let transaction = database.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let source = path.as_os_str().as_encoded_bytes();
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
        "INSERT INTO usage_events VALUES (?1, ?2, ?3, ?4, ?5, ?6)
         ON CONFLICT (identity) DO NOTHING",
    )?;
    loop {
        line.clear();
        reader.read_until(b'\n', &mut line)?;
        if line.last() != Some(&b'\n') {
            break;
        }
        state.offset += line.len() as u64;
        if let Some((identity, event)) = normalize(&line, &mut state) {
            insert.execute(params![
                identity,
                event.timestamp,
                event.model,
                event.input,
                event.cached,
                event.output
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
            timestamp: timestamp.timestamp(),
            model: model.to_owned(),
            input: tokens.input,
            cached: tokens.cached,
            output: tokens.output,
        },
    ))
}
