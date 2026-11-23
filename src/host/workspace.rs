use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, ensure};

use crate::panels::{DesktopLayout, PanelAction};

use super::git::{
    Repository, WorktreeRemoval, checkouts, create_worktree, delete_integrated_branch,
    inspect_worktree_removal, occupied_checkout, remove_worktree, repository, scan_projects,
    session_directories, validate_branch_name,
};
use super::protocol::{Agent, Layout, Reply, Request, SessionTarget, Snapshot};
use super::terminal::{Terminal, detect_agent, is_executable};

#[derive(Default)]
pub(super) struct WorkspaceState {
    layout: DesktopLayout,
    projects: Vec<Repository>,
    agent_executables: BTreeMap<Agent, PathBuf>,
    terminal_projects: Vec<PathBuf>,
    worktree_removal: Option<WorktreeRemoval>,
}

impl WorkspaceState {
    fn add(&mut self, project: Repository) {
        if let Some(existing) = self
            .projects
            .iter_mut()
            .find(|item| item.root == project.root)
        {
            *existing = project;
        } else {
            self.projects.push(project);
        }
    }
}

pub(super) fn dispatch(
    terminals: &mut Vec<Result<Terminal, Snapshot>>,
    layout: &mut Layout,
    project_dir: &str,
    mut workspace: Option<&mut WorkspaceState>,
    request: Request,
) -> Result<Reply> {
    match request {
        Request::Poll(seen) => {
            let mut panes = Vec::with_capacity(terminals.len());
            for (pane, terminal) in terminals.iter_mut().enumerate() {
                let revision = seen.get(pane).copied().unwrap_or_default();
                panes.push(match terminal {
                    Ok(terminal) => terminal.snapshot(revision)?,
                    Err(snapshot) => (snapshot.revision > revision).then(|| snapshot.clone()),
                });
            }
            return Ok(Reply::State {
                layout: *layout,
                desktop_layout: workspace.as_ref().map(|state| state.layout.clone()),
                project_dir: project_dir.into(),
                panes,
                projects: workspace.as_ref().map_or_else(Vec::new, |state| {
                    state
                        .projects
                        .iter()
                        .map(|project| project.root.clone())
                        .collect()
                }),
                worktrees: workspace.as_ref().map_or_else(BTreeMap::new, |state| {
                    state
                        .projects
                        .iter()
                        .map(|project| (project.root.clone(), project.worktrees.clone()))
                        .collect()
                }),
                agent_executables: workspace
                    .as_ref()
                    .map_or_else(BTreeMap::new, |state| state.agent_executables.clone()),
                terminal_projects: workspace
                    .as_ref()
                    .map_or_else(Vec::new, |state| state.terminal_projects.clone()),
            });
        }
        Request::Input { pane, bytes } => terminals
            .get_mut(pane)
            .context("unknown terminal")?
            .as_mut()
            .map_err(|snapshot| anyhow::anyhow!("{} has exited", snapshot.label))?
            .input(&bytes)?,
        Request::Resize { pane, rows, cols } => {
            if let Ok(terminal) = terminals.get_mut(pane).context("unknown terminal")? {
                terminal.resize(rows, cols)?;
            }
        }
        Request::SetLayout(position) => {
            *layout = Layout {
                left: position.left.clamp(0.15, 0.85),
                top: position.top.clamp(0.15, 0.85),
            };
        }
        Request::Panel(action) => {
            let state = workspace.context("workspace action requires the desktop host")?;
            match action {
                PanelAction::Split { pane, axis } => state.layout.split(pane, axis)?,
                PanelAction::Resize { path, ratio } => state.layout.resize(&path, ratio)?,
                PanelAction::Show { pane } => {
                    ensure!(pane < terminals.len(), "Sesión desconocida");
                    state.layout.show(pane)?;
                }
                PanelAction::Hide { pane } => state.layout.hide(pane)?,
                PanelAction::Dismiss { path } => state.layout.dismiss(&path)?,
                PanelAction::Close { pane } => {
                    ensure!(
                        state.layout.root.contains(pane),
                        "El panel ya no está visible"
                    );
                    if let Ok(terminal) = &mut terminals[pane] {
                        terminals[pane] = Err(terminal.close()?);
                    }
                    state.layout.hide(pane)?;
                }
            }
        }
        Request::DesktopReady => ensure!(workspace.is_some(), "expected the desktop host"),
        Request::AddProject(path) => {
            let state = workspace
                .as_mut()
                .context("workspace action requires the desktop host")?;
            state.add(repository(&path)?);
        }
        Request::ScanProjects(path) => {
            let state = workspace.context("workspace action requires the desktop host")?;
            return Ok(Reply::Discovered(scan_projects(&path, &state.projects)?));
        }
        Request::AddDiscoveredProjects(paths) => {
            let state = workspace.context("workspace action requires the desktop host")?;
            // Resolve all selections before updating the workspace.
            let projects = paths
                .iter()
                .map(|path| repository(path))
                .collect::<Result<Vec<_>>>()?;
            for project in projects {
                state.add(project);
            }
        }
        Request::SetAgentExecutable { agent, path } => {
            let state = workspace
                .as_mut()
                .context("workspace action requires the desktop host")?;
            if let Some(path) = path {
                ensure!(
                    path.is_absolute() && is_executable(&path),
                    "Selecciona un ejecutable de {}",
                    agent.name()
                );
                state.agent_executables.insert(agent, path);
            } else {
                state.agent_executables.remove(&agent);
            }
        }
        Request::SessionDirectories(project) => {
            let state = workspace.context("workspace action requires the desktop host")?;
            let added = state
                .projects
                .iter_mut()
                .find(|item| item.root == project)
                .context("Añade el repositorio primero")?;
            let directories = session_directories(&project)?;
            added.worktrees.clone_from(&directories.worktrees);
            return Ok(Reply::Directories(directories));
        }
        Request::InspectWorktreeRemoval { project, path } => {
            let state = workspace.context("workspace action requires the desktop host")?;
            state.worktree_removal = None;
            ensure!(
                state.projects.iter().any(|item| item.root == project),
                "Añade el repositorio primero"
            );
            let preview = inspect_worktree_removal(&project, &path, |directory| {
                active_sessions(terminals, directory)
            })?;
            state.worktree_removal = Some(preview.clone());
            return Ok(Reply::WorktreeRemoval(preview));
        }
        Request::RemoveWorktree {
            project,
            path,
            force,
            delete_branch,
        } => {
            let state = workspace.context("workspace action requires the desktop host")?;
            let reviewed = state
                .worktree_removal
                .take()
                .context("Revisa el worktree antes de retirarlo")?;
            ensure!(
                reviewed.project == project && reviewed.path == path,
                "Revisa este worktree antes de retirarlo"
            );
            let current = inspect_worktree_removal(&project, &path, |directory| {
                active_sessions(terminals, directory)
            })?;
            if current != reviewed
                || !current.active_sessions.is_empty()
                || force == current.changes.is_empty()
            {
                state.worktree_removal = Some(current.clone());
                return Ok(Reply::WorktreeRemoval(current));
            }

            remove_worktree(&current, force)?;
            if let Some(added) = state.projects.iter_mut().find(|item| item.root == project) {
                added.worktrees.retain(|checkout| *checkout != path);
            }
            let retained_branch = current
                .branch
                .as_ref()
                .filter(|branch| {
                    delete_branch && delete_integrated_branch(&current, branch).is_err()
                })
                .cloned();
            return Ok(Reply::WorktreeRemoved { retained_branch });
        }
        Request::StartSession {
            project,
            agent,
            target,
        } => {
            let state = workspace
                .as_mut()
                .context("workspace action requires the desktop host")?;
            ensure!(
                state.projects.iter().any(|item| item.root == project),
                "Añade el repositorio antes de iniciar {}",
                agent.name()
            );
            let program = state
                .agent_executables
                .get(&agent)
                .cloned()
                .or_else(|| detect_agent(agent))
                .with_context(|| {
                    format!(
                        "No se encontró {}. Elige su ejecutable desde la barra lateral.",
                        agent.name()
                    )
                })?;
            let directory = match &target {
                SessionTarget::Main => project.clone(),
                SessionTarget::Existing(path) => {
                    let path = path.canonicalize()?;
                    ensure!(
                        path == project || repository(&project)?.worktrees.contains(&path),
                        "El directorio no pertenece a los worktrees del proyecto"
                    );
                    path
                }
                SessionTarget::Branch(name) | SessionTarget::NewBranch { name, base: _ } => {
                    let base = match &target {
                        SessionTarget::NewBranch { base, .. } => Some(base.as_str()),
                        _ => None,
                    };
                    let directories = session_directories(&project)?;
                    let branch = directories
                        .branches
                        .iter()
                        .find(|branch| branch.name == *name);
                    if let Some(path) = branch.and_then(|branch| branch.occupied.clone()) {
                        return Ok(Reply::BranchOccupied(path));
                    }
                    if let Some(base) = base {
                        ensure!(branch.is_none(), "La rama {name} ya existe");
                        ensure!(
                            directories
                                .branches
                                .iter()
                                .any(|branch| branch.name == base),
                            "La rama base {base} ya no existe"
                        );
                        validate_branch_name(&project, name)?;
                    } else {
                        ensure!(branch.is_some(), "La rama {name} ya no existe");
                    }
                    let created = match create_worktree(&project, name, base) {
                        Ok(path) => path,
                        Err(error) => {
                            if let Ok(current) = checkouts(&project)
                                && let Some(path) = occupied_checkout(&current, name)
                            {
                                return Ok(Reply::BranchOccupied(path));
                            }
                            return Err(error);
                        }
                    };
                    state.add(repository(&project)?);
                    created
                }
            };
            let mut terminal =
                Terminal::new(program.as_os_str(), true, &directory).with_context(|| {
                    format!(
                        "No se pudo iniciar {} desde {}",
                        agent.name(),
                        program.display()
                    )
                })?;
            terminal.set_agent(agent);
            state.layout.assign(terminals.len());
            terminals.push(Ok(terminal));
            state.terminal_projects.push(project);
        }
        Request::Stop => {}
    }
    Ok(Reply::Ok)
}

fn active_sessions(
    terminals: &mut [Result<Terminal, Snapshot>],
    directory: &Path,
) -> Result<Vec<usize>> {
    let mut active = Vec::new();
    for (index, terminal) in terminals.iter_mut().enumerate() {
        if let Ok(terminal) = terminal
            && terminal.is_running_in(directory)?
        {
            active.push(index);
        }
    }
    Ok(active)
}
