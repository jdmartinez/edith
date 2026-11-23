use gpui::{
    Context, CursorStyle, Div, MouseButton, MouseMoveEvent, Window, div, prelude::*, px, rgb,
};

use crate::host::{PANE_COUNT, Request};
use crate::panels::Axis;
use crate::ui::controls::separator_style;

use super::{DIVIDER, HEADER_HEIGHT, SIDEBAR_WIDTH, Workspace};

#[derive(Clone, Copy)]
pub(super) enum Divider {
    Columns,
    Rows,
}

impl Workspace {
    pub(super) fn prototype(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Div {
        let bounds = window.viewport_size();
        let width = (f32::from(bounds.width) - SIDEBAR_WIDTH).max(0.0);
        let height = (f32::from(bounds.height) - HEADER_HEIGHT).max(0.0);
        let left = (width - DIVIDER).max(0.0) * self.layout.left;
        let right = (width - DIVIDER - left).max(0.0);
        let top = (height - DIVIDER).max(0.0) * self.layout.top;
        let bottom = (height - DIVIDER - top).max(0.0);
        if self.panes.len() != PANE_COUNT {
            return div().child(self.status.clone());
        }
        self.resize(0, left, height);
        self.resize(1, right, top);
        self.resize(2, right, bottom);

        div()
            .size_full()
            .flex()
            .flex_col()
            .text_color(rgb(0xd9e0ee))
            .bg(rgb(0x11151d))
            .track_focus(&self.focus)
            .on_key_down(cx.listener(Self::key_down))
            .on_mouse_move(cx.listener(Self::drag_move))
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|view, _, _, _| {
                    view.drag = None;
                }),
            )
            .on_mouse_up_out(
                MouseButton::Left,
                cx.listener(|view, _, _, _| {
                    view.drag = None;
                }),
            )
            .child(
                div()
                    .flex()
                    .flex_none()
                    .items_center()
                    .justify_between()
                    .h(px(HEADER_HEIGHT))
                    .px_4()
                    .bg(rgb(0x171d28))
                    .text_size(px(13.0))
                    .child("Edith · Prototipo GPUI")
                    .child(div().text_color(rgb(0xaab8cc)).child(self.status.clone())),
            )
            .child(
                div()
                    .flex()
                    .flex_row()
                    .w_full()
                    .h(px(height))
                    .child(self.sidebar(height, cx))
                    .child(self.pane(0, left, height, cx))
                    .child(self.separator(Divider::Columns, DIVIDER, height, cx))
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .w(px(right))
                            .h(px(height))
                            .child(self.pane(1, right, top, cx))
                            .child(self.separator(Divider::Rows, right, DIVIDER, cx))
                            .child(self.pane(2, right, bottom, cx)),
                    ),
            )
    }

    fn drag_move(&mut self, event: &MouseMoveEvent, window: &mut Window, cx: &mut Context<Self>) {
        if event.pressed_button != Some(MouseButton::Left) {
            self.drag = None;
            return;
        }
        let bounds = window.viewport_size();
        let position = match self.drag {
            Some(Divider::Columns) => &mut self.layout.left,
            Some(Divider::Rows) => &mut self.layout.top,
            None => return,
        };
        let ratio = match self.drag {
            Some(Divider::Columns) => {
                (f32::from(event.position.x) - SIDEBAR_WIDTH - DIVIDER / 2.0)
                    / (f32::from(bounds.width) - SIDEBAR_WIDTH - DIVIDER).max(1.0)
            }
            Some(Divider::Rows) => {
                (f32::from(event.position.y) - HEADER_HEIGHT - DIVIDER / 2.0)
                    / (f32::from(bounds.height) - HEADER_HEIGHT - DIVIDER).max(1.0)
            }
            None => return,
        };
        *position = ratio.clamp(0.15, 0.85);
        let _ = self.requests.send(Request::SetLayout(self.layout));
        cx.notify();
    }

    fn separator(&self, divider: Divider, width: f32, height: f32, cx: &Context<Self>) -> Div {
        let axis = match divider {
            Divider::Columns => Axis::Columns,
            Divider::Rows => Axis::Rows,
        };
        separator_style(axis, width, height).on_mouse_down(
            MouseButton::Left,
            cx.listener(move |view, _, _, _| {
                view.drag = Some(divider);
            }),
        )
    }

    fn sidebar(&self, height: f32, cx: &Context<Self>) -> Div {
        let path = std::path::Path::new(&self.project_dir);
        let project = path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("Proyecto local");
        let display_path = dirs::home_dir()
            .and_then(|home| path.strip_prefix(home).ok())
            .map(|relative| {
                std::path::Path::new("~")
                    .join(relative)
                    .display()
                    .to_string()
            })
            .unwrap_or_else(|| self.project_dir.clone());

        div()
            .w(px(SIDEBAR_WIDTH))
            .h(px(height))
            .flex_none()
            .overflow_hidden()
            .border_r_1()
            .border_color(rgb(0x222a38))
            .bg(rgb(0x151a24))
            .p(px(12.0))
            .child(
                div()
                    .px(px(6.0))
                    .pb(px(14.0))
                    .child(
                        div()
                            .text_size(px(16.0))
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .child(project.to_owned()),
                    )
                    .child(
                        div()
                            .truncate()
                            .text_size(px(11.0))
                            .text_color(rgb(0x9aa8bc))
                            .child(display_path),
                    ),
            )
            .children(self.panes.iter().enumerate().map(|(index, snapshot)| {
                let (icon, agent_color, name) = match snapshot.label.as_str() {
                    "claude" => ("C", 0xf4a184, "Claude Code".to_owned()),
                    "codex" => ("Cx", 0xa4e4cd, "Codex".to_owned()),
                    "copilot" => ("GH", 0xc7b6ff, "Copilot CLI".to_owned()),
                    _ => (">_", 0x98b8ec, format!("Terminal {}", index + 1)),
                };
                let (status, status_color) = if snapshot.exited {
                    ("Cerrada", 0x9299a8)
                } else if self.connected {
                    ("Abierta", 0x97c8aa)
                } else if self.initial_layout {
                    ("Sin conexión", 0xe8ae73)
                } else {
                    ("Conectando", 0x9299a8)
                };
                let details = match snapshot.pid {
                    Some(pid) => format!("{} · PID {pid}", snapshot.label),
                    None => snapshot.label.clone(),
                };

                div()
                    .cursor(CursorStyle::PointingHand)
                    .flex()
                    .items_center()
                    .gap(px(8.0))
                    .mb(px(4.0))
                    .h(px(50.0))
                    .px(px(7.0))
                    .rounded_md()
                    .bg(rgb(if self.active == index {
                        0x263b53
                    } else {
                        0x151a24
                    }))
                    .hover(|style| style.bg(rgb(0x233347)))
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |view, _, window, cx| {
                            view.focus_pane(index, window, cx);
                        }),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_none()
                            .items_center()
                            .justify_center()
                            .size(px(28.0))
                            .rounded_md()
                            .bg(rgb(0x202d3f))
                            .text_color(rgb(agent_color))
                            .text_size(px(11.0))
                            .font_weight(gpui::FontWeight::BOLD)
                            .child(icon),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_size(px(12.0))
                            .child(name)
                            .child(
                                div()
                                    .truncate()
                                    .text_size(px(10.0))
                                    .text_color(rgb(0xaab8cc))
                                    .child(details),
                            ),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_none()
                            .items_center()
                            .gap(px(3.0))
                            .rounded_md()
                            .bg(rgb(0x1b2533))
                            .px(px(5.0))
                            .py(px(2.0))
                            .text_size(px(10.0))
                            .text_color(rgb(status_color))
                            .child(div().size(px(5.0)).rounded_full().bg(rgb(status_color)))
                            .child(status),
                    )
            }))
    }
}
