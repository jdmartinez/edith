use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender};
use std::time::{Duration, Instant};

use anyhow::Result;

use crate::host::{Endpoint, Reply, Request};

pub(super) const INTERVAL: Duration = Duration::from_millis(33);

pub(super) fn poll(
    endpoint: Endpoint,
    requests: Receiver<Request>,
    updates: Sender<Result<Reply>>,
) {
    let mut revisions = Vec::new();
    let mut workspace = Default::default();
    let mut next_poll = Instant::now();
    let mut initial = true;
    let mut disconnected = false;
    loop {
        if Instant::now() >= next_poll {
            let result = endpoint.request(Request::Poll(revisions.clone()));
            next_poll = Instant::now() + INTERVAL;
            match result {
                Ok(Reply::State {
                    layout,
                    desktop_layout,
                    project_dir,
                    panes,
                    projects,
                    worktrees,
                    agent_executables,
                    terminal_projects,
                    retention_days,
                }) => {
                    let metadata = (
                        projects,
                        worktrees,
                        agent_executables,
                        terminal_projects,
                        desktop_layout,
                        retention_days,
                    );
                    let changed = initial
                        || disconnected
                        || workspace != metadata
                        || panes.iter().any(Option::is_some);
                    workspace = metadata;
                    revisions.resize(panes.len(), 0);
                    initial = false;
                    disconnected = false;
                    for (index, pane) in panes.iter().enumerate() {
                        if let Some(snapshot) = pane {
                            revisions[index] = snapshot.revision;
                        }
                    }
                    if changed
                        && updates
                            .send(Ok(Reply::State {
                                layout,
                                desktop_layout: workspace.4.clone(),
                                project_dir,
                                panes,
                                projects: workspace.0.clone(),
                                worktrees: workspace.1.clone(),
                                agent_executables: workspace.2.clone(),
                                terminal_projects: workspace.3.clone(),
                                retention_days: workspace.5,
                            }))
                            .is_err()
                    {
                        return;
                    }
                }
                Ok(_) => {}
                Err(error) if !disconnected => {
                    disconnected = true;
                    revisions.clear();
                    if updates.send(Err(error)).is_err() {
                        return;
                    }
                }
                Err(_) => {}
            }
        }

        match requests.recv_timeout(next_poll.saturating_duration_since(Instant::now())) {
            Ok(request) => {
                let adding = matches!(request, Request::AddDiscoveredProjects(_));
                let starting = matches!(request, Request::StartSession { .. });
                let update = match endpoint.request(request) {
                    Ok(
                        reply @ (Reply::Discovered(_)
                        | Reply::Directories(_)
                        | Reply::BranchOccupied(_)
                        | Reply::WorktreeRemoval(_)
                        | Reply::WorktreeRemoved { .. }),
                    ) => reply,
                    Ok(Reply::Ok) if starting => Reply::SessionStarted,
                    Ok(Reply::Ok) if adding => Reply::Ok,
                    Err(error) => Reply::Error(error.to_string()),
                    _ => continue,
                };
                if updates.send(Ok(update)).is_err() {
                    return;
                }
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => return,
        }
    }
}
