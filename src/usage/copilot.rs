use chrono::DateTime;
use serde_json::Value;
use sha2::{Digest, Sha256};

use super::{UsageAgent, UsageEvent, identifier};

pub(super) fn normalize(line: &[u8]) -> Option<(Vec<u8>, UsageEvent)> {
    let record: Value = serde_json::from_slice(line).ok()?;
    if record["type"] != "session.usage_record" {
        return None;
    }

    let timestamp = DateTime::parse_from_rfc3339(record["timestamp"].as_str()?).ok()?;
    let usage = &record["data"]["usage"];
    let accounting_id = identifier(&usage["accounting"]["usageId"]);
    let id = accounting_id.or_else(|| identifier(&record["id"]))?;
    let token_count = |key: &str| usage[key].as_i64().filter(|&amount| amount >= 0);
    let event = UsageEvent {
        agent: UsageAgent::Copilot,
        timestamp: timestamp.timestamp(),
        model: identifier(&usage["model"])
            .unwrap_or("Modelo desconocido")
            .to_owned(),
        input: token_count("inputTokens"),
        cached: token_count("cacheReadTokens"),
        output: token_count("outputTokens"),
        cache_write: token_count("cacheWriteTokens"),
        reasoning: token_count("reasoningTokens"),
        api_duration_ms: token_count("duration"),
    };
    if [
        event.input,
        event.cached,
        event.output,
        event.cache_write,
        event.reasoning,
        event.api_duration_ms,
    ]
    .into_iter()
    .all(|count| count.is_none())
    {
        return None;
    }

    // Copies of a transcript retain the event ID. Shutdown summaries overlap these deltas.
    let mut identity = Sha256::new();
    identity.update(b"copilot\0");
    identity.update([u8::from(accounting_id.is_some())]);
    identity.update(id.as_bytes());
    Some((identity.finalize().to_vec(), event))
}
