use std::path::{Path, PathBuf};

use gpui::{Context, Div, MouseButton, div, prelude::*, px, rgb};

use crate::host::{Agent, Request};
use crate::ui::controls::button;

use super::Workspace;

impl Workspace {
    pub(super) fn closed_sessions(&self, project: &Path, cx: &Context<Self>) -> Div {
        let panes: Vec<_> = self
            .terminal_projects
            .iter()
            .enumerate()
            .filter(|(pane, owner)| owner.as_path() == project && self.panes[*pane].exited)
            .map(|(pane, _)| pane)
            .collect();
        if panes.is_empty() {
            return div();
        }

        let expanded = self.sessions.expanded_closed_projects.contains(project);
        let project = PathBuf::from(project);
        div()
            .flex()
            .flex_col()
            .gap_1()
            .child(
                button(format!(
                    "{} Cerradas ({})",
                    if expanded { "▾" } else { "▸" },
                    panes.len()
                ))
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |view, _, _, cx| {
                        let projects = &mut view.sessions.expanded_closed_projects;
                        if !projects.remove(&project) {
                            projects.insert(project.clone());
                        }
                        cx.notify();
                    }),
                ),
            )
            .when(expanded, |section| {
                section.children(panes.into_iter().map(|pane| self.closed_session(pane, cx)))
            })
    }

    fn closed_session(&self, pane: usize, cx: &Context<Self>) -> Div {
        let snapshot = &self.panes[pane];
        let agent = self.session_agent(pane);
        div()
            .flex()
            .flex_col()
            .gap_1()
            .pl_2()
            .child(self.session_button(
                pane,
                format!("{} {} · Ver salida", agent.name(), pane + 1),
                cx,
            ))
            .when_some(snapshot.resume_error.as_ref(), |row, error| {
                row.child(
                    div()
                        .whitespace_normal()
                        .text_size(px(11.0))
                        .text_color(rgb(0xf0b687))
                        .child(error.clone()),
                )
            })
            .when(self.connected && snapshot.can_resume, |row| {
                row.child(
                    button(if snapshot.resume_error.is_some() {
                        "Reintentar reanudación"
                    } else {
                        "Reanudar"
                    })
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |view, _, _, cx| {
                            view.send(
                                Request::ResumeSession {
                                    pane,
                                    conversation: None,
                                },
                                cx,
                            );
                        }),
                    ),
                )
            })
            .when(self.connected && agent == Agent::Codex, |row| {
                row.child(button("Pegar UUID y reanudar").on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |view, _, _, cx| {
                        if let Some(conversation) =
                            cx.read_from_clipboard().and_then(|item| item.text())
                        {
                            view.send(
                                Request::ResumeSession {
                                    pane,
                                    conversation: Some(conversation),
                                },
                                cx,
                            );
                        } else {
                            view.status = "Copia el UUID de la conversación de Codex.".into();
                            cx.notify();
                        }
                    }),
                ))
            })
    }
}
