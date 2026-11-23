use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

use gpui::{
    Context, Div, FocusHandle, MouseButton, PathPromptOptions, SharedString, div, prelude::*, px,
    rgb,
};

use crate::host::{Agent, Repository, Request, WorktreeRemoval};
use crate::ui::controls::{agent_icon, button, project_name};

use super::{SIDEBAR_WIDTH, Workspace};

use picker::SessionPicker;

mod closed;
mod picker;

#[derive(Clone, Copy)]
enum PathAction {
    AddProject,
    ScanProjects,
    ChooseExecutable(Agent),
}

pub(super) struct Sessions {
    projects: Vec<PathBuf>,
    worktrees: BTreeMap<PathBuf, Vec<PathBuf>>,
    agent_executables: BTreeMap<Agent, PathBuf>,
    discovery: Option<Vec<Repository>>,
    selected_projects: BTreeSet<PathBuf>,
    choosing_agent: Option<PathBuf>,
    session_picker: Option<SessionPicker>,
    worktree_removal: Option<WorktreeRemoval>,
    branch_input: FocusHandle,
    expanded_closed_projects: BTreeSet<PathBuf>,
}

impl Sessions {
    pub(super) fn new(branch_input: FocusHandle) -> Self {
        Self {
            projects: Vec::new(),
            worktrees: BTreeMap::new(),
            agent_executables: BTreeMap::new(),
            discovery: None,
            selected_projects: BTreeSet::new(),
            choosing_agent: None,
            session_picker: None,
            worktree_removal: None,
            branch_input,
            expanded_closed_projects: BTreeSet::new(),
        }
    }

    pub(super) fn has_projects(&self) -> bool {
        !self.projects.is_empty()
    }

    pub(super) fn update_projects(
        &mut self,
        projects: Vec<PathBuf>,
        worktrees: BTreeMap<PathBuf, Vec<PathBuf>>,
        agent_executables: BTreeMap<Agent, PathBuf>,
    ) {
        self.projects = projects;
        self.worktrees = worktrees;
        self.agent_executables = agent_executables;
    }

    pub(super) fn set_worktree_removal(&mut self, preview: Option<WorktreeRemoval>) {
        self.worktree_removal = preview;
    }

    pub(super) fn finish_session(&mut self) {
        self.session_picker = None;
    }

    pub(super) fn set_discovery(&mut self, candidates: Option<Vec<Repository>>) {
        self.discovery = candidates;
        self.selected_projects.clear();
    }
}

impl Workspace {
    pub(super) fn desktop_sidebar(&self, cx: &Context<Self>) -> impl IntoElement {
        div()
            .id("workspace-sidebar")
            .w(px(SIDEBAR_WIDTH))
            .h_full()
            .flex_none()
            .overflow_y_scroll()
            .bg(rgb(0x171d28))
            .border_r_1()
            .border_color(rgb(0x2a3344))
            .p_3()
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .mb_3()
                    .child(
                        div()
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .child("Proyectos"),
                    )
                    .child(button("+ Añadir repositorio").on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|view, _, _, cx| view.choose_path(PathAction::AddProject, cx)),
                    )),
            )
            .child(button("Escanear carpeta…").mb_3().on_mouse_down(
                MouseButton::Left,
                cx.listener(|view, _, _, cx| view.choose_path(PathAction::ScanProjects, cx)),
            ))
            .when_some(self.sessions.discovery.as_ref(), |sidebar, candidates| {
                sidebar.child(self.discovery_panel(candidates, cx))
            })
            .children(self.sessions.projects.iter().map(|project| {
                let row = div()
                    .mb_4()
                    .pb_3()
                    .border_b_1()
                    .border_color(rgb(0x2a3344))
                    .child(
                        div()
                            .truncate()
                            .text_size(px(14.0))
                            .child(project_name(project).to_owned()),
                    )
                    .child(
                        div()
                            .truncate()
                            .mb_2()
                            .text_size(px(11.0))
                            .text_color(rgb(0x929daf))
                            .child(project.display().to_string()),
                    )
                    .children(
                        self.sessions
                            .worktrees
                            .get(project)
                            .into_iter()
                            .flatten()
                            .map(|path| {
                                div()
                                    .pl_2()
                                    .mb_1()
                                    .text_size(px(11.0))
                                    .text_color(rgb(0xaeb8c9))
                                    .child(format!("Worktree · {}", project_name(path)))
                                    .child(div().child(path.display().to_string()))
                                    .child(button("Retirar worktree…").mt_1().on_mouse_down(
                                        MouseButton::Left,
                                        cx.listener({
                                            let project = project.clone();
                                            let path = path.clone();
                                            move |view, _, _, cx| {
                                                view.sessions.worktree_removal = None;
                                                view.send(
                                                    Request::InspectWorktreeRemoval {
                                                        project: project.clone(),
                                                        path: path.clone(),
                                                    },
                                                    cx,
                                                );
                                            }
                                        }),
                                    ))
                                    .when_some(
                                        self.sessions.worktree_removal.as_ref().filter(|preview| {
                                            preview.project == *project && preview.path == *path
                                        }),
                                        |row, preview| {
                                            row.child(self.worktree_removal_panel(preview, cx))
                                        },
                                    )
                            }),
                    )
                    .children(
                        self.terminal_projects
                            .iter()
                            .enumerate()
                            .filter(|(index, owner)| {
                                *owner == project && !self.panes[*index].exited
                            })
                            .map(|(index, _)| {
                                let state = if self.connected {
                                    "Abierta"
                                } else {
                                    "Sin conexión"
                                };
                                let hidden = self.panels_supported
                                    && !self.desktop_layout.root.contains(index)
                                    && !self.panes[index].closed_by_user;
                                self.session_button(
                                    index,
                                    format!(
                                        "{} {} · {state}",
                                        self.session_agent(index).name(),
                                        index + 1
                                    ),
                                    cx,
                                )
                                .when(hidden, |row| {
                                    row.child(
                                        div()
                                            .flex_none()
                                            .ml_auto()
                                            .text_size(px(10.0))
                                            .text_color(rgb(0xb9cbe4))
                                            .child("Oculta"),
                                    )
                                })
                                .mb_1()
                            }),
                    )
                    .child(self.closed_sessions(project, cx));
                self.session_picker_controls(row, project, cx)
            }))
            .child(
                div()
                    .text_size(px(12.0))
                    .text_color(rgb(0xaeb8c9))
                    .child("Ejecutables de agentes")
                    .children(Agent::ALL.map(|agent| {
                        let custom = self.sessions.agent_executables.get(&agent);
                        div()
                            .mt_3()
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap(px(6.0))
                                    .child(agent_icon(agent))
                                    .child(agent.name()),
                            )
                            .child(div().truncate().mb_1().child(custom.map_or_else(
                                || "Detección automática".into(),
                                |path| path.display().to_string(),
                            )))
                            .child(button("Elegir ejecutable").on_mouse_down(
                                MouseButton::Left,
                                cx.listener(move |view, _, _, cx| {
                                    view.choose_path(PathAction::ChooseExecutable(agent), cx)
                                }),
                            ))
                            .when(custom.is_some(), |row| {
                                row.child(button("Usar detección automática").mt_1().on_mouse_down(
                                    MouseButton::Left,
                                    cx.listener(move |view, _, _, cx| {
                                        view.send(
                                            Request::SetAgentExecutable { agent, path: None },
                                            cx,
                                        )
                                    }),
                                ))
                            })
                    })),
            )
    }

    fn worktree_removal_panel(&self, preview: &WorktreeRemoval, cx: &Context<Self>) -> Div {
        let panel = div().mt_2().flex().flex_col().gap_1().child(
            button("Cancelar retirada").on_mouse_down(
                MouseButton::Left,
                cx.listener(|view, _, _, cx| {
                    view.sessions.worktree_removal = None;
                    cx.notify();
                }),
            ),
        );
        if !preview.active_sessions.is_empty() {
            return panel
                .child("Abre y cierra estas sesiones antes de retirar el worktree:")
                .children(preview.active_sessions.iter().map(|&index| {
                    self.session_button(index, format!("Mostrar sesión {}", index + 1), cx)
                }))
                .child(button("Volver a comprobar").on_mouse_down(
                    MouseButton::Left,
                    cx.listener({
                        let project = preview.project.clone();
                        let path = preview.path.clone();
                        move |view, _, _, cx| {
                            view.send(
                                Request::InspectWorktreeRemoval {
                                    project: project.clone(),
                                    path: path.clone(),
                                },
                                cx,
                            );
                        }
                    }),
                ));
        }

        let force = !preview.changes.is_empty();
        panel
            .when(force, |panel| {
                panel
                    .child("La retirada forzada puede descartar estos cambios y archivos:")
                    .children(
                        preview
                            .changes
                            .iter()
                            .map(|change| div().child(change.clone())),
                    )
            })
            .when_some(preview.branch.as_ref(), |panel, branch| {
                panel
                    .child(format!("Rama local: {branch}"))
                    .child("Git conservará la rama si tiene commits pendientes de integrar.")
            })
            .children(
                [false, true]
                    .into_iter()
                    .filter(|delete| !delete || preview.branch.is_some())
                    .map(|delete_branch| {
                        let label = match (force, preview.branch.is_some(), delete_branch) {
                            (false, false, _) => "Confirmar retirada",
                            (true, false, _) => "Confirmar retirada forzada",
                            (false, true, false) => "Retirar y conservar rama",
                            (true, true, false) => "Forzar retirada y conservar rama",
                            (false, true, true) => "Retirar e intentar borrar rama",
                            (true, true, true) => "Forzar retirada e intentar borrar rama",
                        };
                        button(label).on_mouse_down(
                            MouseButton::Left,
                            cx.listener({
                                let project = preview.project.clone();
                                let path = preview.path.clone();
                                move |view, _, _, cx| {
                                    view.sessions.worktree_removal = None;
                                    view.send(
                                        Request::RemoveWorktree {
                                            project: project.clone(),
                                            path: path.clone(),
                                            force,
                                            delete_branch,
                                        },
                                        cx,
                                    );
                                }
                            }),
                        )
                    }),
            )
    }

    fn discovery_panel(&self, candidates: &[Repository], cx: &Context<Self>) -> Div {
        div()
            .mb_3()
            .pb_3()
            .border_b_1()
            .border_color(rgb(0x2a3344))
            .child(if candidates.is_empty() {
                "No hay repositorios nuevos."
            } else {
                "Selecciona los repositorios que quieras añadir."
            })
            .children(candidates.iter().map(|candidate| {
                let root = candidate.root.clone();
                let selected = self.sessions.selected_projects.contains(&root);
                button(format!(
                    "{} {}",
                    if selected { "☑" } else { "☐" },
                    project_name(&root)
                ))
                .mt_1()
                .child(
                    div()
                        .id(SharedString::from(root.display().to_string()))
                        .overflow_x_scroll()
                        .whitespace_nowrap()
                        .text_size(px(11.0))
                        .child(root.display().to_string()),
                )
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |view, _, _, cx| {
                        if !view.sessions.selected_projects.remove(&root) {
                            view.sessions.selected_projects.insert(root.clone());
                        }
                        cx.notify();
                    }),
                )
            }))
            .child(
                div()
                    .flex()
                    .gap_2()
                    .mt_2()
                    .when(!self.sessions.selected_projects.is_empty(), |row| {
                        row.child(button("Añadir seleccionados").on_mouse_down(
                            MouseButton::Left,
                            cx.listener(|view, _, _, cx| {
                                view.send(
                                    Request::AddDiscoveredProjects(
                                        view.sessions.selected_projects.iter().cloned().collect(),
                                    ),
                                    cx,
                                );
                            }),
                        ))
                    })
                    .child(button("Cerrar").on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|view, _, _, cx| {
                            view.sessions.set_discovery(None);
                            cx.notify();
                        }),
                    )),
            )
    }

    fn choose_path(&self, action: PathAction, cx: &mut Context<Self>) {
        let executable = matches!(action, PathAction::ChooseExecutable(_));
        let selected = cx.prompt_for_paths(PathPromptOptions {
            files: executable,
            directories: !executable,
            multiple: false,
            prompt: Some(
                match action {
                    PathAction::AddProject => "Añadir repositorio".into(),
                    PathAction::ScanProjects => "Escanear carpeta".into(),
                    PathAction::ChooseExecutable(agent) => {
                        format!("Elegir ejecutable de {}", agent.name())
                    }
                }
                .into(),
            ),
        });
        cx.spawn(async move |this, cx| match selected.await {
            Ok(Err(error)) => {
                _ = this.update(cx, |view, cx| {
                    view.status = error.to_string().into();
                    cx.notify();
                });
            }
            Ok(Ok(Some(paths))) => {
                if let Some(path) = paths.into_iter().next() {
                    _ = this.update(cx, |view, cx| {
                        let request = match action {
                            PathAction::AddProject => Request::AddProject(path),
                            PathAction::ChooseExecutable(agent) => Request::SetAgentExecutable {
                                agent,
                                path: Some(path),
                            },
                            PathAction::ScanProjects => {
                                view.sessions.set_discovery(None);
                                Request::ScanProjects(path)
                            }
                        };
                        view.send(request, cx);
                        if matches!(action, PathAction::ScanProjects)
                            && view.status.as_ref() == "Host conectado"
                        {
                            view.status = "Escaneando carpeta…".into();
                        }
                    });
                }
            }
            _ => {}
        })
        .detach();
    }
}
