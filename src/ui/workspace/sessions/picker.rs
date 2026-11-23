use std::path::PathBuf;

use gpui::{
    Context, CursorStyle, Div, KeyDownEvent, MouseButton, SharedString, Window, div, prelude::*,
    px, rgb,
};

use crate::host::{Agent, Request, SessionDirectories, SessionTarget};
use crate::ui::controls::{button, icon_button, project_name};

use super::{Sessions, Workspace};

pub(super) struct SessionPicker {
    project: PathBuf,
    agent: Agent,
    directories: Option<SessionDirectories>,
    pending: bool,
    occupied: Option<PathBuf>,
    creating: bool,
    base: Option<String>,
    name: String,
}

impl SessionPicker {
    fn new_branch(&self) -> Option<SessionTarget> {
        let name = self.name.trim();
        if name.is_empty() {
            return None;
        }
        Some(SessionTarget::NewBranch {
            name: name.to_owned(),
            base: self.base.clone()?,
        })
    }
}

impl Sessions {
    pub(in crate::ui::workspace) fn set_directories(&mut self, directories: SessionDirectories) {
        if let Some(picker) = &mut self.session_picker
            && picker.project == directories.project
        {
            picker.directories = Some(directories);
            picker.pending = false;
        }
    }

    pub(in crate::ui::workspace) fn set_occupied(&mut self, path: PathBuf) {
        if let Some(picker) = &mut self.session_picker {
            picker.occupied = Some(path);
            picker.pending = false;
        }
    }

    pub(in crate::ui::workspace) fn allow_session_retry(&mut self) {
        if let Some(picker) = &mut self.session_picker {
            picker.pending = false;
        }
    }
}

impl Workspace {
    pub(super) fn session_picker_controls(
        &self,
        row: Div,
        project: &PathBuf,
        cx: &Context<Self>,
    ) -> Div {
        let root = project.clone();
        row.child(
            button(
                if self.sessions.choosing_agent.as_ref() == Some(project)
                    || self
                        .sessions
                        .session_picker
                        .as_ref()
                        .is_some_and(|picker| picker.project == *project)
                {
                    "Cancelar"
                } else {
                    "+ Nueva sesión"
                },
            )
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |view, _, window, cx| {
                    if view
                        .sessions
                        .session_picker
                        .as_ref()
                        .is_some_and(|picker| picker.pending)
                    {
                        return;
                    }
                    let already_open = view.sessions.choosing_agent.as_ref() == Some(&root)
                        || view
                            .sessions
                            .session_picker
                            .as_ref()
                            .is_some_and(|picker| picker.project == root);
                    view.sessions.session_picker = None;
                    view.focus.focus(window);
                    view.sessions.choosing_agent = (!already_open).then(|| root.clone());
                    cx.notify();
                }),
            ),
        )
        .when(
            self.sessions.choosing_agent.as_ref() == Some(project),
            |row| {
                row.children(Agent::ALL.map(|agent| {
                    let project = project.clone();
                    icon_button(agent, agent.name()).mt_1().on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |view, _, _, cx| {
                            view.sessions.choosing_agent = None;
                            view.sessions.session_picker = Some(SessionPicker {
                                project: project.clone(),
                                agent,
                                directories: None,
                                pending: true,
                                occupied: None,
                                creating: false,
                                base: None,
                                name: String::new(),
                            });
                            view.send(Request::SessionDirectories(project.clone()), cx);
                        }),
                    )
                }))
            },
        )
        .when_some(
            self.sessions
                .session_picker
                .as_ref()
                .filter(|picker| picker.project == *project),
            |row, picker| row.child(self.directory_picker(picker, cx)),
        )
    }

    fn directory_picker(&self, picker: &SessionPicker, cx: &Context<Self>) -> Div {
        let panel = div().mt_2().flex().flex_col().gap_1().text_size(px(12.0));
        if picker.pending {
            return panel.child("Consultando el repositorio…");
        }
        let Some(directories) = &picker.directories else {
            return panel.child(button("Reintentar").on_mouse_down(
                MouseButton::Left,
                cx.listener(|view, _, _, cx| {
                    if let Some(picker) = &mut view.sessions.session_picker {
                        picker.pending = true;
                        let project = picker.project.clone();
                        view.send(Request::SessionDirectories(project), cx);
                    }
                }),
            ));
        };
        if let Some(path) = &picker.occupied {
            return panel
                .child("Rama ocupada en")
                .child(path.display().to_string())
                .when(path.is_dir(), |panel| {
                    panel.child(self.directory_button(
                        "Abrir sesión aquí",
                        SessionTarget::Existing(path.clone()),
                        cx,
                    ))
                })
                .child(self.new_branch_button(cx))
                .child(button("Elegir otra rama").on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|view, _, _, cx| {
                        if let Some(picker) = &mut view.sessions.session_picker {
                            picker.occupied = None;
                            picker.creating = false;
                            cx.notify();
                        }
                    }),
                ));
        }
        if picker.creating {
            let name = if picker.name.is_empty() {
                "Nombre de la rama"
            } else {
                &picker.name
            };
            return panel
                .child(
                    div()
                        .track_focus(&self.sessions.branch_input)
                        .px_2()
                        .py_1()
                        .border_1()
                        .border_color(rgb(0x536987))
                        .focus(|style| style.border_color(rgb(0x8ab4ed)))
                        .rounded_md()
                        .cursor(CursorStyle::IBeam)
                        .child(name.to_owned())
                        .on_key_down(cx.listener(Self::edit_branch_name))
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(|view, _, window, cx| {
                                view.sessions.branch_input.focus(window);
                                cx.notify();
                            }),
                        ),
                )
                .child("Rama base")
                .children(directories.branches.iter().map(|branch| {
                    let name = branch.name.clone();
                    let selected = picker.base.as_ref() == Some(&name);
                    button(format!("{}{}", if selected { "✓ " } else { "" }, name)).on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |view, _, _, cx| {
                            if let Some(picker) = &mut view.sessions.session_picker {
                                picker.base = Some(name.clone());
                                cx.notify();
                            }
                        }),
                    )
                }))
                .when_some(picker.new_branch(), |panel, target| {
                    panel.child(self.directory_button("Crear y abrir sesión", target, cx))
                })
                .child(button("Volver a los directorios").on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|view, _, window, cx| {
                        if let Some(picker) = &mut view.sessions.session_picker {
                            picker.creating = false;
                            view.focus.focus(window);
                            cx.notify();
                        }
                    }),
                ));
        }
        panel
            .child(format!("Directorio de la sesión · {}", picker.agent.name()))
            .child(self.directory_button("Directorio principal", SessionTarget::Main, cx))
            .children(directories.worktrees.iter().map(|path| {
                self.directory_button(
                    format!("Worktree · {}", project_name(path)),
                    SessionTarget::Existing(path.clone()),
                    cx,
                )
                .child(
                    div()
                        .whitespace_normal()
                        .text_size(px(11.0))
                        .child(path.display().to_string()),
                )
            }))
            .when(!directories.branches.is_empty(), |panel| {
                panel
                    .child(self.new_branch_button(cx))
                    .child("Crear worktree con una rama local")
                    .children(directories.branches.iter().map(|branch| {
                        self.directory_button(
                            if branch.occupied.is_some() {
                                format!("{} · Ocupada", branch.name)
                            } else {
                                branch.name.clone()
                            },
                            SessionTarget::Branch(branch.name.clone()),
                            cx,
                        )
                    }))
            })
    }

    fn directory_button(
        &self,
        label: impl Into<SharedString>,
        target: SessionTarget,
        cx: &Context<Self>,
    ) -> Div {
        button(label).whitespace_normal().on_mouse_down(
            MouseButton::Left,
            cx.listener(move |view, _, window, cx| {
                view.focus.focus(window);
                view.start_session(target.clone(), cx);
            }),
        )
    }

    fn new_branch_button(&self, cx: &Context<Self>) -> Div {
        button("Crear rama nueva").on_mouse_down(
            MouseButton::Left,
            cx.listener(|view, _, window, cx| {
                if let Some(picker) = &mut view.sessions.session_picker {
                    picker.occupied = None;
                    picker.creating = true;
                    view.sessions.branch_input.focus(window);
                    cx.notify();
                }
            }),
        )
    }

    fn start_session(&mut self, target: SessionTarget, cx: &mut Context<Self>) {
        if let Some(picker) = &mut self.sessions.session_picker
            && !picker.pending
        {
            picker.pending = true;
            let request = Request::StartSession {
                project: picker.project.clone(),
                agent: picker.agent,
                target,
            };
            self.send(request, cx);
        }
    }

    fn edit_branch_name(
        &mut self,
        event: &KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(picker) = &mut self.sessions.session_picker {
            let key = &event.keystroke;
            match key.key.as_str() {
                "backspace" => {
                    picker.name.pop();
                }
                "enter" => {
                    if let Some(target) = picker.new_branch() {
                        self.focus.focus(window);
                        self.start_session(target, cx);
                    }
                }
                "escape" => self.focus.focus(window),
                "v" if key.modifiers.platform || key.modifiers.control => {
                    if let Some(text) = cx.read_from_clipboard().and_then(|item| item.text()) {
                        picker.name.push_str(text.trim());
                    }
                }
                _ if !key.modifiers.platform && !key.modifiers.control => {
                    if let Some(text) = &key.key_char {
                        picker.name.push_str(text);
                    }
                }
                _ => {}
            }
        }
        window.prevent_default();
        cx.stop_propagation();
        cx.notify();
    }
}
