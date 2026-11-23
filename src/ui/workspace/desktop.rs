use gpui::{Context, Div, MouseButton, MouseMoveEvent, Window, div, prelude::*, px, rgb};

use crate::host::Request;
use crate::panels::{Axis, Panel, PanelAction};
use crate::ui::controls::{button, project_name, separator_style};

use super::{DIVIDER, HEADER_HEIGHT, SIDEBAR_WIDTH, Workspace};

const STATUS_HEIGHT: f32 = 25.0;

pub(super) struct PanelDrag {
    path: Vec<bool>,
    axis: Axis,
    origin: f32,
    initial_ratio: f32,
    ratio: f32,
    available: f32,
}

impl PanelDrag {
    fn ratio_at(&self, position: gpui::Point<gpui::Pixels>) -> f32 {
        (self.initial_ratio
            + (axis_position(self.axis, position) - self.origin) / self.available.max(1.0))
        .clamp(0.15, 0.85)
    }
}

fn axis_position(axis: Axis, position: gpui::Point<gpui::Pixels>) -> f32 {
    f32::from(match axis {
        Axis::Columns => position.x,
        Axis::Rows => position.y,
    })
}

impl Workspace {
    pub(super) fn desktop(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Div {
        let bounds = window.viewport_size();
        let sidebar_width = if self.sidebar_visible {
            SIDEBAR_WIDTH
        } else {
            0.0
        };
        let width = (f32::from(bounds.width) - sidebar_width).max(0.0);
        let show_status = self.status.as_ref() != "Host conectado";
        let status_height = if show_status { STATUS_HEIGHT } else { 0.0 };
        let height = (f32::from(bounds.height) - HEADER_HEIGHT - status_height).max(0.0);
        let mut terminal = div().flex().flex_col().w(px(width)).min_w_0();
        let root = self.desktop_layout.root.clone();
        terminal = terminal.child(self.desktop_panel(&root, width, height, &mut Vec::new(), cx));

        div()
            .size_full()
            .flex()
            .flex_col()
            .text_color(rgb(0xd9e0ee))
            .bg(rgb(0x11151d))
            .track_focus(&self.focus)
            .on_key_down(cx.listener(Self::key_down))
            .on_mouse_move(cx.listener(Self::panel_drag_move))
            .on_mouse_up(MouseButton::Left, cx.listener(Self::panel_drag_end))
            .on_mouse_up_out(MouseButton::Left, cx.listener(Self::panel_drag_end))
            .child(
                div()
                    .flex()
                    .flex_none()
                    .items_center()
                    .h(px(HEADER_HEIGHT))
                    .bg(rgb(0x171d28))
                    .border_b_1()
                    .border_color(rgb(0x2a3344))
                    .child(
                        div().px_2().flex_none().child(
                            button(if self.sidebar_visible {
                                "Ocultar barra lateral"
                            } else {
                                "Mostrar barra lateral"
                            })
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(|view, _, window, cx| {
                                    view.sidebar_visible = !view.sidebar_visible;
                                    view.focus.focus(window);
                                    cx.notify();
                                }),
                            ),
                        ),
                    )
                    .child(
                        div()
                            .px_2()
                            .flex_none()
                            .child(button("Estadísticas").on_mouse_down(
                                MouseButton::Left,
                                cx.listener(|view, _, window, cx| {
                                    view.usage.open = true;
                                    view.usage.scan();
                                    view.focus.focus(window);
                                    cx.notify();
                                }),
                            )),
                    )
                    .child(
                        div()
                            .id("terminal-tabs")
                            .flex()
                            .flex_1()
                            .min_w_0()
                            .items_center()
                            .gap(px(4.0))
                            .overflow_x_scroll()
                            .children(
                                self.terminal_projects
                                    .iter()
                                    .enumerate()
                                    .filter(|(_, project)| !project.as_os_str().is_empty())
                                    .map(|(index, project)| {
                                        self.session_button(
                                            index,
                                            format!(
                                                "{} · {} {}",
                                                project_name(project),
                                                self.session_agent(index).name(),
                                                index + 1
                                            ),
                                            cx,
                                        )
                                        .flex_none()
                                    }),
                            ),
                    ),
            )
            .when(show_status, |root| {
                root.child(
                    div()
                        .h(px(STATUS_HEIGHT))
                        .flex_none()
                        .truncate()
                        .px_3()
                        .py_1()
                        .bg(rgb(0x34283a))
                        .text_size(px(12.0))
                        .child(self.status.clone()),
                )
            })
            .child(
                div()
                    .flex()
                    .flex_1()
                    .min_h_0()
                    .when(self.sidebar_visible, |row| {
                        row.child(self.desktop_sidebar(cx))
                    })
                    .child(terminal),
            )
            .when_some(self.panel_menu, |root, (pane, position)| {
                root.child(
                    div().absolute().child(gpui::deferred(
                        gpui::anchored()
                            .position(position + gpui::point(px(18.0), px(16.0)))
                            .anchor(gpui::Corner::TopRight)
                            .snap_to_window()
                            .child(self.panel_actions(pane, cx)),
                    )),
                )
            })
    }

    fn desktop_panel(
        &mut self,
        panel: &Panel,
        width: f32,
        height: f32,
        path: &mut Vec<bool>,
        cx: &Context<Self>,
    ) -> Div {
        let container = div()
            .w(px(width))
            .h(px(height))
            .flex_none()
            .overflow_hidden();
        match panel {
            Panel::Empty => container
                .flex()
                .flex_col()
                .items_center()
                .justify_center()
                .gap_2()
                .text_size(px(13.0))
                .text_color(rgb(0xaab8cc))
                .child(if !self.sessions.has_projects() {
                    "Añade un repositorio para empezar."
                } else if self.panes.is_empty() {
                    "Abre una sesión desde un proyecto."
                } else {
                    "Abre una sesión o selecciona una en la barra lateral."
                })
                .when(!path.is_empty(), |empty| {
                    let path = path.clone();
                    empty.child(button("Quitar panel").on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |view, _, _, cx| {
                            view.send(
                                Request::Panel(PanelAction::Dismiss { path: path.clone() }),
                                cx,
                            );
                        }),
                    ))
                }),
            Panel::Pane(index) => {
                self.resize(*index, width, height);
                self.pane(*index, width, height, cx)
            }
            Panel::Split {
                axis,
                ratio,
                first,
                second,
            } => {
                let ratio = self
                    .panel_drag
                    .as_ref()
                    .filter(|drag| drag.path == *path)
                    .map_or(*ratio, |drag| drag.ratio);
                let columns = *axis == Axis::Columns;
                let extent = if columns { width } else { height };
                let divider = DIVIDER.min(extent);
                let available = extent - divider;
                let first_extent = available * ratio;
                let second_extent = available - first_extent;
                let dimensions = |extent| {
                    if columns {
                        (extent, height)
                    } else {
                        (width, extent)
                    }
                };

                let divider_path = path.clone();
                let axis = *axis;
                let (divider_width, divider_height) = dimensions(divider);
                let separator = separator_style(axis, divider_width, divider_height).on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |view, event: &gpui::MouseDownEvent, _, cx| {
                        view.panel_menu = None;
                        view.panel_drag = Some(PanelDrag {
                            path: divider_path.clone(),
                            axis,
                            origin: axis_position(axis, event.position),
                            initial_ratio: ratio,
                            ratio,
                            available,
                        });
                        cx.notify();
                    }),
                );
                path.push(false);
                let (w, h) = dimensions(first_extent);
                let first = self.desktop_panel(first, w, h, path, cx);
                *path.last_mut().unwrap() = true;
                let (w, h) = dimensions(second_extent);
                let second = self.desktop_panel(second, w, h, path, cx);
                path.pop();

                container
                    .flex()
                    .when(!columns, |group| group.flex_col())
                    .child(first)
                    .child(separator)
                    .child(second)
            }
        }
    }

    fn panel_drag_move(&mut self, event: &MouseMoveEvent, _: &mut Window, cx: &mut Context<Self>) {
        if event.pressed_button != Some(MouseButton::Left) {
            if self.panel_drag.take().is_some() {
                cx.notify();
            }
            return;
        }
        if let Some(drag) = &mut self.panel_drag {
            drag.ratio = drag.ratio_at(event.position);
            cx.notify();
        }
    }

    fn panel_drag_end(
        &mut self,
        event: &gpui::MouseUpEvent,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(drag) = self.panel_drag.take() {
            let ratio = drag.ratio_at(event.position);
            if self.desktop_layout.resize(&drag.path, ratio).is_ok() {
                self.send(
                    Request::Panel(PanelAction::Resize {
                        path: drag.path,
                        ratio,
                    }),
                    cx,
                );
            }
            cx.notify();
        }
    }

    fn panel_actions(&self, pane: usize, cx: &Context<Self>) -> Div {
        div()
            .p_1()
            .flex()
            .flex_col()
            .gap_1()
            .rounded_md()
            .bg(rgb(0x171d28))
            .when(
                self.panes.get(pane).is_some_and(|snapshot| {
                    snapshot.agent == Some(crate::host::Agent::Codex) && !snapshot.exited
                }),
                |menu| {
                    menu.child(button("Vincular UUID de Codex").on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |view, _, window, cx| {
                            view.panel_menu = None;
                            view.focus.focus(window);
                            if let Some(conversation) =
                                cx.read_from_clipboard().and_then(|item| item.text())
                            {
                                view.send(Request::LinkConversation { pane, conversation }, cx);
                            } else {
                                view.status = "Copia el UUID de la conversación de Codex.".into();
                                cx.notify();
                            }
                            cx.stop_propagation();
                        }),
                    ))
                },
            )
            .children(
                [
                    (
                        "Dividir a la derecha",
                        PanelAction::Split {
                            pane,
                            axis: Axis::Columns,
                        },
                    ),
                    (
                        "Dividir debajo",
                        PanelAction::Split {
                            pane,
                            axis: Axis::Rows,
                        },
                    ),
                    ("Ocultar sesión", PanelAction::Hide { pane }),
                    ("Cerrar sesión", PanelAction::Close { pane }),
                ]
                .into_iter()
                .map(|(label, action)| {
                    button(label).on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |view, _, window, cx| {
                            view.panel_menu = None;
                            view.focus.focus(window);
                            view.send(Request::Panel(action.clone()), cx);
                            cx.stop_propagation();
                        }),
                    )
                }),
            )
    }

    pub(super) fn focus_pane(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        self.panel_menu = None;
        if !self.panels_supported || self.desktop_layout.root.contains(index) {
            self.active = index;
        }
        self.focus.focus(window);
        if self.panels_supported {
            let _ = self
                .requests
                .send(Request::Panel(PanelAction::Show { pane: index }));
        } else if self.desktop {
            self.desktop_layout.root = Panel::Pane(index);
        }
        cx.notify();
    }
}
