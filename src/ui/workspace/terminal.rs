use gpui::{
    App, Context, Div, KeyDownEvent, Keystroke, MouseButton, SharedString, Window, div, font,
    prelude::*, px, rgb,
};

use crate::host::{Agent, AgentState, Request};
use crate::ui::controls::{button, icon_button};

use super::Workspace;

const PANE_HEADER_HEIGHT: f32 = 29.0;
const PADDING: f32 = 8.0;
const LINE_HEIGHT: f32 = 17.0;
const FONT_SIZE: f32 = 13.0;

#[cfg(target_os = "macos")]
const MONO_FONT: &str = "Menlo";
#[cfg(target_os = "windows")]
const MONO_FONT: &str = "Consolas";
#[cfg(not(any(target_os = "macos", target_os = "windows")))]
const MONO_FONT: &str = "DejaVu Sans Mono";

pub(super) fn cell_width(cx: &App) -> f32 {
    let font = cx.text_system().resolve_font(&font(MONO_FONT));
    cx.text_system()
        .advance(font, px(FONT_SIZE), 'M')
        .map(|advance| f32::from(advance.width))
        .unwrap_or(8.0)
        .max(1.0)
}

impl Workspace {
    pub(super) fn key_down(
        &mut self,
        event: &KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let pane = self.active;
        if !self.connected
            || (self.desktop && !self.desktop_layout.root.contains(pane))
            || self.panes.get(pane).is_none_or(|pane| pane.exited)
        {
            return;
        }
        let key = &event.keystroke;
        let modifiers = key.modifiers;
        let paste = key.key == "v"
            && ((cfg!(target_os = "macos") && modifiers.platform)
                || (modifiers.control && modifiers.shift));
        let bytes = if paste {
            let Some(text) = cx.read_from_clipboard().and_then(|item| item.text()) else {
                return;
            };
            if self.panes[pane].bracketed_paste {
                format!("\x1b[200~{text}\x1b[201~").into_bytes()
            } else {
                text.replace("\r\n", "\n").replace('\n', "\r").into_bytes()
            }
        } else {
            let Some(bytes) = terminal_key(key, self.panes[pane].application_cursor) else {
                return;
            };
            bytes
        };
        for chunk in bytes.chunks(16_384) {
            let _ = self.requests.send(Request::Input {
                pane,
                bytes: chunk.to_vec(),
            });
        }
        window.prevent_default();
        cx.stop_propagation();
    }

    pub(super) fn pane(&self, index: usize, width: f32, height: f32, cx: &Context<Self>) -> Div {
        let snapshot = &self.panes[index];
        let (rows, cols) = self.sizes[index];
        let focused = self.active == index;
        let label = if self.desktop {
            self.session_agent(index).name()
        } else if snapshot.label.is_empty() {
            "Terminal"
        } else {
            &snapshot.label
        };
        let status = match snapshot.pid {
            Some(pid) if snapshot.exited => format!("PID {pid} · Finalizado"),
            Some(pid) => format!("PID {pid} · {cols}×{rows}"),
            None if snapshot.exited => "Error de inicio".into(),
            None => "Esperando al host…".into(),
        };
        div()
            .flex()
            .flex_col()
            .w(px(width))
            .h(px(height))
            .flex_none()
            .overflow_hidden()
            .bg(rgb(0x11151d))
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
                    .justify_between()
                    .h(px(PANE_HEADER_HEIGHT))
                    .px(px(PADDING))
                    .bg(rgb(if focused { 0x263b53 } else { 0x1c2431 }))
                    .text_size(px(12.0))
                    .child(div().min_w_0().truncate().child(if self.desktop {
                        let directory = if snapshot.directory.as_os_str().is_empty() {
                            &self.terminal_projects[index]
                        } else {
                            &snapshot.directory
                        };
                        format!("{label} {} · {}", index + 1, directory.display())
                    } else {
                        format!("{}  {label}", index + 1)
                    }))
                    .child(
                        div()
                            .flex()
                            .flex_none()
                            .items_center()
                            .gap_2()
                            .when(!self.panels_supported || width >= 285.0, |header| {
                                header.child(div().text_color(rgb(0xaab8cc)).child(status))
                            })
                            .when(self.panels_supported, |header| {
                                header.child(button("Panel ▾").on_mouse_down(
                                    MouseButton::Left,
                                    cx.listener(
                                        move |view, event: &gpui::MouseDownEvent, _, cx| {
                                            view.panel_menu =
                                                (view.panel_menu.map(|(pane, _)| pane)
                                                    != Some(index))
                                                .then_some((index, event.position));
                                            cx.stop_propagation();
                                            cx.notify();
                                        },
                                    ),
                                ))
                            }),
                    ),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .p(px(PADDING))
                    .overflow_hidden()
                    .font_family(MONO_FONT)
                    .text_size(px(FONT_SIZE))
                    .line_height(px(LINE_HEIGHT))
                    .whitespace_nowrap()
                    .children(snapshot.lines.iter().map(|line| {
                        div()
                            .flex()
                            .flex_row()
                            .flex_none()
                            .h(px(LINE_HEIGHT))
                            .children(line.iter().map(|run| {
                                let mut span = div()
                                    .w(px(self.cell_width * f32::from(run.cells)))
                                    .h(px(LINE_HEIGHT))
                                    .flex_none()
                                    .overflow_hidden()
                                    .text_color(rgb(run.fg))
                                    .bg(rgb(run.bg));
                                if run.bold {
                                    span = span.font_weight(gpui::FontWeight::BOLD);
                                }
                                if run.underline {
                                    span = span.underline();
                                }
                                span.child(SharedString::from(run.text.clone()))
                            }))
                    })),
            )
    }

    pub(super) fn resize(&mut self, pane: usize, width: f32, height: f32) {
        if !self.initial_layout || !self.connected || self.panes[pane].exited {
            return;
        }
        let rows = (((height - PANE_HEADER_HEIGHT - 2.0 * PADDING) / LINE_HEIGHT).floor() as u16)
            .clamp(2, 200);
        let cols = (((width - 2.0 * PADDING) / self.cell_width).floor() as u16).clamp(2, 300);
        if self.sizes[pane] != (rows, cols) {
            self.sizes[pane] = (rows, cols);
            let _ = self.requests.send(Request::Resize { pane, rows, cols });
        }
    }

    pub(super) fn session_agent(&self, index: usize) -> Agent {
        self.panes
            .get(index)
            .and_then(|pane| pane.agent)
            .unwrap_or(Agent::Codex)
    }

    pub(super) fn session_button(&self, index: usize, label: String, cx: &Context<Self>) -> Div {
        icon_button(self.session_agent(index), label)
            .when_some(self.panes.get(index), |button, snapshot| {
                let (text, color) = if !self.connected {
                    ("Sin conexión", 0x9eacc1)
                } else {
                    let state = if snapshot.exited {
                        AgentState::Closed
                    } else {
                        snapshot.attention.state
                    };
                    match state {
                        AgentState::Working => ("Trabajando", 0x8ab4ed),
                        AgentState::Attention => ("Requiere atención", 0xf0b687),
                        AgentState::Ready => ("Lista", 0x87e0cd),
                        AgentState::Open => ("Abierta", 0x9eacc1),
                        AgentState::Closed => ("Cerrada", 0x9eacc1),
                    }
                };
                let badge = if self.connected && !snapshot.exited && snapshot.attention.estimated {
                    format!("{text} (estimado)")
                } else {
                    text.to_owned()
                };
                button.child(
                    div()
                        .flex_none()
                        .text_size(px(10.0))
                        .text_color(rgb(color))
                        .child(badge),
                )
            })
            .when(index == self.active, |button| {
                button.bg(rgb(0x2b405a)).text_color(rgb(0xf1f5ff))
            })
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |view, _, window, cx| {
                    view.focus_pane(index, window, cx);
                }),
            )
    }
}

fn terminal_key(stroke: &Keystroke, application_cursor: bool) -> Option<Vec<u8>> {
    let modifiers = stroke.modifiers;
    if modifiers.platform {
        return None;
    }

    let key = stroke.key.as_str();
    let cursor = match key {
        "up" => Some('A'),
        "down" => Some('B'),
        "right" => Some('C'),
        "left" => Some('D'),
        "end" => Some('F'),
        "home" => Some('H'),
        _ => None,
    };
    if let Some(cursor) = cursor {
        let code = 1
            + u8::from(modifiers.shift)
            + u8::from(modifiers.alt) * 2
            + u8::from(modifiers.control) * 4;
        let sequence = if code != 1 {
            format!("\x1b[1;{code}{cursor}")
        } else if application_cursor {
            format!("\x1bO{cursor}")
        } else {
            format!("\x1b[{cursor}")
        };
        return Some(sequence.into_bytes());
    }

    let sequence = match key {
        "enter" => "\r",
        "tab" if modifiers.shift => "\x1b[Z",
        "tab" => "\t",
        "backspace" if modifiers.control => "\x08",
        "backspace" => "\x7f",
        "escape" => "\x1b",
        "insert" => "\x1b[2~",
        "delete" => "\x1b[3~",
        "pageup" => "\x1b[5~",
        "pagedown" => "\x1b[6~",
        "f1" => "\x1bOP",
        "f2" => "\x1bOQ",
        "f3" => "\x1bOR",
        "f4" => "\x1bOS",
        "f5" => "\x1b[15~",
        "f6" => "\x1b[17~",
        "f7" => "\x1b[18~",
        "f8" => "\x1b[19~",
        "f9" => "\x1b[20~",
        "f10" => "\x1b[21~",
        "f11" => "\x1b[23~",
        "f12" => "\x1b[24~",
        _ => "",
    };
    if !sequence.is_empty() {
        return Some(sequence.as_bytes().to_vec());
    }

    if modifiers.control {
        let control = match key {
            "space" | "@" => 0,
            "[" => 27,
            "\\" => 28,
            "]" => 29,
            "^" => 30,
            "_" => 31,
            letter if letter.len() == 1 && letter.as_bytes()[0].is_ascii_alphabetic() => {
                letter.as_bytes()[0].to_ascii_lowercase() - b'a' + 1
            }
            _ => return None,
        };
        return Some(vec![control]);
    }
    if let Some(text) = &stroke.key_char {
        return Some(text.as_bytes().to_vec());
    }
    if key == "space" {
        return Some(vec![b' ']);
    }
    None
}
