use std::fs::{self, File, OpenOptions};
use std::io::{self, BufRead, BufReader, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::json;
use tempfile::TempDir;
use uuid::Uuid;

use super::protocol::Agent;

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub(crate) enum AgentState {
    Working,
    Attention,
    Ready,
    #[default]
    Open,
    Closed,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub(crate) struct AttentionStatus {
    pub(crate) state: AgentState,
    pub(crate) estimated: bool,
    pub(crate) confirmed_request: Option<u64>,
}

impl AttentionStatus {
    fn estimated(state: AgentState) -> Self {
        Self {
            state,
            estimated: true,
            confirmed_request: None,
        }
    }

    pub(crate) fn closed() -> Self {
        Self {
            state: AgentState::Closed,
            ..Self::default()
        }
    }
}

pub(super) fn estimate(agent: Agent, visible_screen: &str) -> AttentionStatus {
    for line in visible_screen.lines().rev().map(str::trim) {
        let line = line.trim_start_matches(['•', '✦', '✻', '⏺']).trim_start();
        let attention = match agent {
            Agent::Claude => line.starts_with("Do you want to proceed?"),
            Agent::Codex => line.starts_with("Would you like to run the following command?"),
            Agent::Copilot => line.starts_with("Do you want to run this command?"),
        };
        let working = match agent {
            Agent::Claude => line.contains("esc to interrupt") && line.contains('('),
            Agent::Codex => line.starts_with("Working (") && line.contains("esc to interrupt"),
            Agent::Copilot => line.starts_with("Thinking…") || line.starts_with("Thinking..."),
        };
        if attention {
            return AttentionStatus::estimated(AgentState::Attention);
        }
        if working {
            return AttentionStatus::estimated(AgentState::Working);
        }
    }
    let prompt = match agent {
        Agent::Claude => "❯",
        Agent::Codex => "›",
        Agent::Copilot => "❯",
    };
    if visible_screen.lines().any(|line| line.trim() == prompt) {
        AttentionStatus::estimated(AgentState::Ready)
    } else {
        AttentionStatus::default()
    }
}

pub(crate) const HOOK_ARGUMENT: &str = "--claude-attention-event";
const SIGNAL_FILE: &str = "claude-signals";
const MAX_HOOK_INPUT: u64 = 64 * 1024;

pub(super) struct ClaudeHook {
    _directory: TempDir,
    settings: PathBuf,
    signals: PathBuf,
    offset: u64,
    requests: u64,
    status: Option<AttentionStatus>,
}

impl ClaudeHook {
    pub(super) fn create(session: Uuid) -> Result<Self> {
        let directory = tempfile::Builder::new()
            .prefix("edith-attention-")
            .tempdir()?;
        let settings = directory.path().join("settings.json");
        let signals = directory.path().join(SIGNAL_FILE);
        File::create_new(&signals)?;
        let executable = std::env::current_exe()?;
        let signal_argument = signals.to_str().context("non-UTF-8 Claude hook path")?;
        let hook = json!({
            "type": "command",
            "command": executable,
            "args": [HOOK_ARGUMENT, signal_argument, &session.to_string()],
            "timeout": 2,
        });
        let config = json!({"hooks": {
            "Notification": [{"matcher": "permission_prompt|elicitation_dialog", "hooks": [hook]}],
            "UserPromptSubmit": [{"hooks": [hook]}],
            "Stop": [{"hooks": [hook]}],
        }});
        fs::write(&settings, serde_json::to_vec(&config)?)?;
        Ok(Self {
            _directory: directory,
            settings,
            signals,
            offset: 0,
            requests: 0,
            status: None,
        })
    }

    pub(super) fn settings(&self) -> &Path {
        &self.settings
    }

    pub(super) fn read_status(&mut self) -> io::Result<Option<AttentionStatus>> {
        let mut file = File::open(&self.signals)?;
        file.seek(SeekFrom::Start(self.offset))?;
        let mut reader = BufReader::new(file.take(MAX_HOOK_INPUT));
        let mut line = Vec::new();
        while reader.read_until(b'\n', &mut line)? != 0 && line.ends_with(b"\n") {
            self.offset += line.len() as u64;
            self.record(&line);
            line.clear();
        }
        Ok(self.status)
    }

    fn record(&mut self, signal: &[u8]) {
        let state = match signal {
            b"working\n" => AgentState::Working,
            b"attention\n" => AgentState::Attention,
            b"ready\n" => AgentState::Ready,
            _ => return,
        };
        let confirmed_request = (state == AgentState::Attention).then(|| {
            if self.status.is_none_or(|previous| previous.state != state) {
                self.requests = self.requests.saturating_add(1);
            }
            self.requests
        });
        self.status = Some(AttentionStatus {
            state,
            estimated: false,
            confirmed_request,
        });
    }
}

#[derive(Deserialize)]
struct ClaudeEvent {
    session_id: String,
    hook_event_name: String,
    notification_type: Option<String>,
}

fn parse_signal(input: &[u8], session: Uuid) -> Option<&'static [u8]> {
    let event: ClaudeEvent = serde_json::from_slice(input).ok()?;
    if Uuid::parse_str(&event.session_id).ok()? != session {
        return None;
    }
    match (
        event.hook_event_name.as_str(),
        event.notification_type.as_deref(),
    ) {
        ("Notification", Some("permission_prompt" | "elicitation_dialog")) => Some(b"attention\n"),
        ("UserPromptSubmit", None) => Some(b"working\n"),
        ("Stop", None) => Some(b"ready\n"),
        _ => None,
    }
}

pub(crate) fn record_claude_hook(path: &Path, session: &str) -> Result<()> {
    let mut input = Vec::new();
    io::stdin()
        .lock()
        .take(MAX_HOOK_INPUT + 1)
        .read_to_end(&mut input)?;
    if input.len() as u64 > MAX_HOOK_INPUT
        || path.file_name().is_none_or(|name| name != SIGNAL_FILE)
    {
        return Ok(());
    }
    if let Some(signal) = parse_signal(&input, Uuid::parse_str(session)?) {
        OpenOptions::new()
            .append(true)
            .open(path)?
            .write_all(signal)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn terminal_attention_is_estimated_and_has_no_notification_id() {
        let status = estimate(Agent::Claude, "Do you want to proceed?");
        assert_eq!(status.state, AgentState::Attention);
        assert!(status.estimated);
        assert_eq!(status.confirmed_request, None);
        assert_eq!(
            estimate(Agent::Claude, "unknown output"),
            AttentionStatus::default()
        );
    }

    #[test]
    fn only_matching_session_and_pending_request_are_confirmed() {
        let session = Uuid::new_v4();
        let input = |event: &str, notification: &str| {
            serde_json::to_vec(&json!({
                "session_id": session.to_string(),
                "hook_event_name": event,
                "notification_type": notification,
                "message": "tool output is never persisted",
            }))
            .unwrap()
        };
        let attention = input("Notification", "permission_prompt");
        assert_eq!(
            parse_signal(&attention, session),
            Some(b"attention\n".as_slice())
        );
        assert_eq!(parse_signal(&attention, Uuid::new_v4()), None);
        assert_eq!(
            parse_signal(&input("Notification", "idle_prompt"), session),
            None
        );
        assert_eq!(
            parse_signal(&input("PermissionRequest", "permission_prompt"), session),
            None
        );
    }

    #[test]
    fn duplicate_hook_events_share_id_until_status_changes() {
        let mut hook = ClaudeHook::create(Uuid::new_v4()).unwrap();
        let mut writer = OpenOptions::new().append(true).open(&hook.signals).unwrap();
        assert_eq!(hook.read_status().unwrap(), None);
        writer.write_all(b"attention\nattention\nrea").unwrap();
        assert_eq!(
            hook.read_status().unwrap().unwrap().confirmed_request,
            Some(1)
        );
        writer.write_all(b"dy\n").unwrap();
        assert_eq!(
            hook.read_status().unwrap().unwrap().state,
            AgentState::Ready
        );
        writer.write_all(b"attention\n").unwrap();
        assert_eq!(
            hook.read_status().unwrap().unwrap().confirmed_request,
            Some(2)
        );
    }
}
