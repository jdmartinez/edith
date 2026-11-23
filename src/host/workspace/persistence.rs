use std::collections::BTreeMap;
use std::fs::File;
use std::io::{self, BufReader, BufWriter, Write};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};

use crate::panels::DesktopLayout;

use super::{WorkspaceState, reattach};
use crate::host::git::Repository;
use crate::host::protocol::{Agent, Reply, Request, Snapshot, default_retention_days};
use crate::host::terminal::Terminal;

const VERSION: u32 = 1;

#[derive(Deserialize, Serialize)]
struct Manifest {
    version: u32,
    layout: DesktopLayout,
    projects: Vec<Repository>,
    agent_executables: BTreeMap<Agent, PathBuf>,
    sessions: Vec<Session>,
    #[serde(default = "default_retention_days")]
    retention_days: Option<u32>,
}

#[derive(Deserialize, Serialize)]
struct Session {
    project: PathBuf,
    conversation: Option<String>,
    snapshot: Snapshot,
    // A failed recovery is retryable even when it did not complete a session.
    closed_at: Option<u64>,
}

pub(in crate::host) struct WorkspaceStore {
    path: PathBuf,
    last_closed: Vec<(bool, Option<u64>)>,
}

impl WorkspaceStore {
    pub(in crate::host) fn open(
        directory: &Path,
        terminals: &mut Vec<Result<Terminal, Snapshot>>,
    ) -> Result<(Self, WorkspaceState)> {
        let mut store = Self {
            path: directory.join("workspace.json"),
            last_closed: Vec::new(),
        };
        let file = match File::open(&store.path) {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                return Ok((store, WorkspaceState::default()));
            }
            Err(error) => return Err(error).context("No se pudo abrir el espacio guardado"),
        };
        let manifest: Manifest = serde_json::from_reader(BufReader::new(file))
            .context("No se pudo leer el espacio guardado")?;
        ensure!(
            manifest.version == VERSION,
            "Versión del espacio guardado incompatible"
        );
        let mut state = WorkspaceState {
            layout: manifest.layout,
            projects: manifest.projects,
            agent_executables: manifest.agent_executables,
            retention_days: manifest.retention_days,
            ..WorkspaceState::default()
        };
        for (pane, session) in manifest.sessions.into_iter().enumerate() {
            let conversation = session
                .conversation
                .as_deref()
                .map(uuid::Uuid::parse_str)
                .transpose()
                .context("El espacio guardado contiene un UUID inválido")?;
            if let Some(conversation) = conversation {
                state.conversations.insert(pane, conversation);
            }
            state.terminal_projects.push(session.project);
            store
                .last_closed
                .push((session.snapshot.exited, session.closed_at));
            if state.terminal_projects[pane].as_os_str().is_empty() {
                terminals.push(Err(session.snapshot));
            } else {
                terminals.push(restore_session(&state, session.snapshot, conversation));
            }
        }
        // Commit the results of this recovery. Failed launches remain available
        // for an explicit retry without another automatic attempt on restart.
        store.expire_closed(&mut state, terminals)?;
        store.save(&state, terminals)?;
        Ok((store, state))
    }

    pub(in crate::host) fn saves_request(request: &Request) -> bool {
        matches!(
            request,
            Request::Panel(_)
                | Request::AddProject(_)
                | Request::AddDiscoveredProjects(_)
                | Request::SetAgentExecutable { .. }
                | Request::SessionDirectories(_)
                | Request::RemoveWorktree { .. }
                | Request::StartSession { .. }
                | Request::ResumeSession { .. }
                | Request::LinkConversation { .. }
                | Request::ForgetClosedSession(_)
                | Request::SetRetentionDays(_)
                | Request::Stop
        )
    }

    pub(in crate::host) fn expire_closed(
        &mut self,
        state: &mut WorkspaceState,
        terminals: &mut [Result<Terminal, Snapshot>],
    ) -> Result<bool> {
        let Some(days) = state.retention_days else {
            return Ok(false);
        };
        let now = SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs();
        let duration = u64::from(days) * 86_400;
        let mut changed = false;
        for (pane, (closed, timestamp)) in self.last_closed.iter().enumerate() {
            if *closed && timestamp.is_some_and(|date| now.saturating_sub(date) >= duration) {
                if !state.terminal_projects[pane].as_os_str().is_empty() {
                    state.forget_closed(terminals, pane)?;
                }
                changed = true;
            }
        }
        Ok(changed)
    }

    pub(in crate::host) fn saw_exit(&self, reply: &Reply) -> bool {
        let Reply::State { panes, .. } = reply else {
            return false;
        };
        panes.iter().enumerate().any(|(index, pane)| {
            pane.as_ref().is_some_and(|pane| pane.exited)
                && self
                    .last_closed
                    .get(index)
                    .is_some_and(|(closed, _)| !closed)
        })
    }

    pub(in crate::host) fn save(
        &mut self,
        state: &WorkspaceState,
        terminals: &mut [Result<Terminal, Snapshot>],
    ) -> Result<()> {
        let now = SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs();
        let sessions = terminals
            .iter_mut()
            .enumerate()
            .map(|(pane, terminal)| {
                let snapshot = match terminal {
                    Ok(terminal) => terminal
                        .snapshot(0)?
                        .context("Falta la captura de una sesión")?,
                    Err(snapshot) => snapshot.clone(),
                };
                let previous_close = self.last_closed.get(pane).and_then(|(_, date)| *date);
                let closed_at = if snapshot.exited {
                    previous_close.or_else(|| snapshot.resume_error.is_none().then_some(now))
                } else {
                    None
                };
                Ok(Session {
                    project: state.terminal_projects[pane].clone(),
                    conversation: state.conversations.get(&pane).map(uuid::Uuid::to_string),
                    snapshot,
                    closed_at,
                })
            })
            .collect::<Result<Vec<_>>>()?;
        let manifest = Manifest {
            version: VERSION,
            layout: state.layout.clone(),
            projects: state.projects.clone(),
            agent_executables: state.agent_executables.clone(),
            sessions,
            retention_days: state.retention_days,
        };
        let directory = self
            .path
            .parent()
            .context("Falta el directorio del espacio")?;
        let mut temporary = tempfile::NamedTempFile::new_in(directory)?;
        {
            let mut output = BufWriter::new(temporary.as_file_mut());
            serde_json::to_writer(&mut output, &manifest)?;
            output.flush()?;
        }
        temporary.as_file().sync_all()?;
        temporary.persist(&self.path)?;
        #[cfg(unix)]
        File::open(directory)?.sync_all()?;
        self.last_closed = manifest
            .sessions
            .into_iter()
            .map(|session| (session.snapshot.exited, session.closed_at))
            .collect();
        Ok(())
    }
}

fn restore_session(
    state: &WorkspaceState,
    mut snapshot: Snapshot,
    conversation: Option<uuid::Uuid>,
) -> Result<Terminal, Snapshot> {
    // A PID from a previous host no longer identifies this terminal.
    snapshot.pid = None;
    if snapshot.exited {
        return Err(snapshot);
    }
    let revision = snapshot.revision + 1;
    let result = (|| {
        let agent = snapshot.agent.context("Falta el agente de la sesión")?;
        let id = conversation.context("Vincula el UUID exacto para recuperar esta conversación")?;
        reattach(state, &snapshot, agent, id, revision)
    })();
    match result {
        Ok(terminal) => Ok(terminal),
        Err(error) => {
            snapshot.revision = revision;
            snapshot.exited = true;
            snapshot.resume_error = Some(format!("{error:#}"));
            Err(snapshot)
        }
    }
}
