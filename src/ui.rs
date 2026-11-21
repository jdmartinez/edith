use std::env;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::Result;
use gpui::{
    App, Application, Bounds, Context, CursorStyle, Div, FocusHandle, KeyDownEvent, Keystroke,
    MouseButton, MouseMoveEvent, SharedString, Window, WindowBounds, WindowOptions, div, font,
    prelude::*, px, rgb, size,
};

use crate::host::{Endpoint, Layout, PANE_COUNT, Reply, Request, Snapshot};

const HEADER_HEIGHT: f32 = 44.0;
const SIDEBAR_WIDTH: f32 = 252.0;
const PANE_HEADER_HEIGHT: f32 = 29.0;
const PADDING: f32 = 8.0;
const DIVIDER: f32 = 6.0;
const LINE_HEIGHT: f32 = 17.0;
const FONT_SIZE: f32 = 13.0;
const INTERVAL: Duration = Duration::from_millis(33);

#[cfg(target_os = "macos")]
const MONO_FONT: &str = "Menlo";
#[cfg(target_os = "windows")]
const MONO_FONT: &str = "Consolas";
#[cfg(not(any(target_os = "macos", target_os = "windows")))]
const MONO_FONT: &str = "DejaVu Sans Mono";

pub fn run(endpoint: Endpoint, agents: bool) -> Result<()> {
    endpoint.ensure_host(agents, &env::current_dir()?)?;
    let (send, requests) = mpsc::channel();
    let (updates, receive) = mpsc::channel();
    thread::spawn(move || client(endpoint, requests, updates));

    Application::new().run(move |cx: &mut App| {
        cx.on_window_closed(|cx| {
            if cx.windows().is_empty() {
                cx.quit();
            }
        })
        .detach();
        let bounds = Bounds::centered(None, size(px(1200.0), px(740.0)), cx);
        cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                window_min_size: Some(size(px(780.0), px(380.0))),
                ..Default::default()
            },
            |window, cx| {
                window.set_window_title("Edith • GPUI terminal prototype");
                cx.new(move |cx| {
                    let focus = cx.focus_handle();
                    focus.focus(window);
                    cx.spawn(async move |this, cx| {
                        loop {
                            cx.background_executor().timer(INTERVAL).await;
                            if this
                                .update(cx, |view: &mut Workspace, cx| {
                                    if view.refresh() {
                                        cx.notify();
                                    }
                                })
                                .is_err()
                            {
                                break;
                            }
                        }
                    })
                    .detach();
                    let font = cx.text_system().resolve_font(&font(MONO_FONT));
                    let cell_width = cx
                        .text_system()
                        .advance(font, px(FONT_SIZE), 'M')
                        .map(|advance| f32::from(advance.width))
                        .unwrap_or(8.0)
                        .max(1.0);
                    Workspace {
                        focus,
                        active: 0,
                        requests: send,
                        updates: receive,
                        panes: Default::default(),
                        project_dir: String::new(),
                        connected: false,
                        layout: Layout::default(),
                        initial_layout: false,
                        drag: None,
                        sizes: [(0, 0); PANE_COUNT],
                        cell_width,
                        status: "Conectando con el proceso de terminales…".into(),
                    }
                })
            },
        )
        .expect("could not open the GPUI prototype window");
        cx.activate(true);
    });
    Ok(())
}

fn client(endpoint: Endpoint, requests: Receiver<Request>, updates: Sender<Result<Reply>>) {
    let mut revisions = [0; PANE_COUNT];
    let mut next_poll = Instant::now();
    let mut initial = true;
    let mut disconnected = false;
    loop {
        if Instant::now() >= next_poll {
            let result = endpoint.request(Request::Poll(revisions));
            next_poll = Instant::now() + INTERVAL;
            match result {
                Ok(Reply::State {
                    layout,
                    project_dir,
                    panes,
                }) => {
                    let changed = initial || disconnected || panes.iter().any(Option::is_some);
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
                                project_dir,
                                panes,
                            }))
                            .is_err()
                    {
                        return;
                    }
                }
                Ok(_) => {}
                Err(error) if !disconnected => {
                    disconnected = true;
                    revisions = [0; PANE_COUNT];
                    if updates.send(Err(error)).is_err() {
                        return;
                    }
                }
                Err(_) => {}
            }
        }

        match requests.recv_timeout(next_poll.saturating_duration_since(Instant::now())) {
            Ok(request) => {
                if let Err(error) = endpoint.request(request) {
                    if updates.send(Ok(Reply::Error(error.to_string()))).is_err() {
                        return;
                    }
                }
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => return,
        }
    }
}

#[derive(Clone, Copy)]
enum Divider {
    Columns,
    Rows,
}

struct Workspace {
    focus: FocusHandle,
    active: usize,
    requests: Sender<Request>,
    updates: Receiver<Result<Reply>>,
    panes: [Snapshot; PANE_COUNT],
    project_dir: String,
    connected: bool,
    layout: Layout,
    initial_layout: bool,
    drag: Option<Divider>,
    sizes: [(u16, u16); PANE_COUNT],
    cell_width: f32,
    status: SharedString,
}

impl Workspace {
    fn refresh(&mut self) -> bool {
        let mut changed = false;
        for update in self.updates.try_iter() {
            changed = true;
            match update {
                Ok(Reply::State {
                    layout,
                    project_dir,
                    panes,
                }) => {
                    if !self.initial_layout {
                        self.layout = layout;
                        self.initial_layout = true;
                    }
                    self.project_dir = project_dir;
                    self.connected = true;
                    for (index, pane) in panes.into_iter().enumerate() {
                        if let Some(snapshot) = pane {
                            self.panes[index] = snapshot;
                        }
                    }
                    self.status = "Host conectado".into();
                }
                Ok(Reply::Error(error)) => self.status = format!("Host: {error}").into(),
                Ok(Reply::Ok) => {}
                Err(error) => {
                    self.connected = false;
                    self.sizes = [(0, 0); PANE_COUNT];
                    self.status = format!("Host: {error}").into();
                }
            }
        }
        changed
    }

    fn resize(&mut self, widths: [f32; PANE_COUNT], heights: [f32; PANE_COUNT]) {
        if !self.initial_layout || !self.connected {
            return;
        }
        for pane in 0..PANE_COUNT {
            let rows =
                (((heights[pane] - PANE_HEADER_HEIGHT - 2.0 * PADDING) / LINE_HEIGHT).floor()
                    as u16)
                    .clamp(2, 200);
            let cols =
                (((widths[pane] - 2.0 * PADDING) / self.cell_width).floor() as u16).clamp(2, 300);
            if self.sizes[pane] != (rows, cols) {
                self.sizes[pane] = (rows, cols);
                let _ = self.requests.send(Request::Resize { pane, rows, cols });
            }
        }
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
        div()
            .w(px(width))
            .h(px(height))
            .flex_none()
            .bg(rgb(0x222a38))
            .hover(|style| style.bg(rgb(0x599ce8)))
            .cursor(match divider {
                Divider::Columns => CursorStyle::ResizeLeftRight,
                Divider::Rows => CursorStyle::ResizeUpDown,
            })
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |view, _, _, _| {
                    view.drag = Some(divider);
                }),
            )
    }

    fn focus_pane(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        self.active = index;
        self.focus.focus(window);
        cx.notify();
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

    fn pane(&self, index: usize, width: f32, height: f32, cx: &Context<Self>) -> Div {
        let snapshot = &self.panes[index];
        let (rows, cols) = self.sizes[index];
        let focused = self.active == index;
        let label = if snapshot.label.is_empty() {
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
                    .child(format!("{}  {label}", index + 1))
                    .child(div().text_color(rgb(0xaab8cc)).child(status)),
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

    fn key_down(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let pane = self.active;
        if !self.connected || self.panes[pane].exited {
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
}

impl Render for Workspace {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let bounds = window.viewport_size();
        let width = (f32::from(bounds.width) - SIDEBAR_WIDTH).max(0.0);
        let height = (f32::from(bounds.height) - HEADER_HEIGHT).max(0.0);
        let left = (width - DIVIDER).max(0.0) * self.layout.left;
        let right = (width - DIVIDER - left).max(0.0);
        let top = (height - DIVIDER).max(0.0) * self.layout.top;
        let bottom = (height - DIVIDER - top).max(0.0);
        self.resize([left, right, right], [height, top, bottom]);

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
