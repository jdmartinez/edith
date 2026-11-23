use std::borrow::Cow;
use std::collections::{BTreeMap, BTreeSet};
use std::env;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::Result;
use gpui::{
    App, Application, AssetSource, Bounds, Context, CursorStyle, Div, FocusHandle, KeyDownEvent,
    Keystroke, MouseButton, MouseMoveEvent, PathPromptOptions, SharedString, Window, WindowBounds,
    WindowOptions, div, font, prelude::*, px, rgb, size, svg,
};

use crate::host::{
    Agent, Endpoint, Layout, PANE_COUNT, Reply, Repository, Request, SessionDirectories,
    SessionTarget, Snapshot, WorktreeRemoval,
};

use crate::panels::{Axis, DesktopLayout, Panel, PanelAction};
use crate::usage_panel::UsagePanel;

const HEADER_HEIGHT: f32 = 44.0;
const STATUS_HEIGHT: f32 = 25.0;
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

pub fn run(endpoint: Endpoint, agents: bool, desktop: bool) -> Result<()> {
    endpoint.ensure_host(agents, desktop, &env::current_dir()?)?;
    let (send, requests) = mpsc::channel();
    let (updates, receive) = mpsc::channel();
    thread::spawn(move || client(endpoint, requests, updates));

    let app = Application::new().with_assets(AgentIcons);
    app.run(move |cx: &mut App| {
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
                window.set_window_title("Edith");
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
                        panes: Vec::new(),
                        desktop,
                        sidebar_visible: true,
                        usage: UsagePanel::default(),
                        projects: Vec::new(),
                        worktrees: BTreeMap::new(),
                        discovery: None,
                        selected_projects: BTreeSet::new(),
                        terminal_projects: Vec::new(),
                        agent_executables: BTreeMap::new(),
                        choosing_agent: None,
                        session_picker: None,
                        worktree_removal: None,
                        branch_input: cx.focus_handle(),
                        project_dir: String::new(),
                        connected: false,
                        layout: Layout::default(),
                        desktop_layout: DesktopLayout::default(),
                        panels_supported: false,
                        panel_menu: None,
                        panel_drag: None,
                        initial_layout: false,
                        drag: None,
                        sizes: Vec::new(),
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
                }) => {
                    let metadata = (
                        projects,
                        worktrees,
                        agent_executables,
                        terminal_projects,
                        desktop_layout,
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

#[derive(Clone, Copy)]
enum Divider {
    Columns,
    Rows,
}

struct PanelDrag {
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

fn separator_style(axis: Axis, width: f32, height: f32) -> Div {
    div()
        .w(px(width))
        .h(px(height))
        .flex_none()
        .bg(rgb(0x222a38))
        .hover(|style| style.bg(rgb(0x599ce8)))
        .cursor(match axis {
            Axis::Columns => CursorStyle::ResizeLeftRight,
            Axis::Rows => CursorStyle::ResizeUpDown,
        })
}

#[derive(Clone, Copy)]
enum PathAction {
    AddProject,
    ScanProjects,
    ChooseExecutable(Agent),
}

struct SessionPicker {
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

struct Workspace {
    focus: FocusHandle,
    active: usize,
    requests: Sender<Request>,
    updates: Receiver<Result<Reply>>,
    panes: Vec<Snapshot>,
    desktop: bool,
    sidebar_visible: bool,
    usage: UsagePanel,
    projects: Vec<PathBuf>,
    worktrees: BTreeMap<PathBuf, Vec<PathBuf>>,
    discovery: Option<Vec<Repository>>,
    selected_projects: BTreeSet<PathBuf>,
    terminal_projects: Vec<PathBuf>,
    agent_executables: BTreeMap<Agent, PathBuf>,
    choosing_agent: Option<PathBuf>,
    session_picker: Option<SessionPicker>,
    worktree_removal: Option<WorktreeRemoval>,
    branch_input: FocusHandle,
    project_dir: String,
    connected: bool,
    layout: Layout,
    desktop_layout: DesktopLayout,
    panels_supported: bool,
    panel_menu: Option<(usize, gpui::Point<gpui::Pixels>)>,
    panel_drag: Option<PanelDrag>,
    initial_layout: bool,
    drag: Option<Divider>,
    sizes: Vec<(u16, u16)>,
    cell_width: f32,
    status: SharedString,
}

impl Workspace {
    fn usage_view(&self, cx: &mut Context<Self>) -> Div {
        div()
            .size_full()
            .flex()
            .flex_col()
            .bg(rgb(0x11151d))
            .text_color(rgb(0xd9e0ee))
            .track_focus(&self.focus)
            .on_key_down(cx.listener(|view, event: &KeyDownEvent, _, cx| {
                if event.keystroke.key == "escape" {
                    view.usage.open = false;
                    cx.notify();
                }
            }))
            .child(
                div()
                    .flex()
                    .flex_none()
                    .items_center()
                    .gap_3()
                    .p_4()
                    .bg(rgb(0x171d28))
                    .border_b_1()
                    .border_color(rgb(0x2a3344))
                    .child(button("Volver").on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|view, _, _, cx| {
                            view.usage.open = false;
                            cx.notify();
                        }),
                    ))
                    .child(div().text_lg().child("Historial de uso local"))
                    .child(
                        div()
                            .flex_1()
                            .text_sm()
                            .text_color(rgb(0xaab8cc))
                            .child(self.usage.status()),
                    )
                    .child(if self.usage.scanning() {
                        div()
                            .text_sm()
                            .text_color(rgb(0xaab8cc))
                            .child("Actualizando…")
                    } else {
                        button("Actualizar").on_mouse_down(
                            MouseButton::Left,
                            cx.listener(|view, _, _, cx| {
                                view.usage.scan();
                                cx.notify();
                            }),
                        )
                    }),
            )
            .child(
                div()
                    .id("usage-history")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .child(self.usage.content()),
            )
    }

    fn desktop(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Div {
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
                            .children(self.terminal_projects.iter().enumerate().map(
                                |(index, project)| {
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
                                },
                            )),
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
                .child(if self.projects.is_empty() {
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

    fn desktop_sidebar(&self, cx: &Context<Self>) -> impl IntoElement {
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
            .when_some(self.discovery.as_ref(), |sidebar, candidates| {
                sidebar.child(self.discovery_panel(candidates, cx))
            })
            .children(self.projects.iter().map(|project| {
                let root = project.clone();
                div()
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
                        self.worktrees
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
                                                view.worktree_removal = None;
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
                                        self.worktree_removal.as_ref().filter(|preview| {
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
                            .filter(|(_, owner)| *owner == project)
                            .map(|(index, _)| {
                                let state = if self.panes[index].exited {
                                    "Cerrada"
                                } else if self.connected {
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
                    .child(
                        button(
                            if self.choosing_agent.as_ref() == Some(project)
                                || self
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
                                    .session_picker
                                    .as_ref()
                                    .is_some_and(|picker| picker.pending)
                                {
                                    return;
                                }
                                let already_open = view.choosing_agent.as_ref() == Some(&root)
                                    || view
                                        .session_picker
                                        .as_ref()
                                        .is_some_and(|picker| picker.project == root);
                                view.session_picker = None;
                                view.focus.focus(window);
                                view.choosing_agent = (!already_open).then(|| root.clone());
                                cx.notify();
                            }),
                        ),
                    )
                    .when(self.choosing_agent.as_ref() == Some(project), |row| {
                        row.children(Agent::ALL.map(|agent| {
                            let project = project.clone();
                            icon_button(agent, agent.name()).mt_1().on_mouse_down(
                                MouseButton::Left,
                                cx.listener(move |view, _, _, cx| {
                                    view.choosing_agent = None;
                                    view.session_picker = Some(SessionPicker {
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
                    })
                    .when_some(
                        self.session_picker
                            .as_ref()
                            .filter(|picker| picker.project == *project),
                        |row, picker| row.child(self.directory_picker(picker, cx)),
                    )
            }))
            .child(
                div()
                    .text_size(px(12.0))
                    .text_color(rgb(0xaeb8c9))
                    .child("Ejecutables de agentes")
                    .children(Agent::ALL.map(|agent| {
                        let custom = self.agent_executables.get(&agent);
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
                    view.worktree_removal = None;
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
                                    view.worktree_removal = None;
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

    fn directory_picker(&self, picker: &SessionPicker, cx: &Context<Self>) -> Div {
        let panel = div().mt_2().flex().flex_col().gap_1().text_size(px(12.0));
        if picker.pending {
            return panel.child("Consultando el repositorio…");
        }
        let Some(directories) = &picker.directories else {
            return panel.child(button("Reintentar").on_mouse_down(
                MouseButton::Left,
                cx.listener(|view, _, _, cx| {
                    if let Some(picker) = &mut view.session_picker {
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
                        if let Some(picker) = &mut view.session_picker {
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
                        .track_focus(&self.branch_input)
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
                                view.branch_input.focus(window);
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
                            if let Some(picker) = &mut view.session_picker {
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
                        if let Some(picker) = &mut view.session_picker {
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
                if let Some(picker) = &mut view.session_picker {
                    picker.occupied = None;
                    picker.creating = true;
                    view.branch_input.focus(window);
                    cx.notify();
                }
            }),
        )
    }

    fn start_session(&mut self, target: SessionTarget, cx: &mut Context<Self>) {
        if let Some(picker) = &mut self.session_picker
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
                let selected = self.selected_projects.contains(&root);
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
                        if !view.selected_projects.remove(&root) {
                            view.selected_projects.insert(root.clone());
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
                    .when(!self.selected_projects.is_empty(), |row| {
                        row.child(button("Añadir seleccionados").on_mouse_down(
                            MouseButton::Left,
                            cx.listener(|view, _, _, cx| {
                                view.send(
                                    Request::AddDiscoveredProjects(
                                        view.selected_projects.iter().cloned().collect(),
                                    ),
                                    cx,
                                );
                            }),
                        ))
                    })
                    .child(button("Cerrar").on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|view, _, _, cx| {
                            view.discovery = None;
                            view.selected_projects.clear();
                            cx.notify();
                        }),
                    )),
            )
    }

    fn session_agent(&self, index: usize) -> Agent {
        self.panes
            .get(index)
            .and_then(|pane| pane.agent)
            .unwrap_or(Agent::Codex)
    }

    fn session_button(&self, index: usize, label: String, cx: &Context<Self>) -> Div {
        icon_button(self.session_agent(index), label)
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

    fn refresh(&mut self) -> bool {
        let mut changed = self.usage.refresh();
        for update in self.updates.try_iter() {
            changed = true;
            match update {
                Ok(Reply::State {
                    layout,
                    desktop_layout,
                    project_dir,
                    panes,
                    projects,
                    worktrees,
                    agent_executables,
                    terminal_projects,
                }) => {
                    if self.desktop {
                        self.panels_supported = desktop_layout.is_some();
                        if let Some(layout) = desktop_layout {
                            self.active = layout.focused.unwrap_or_default();
                            self.desktop_layout = layout;
                        } else {
                            if panes.len() > self.panes.len() {
                                self.active = panes.len() - 1;
                            }
                            self.desktop_layout.root = panes
                                .get(self.active)
                                .map_or(Panel::Empty, |_| Panel::Pane(self.active));
                        }
                    }
                    self.projects = projects;
                    self.worktrees = worktrees;
                    self.agent_executables = agent_executables;
                    self.terminal_projects = terminal_projects;
                    self.panes.resize_with(panes.len(), Snapshot::default);
                    self.sizes.resize(panes.len(), (0, 0));
                    if self.active >= panes.len() {
                        self.active = 0;
                    }
                    if !self.initial_layout {
                        self.layout = layout;
                        self.initial_layout = true;
                    }
                    self.project_dir = project_dir;
                    if !self.connected {
                        self.status = "Host conectado".into();
                    }
                    self.connected = true;
                    for (index, pane) in panes.into_iter().enumerate() {
                        if let Some(snapshot) = pane {
                            self.panes[index] = snapshot;
                        }
                    }
                }
                Ok(Reply::Directories(directories)) => {
                    if let Some(picker) = &mut self.session_picker
                        && picker.project == directories.project
                    {
                        picker.directories = Some(directories);
                        picker.pending = false;
                    }
                }
                Ok(Reply::BranchOccupied(path)) => {
                    if let Some(picker) = &mut self.session_picker {
                        picker.occupied = Some(path);
                        picker.pending = false;
                    }
                }
                Ok(Reply::WorktreeRemoval(preview)) => self.worktree_removal = Some(preview),
                Ok(Reply::WorktreeRemoved { retained_branch }) => {
                    self.worktree_removal = None;
                    self.status = match retained_branch {
                        Some(branch) => {
                            format!("Worktree retirado. Git conservó la rama {branch}.").into()
                        }
                        None => "Worktree retirado".into(),
                    };
                }
                Ok(Reply::SessionStarted) => self.session_picker = None,
                Ok(Reply::Error(error)) => {
                    if let Some(picker) = &mut self.session_picker {
                        picker.pending = false;
                    }
                    self.status = format!("Host: {error}").into();
                }
                Ok(Reply::Discovered(candidates)) => {
                    self.discovery = Some(candidates);
                    self.selected_projects.clear();
                    self.status = "Host conectado".into();
                }
                Ok(Reply::Ok) => {
                    self.discovery = None;
                    self.selected_projects.clear();
                }
                Err(error) => {
                    self.connected = false;
                    self.sizes.fill((0, 0));
                    self.status = format!("Host: {error}").into();
                }
            }
        }
        changed
    }

    fn resize(&mut self, pane: usize, width: f32, height: f32) {
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

    fn send(&mut self, request: Request, cx: &mut Context<Self>) {
        self.status = if self.requests.send(request).is_ok() {
            "Host conectado".into()
        } else {
            "Sin conexión con el host".into()
        };
        cx.notify();
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
                                view.discovery = None;
                                view.selected_projects.clear();
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

    fn focus_pane(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
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

    fn panel_actions(&self, pane: usize, cx: &Context<Self>) -> Div {
        div()
            .p_1()
            .flex()
            .flex_col()
            .gap_1()
            .rounded_md()
            .bg(rgb(0x171d28))
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

    fn edit_branch_name(
        &mut self,
        event: &KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(picker) = &mut self.session_picker {
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

    fn key_down(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
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
}

impl Render for Workspace {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let bounds = window.viewport_size();
        if self.usage.open {
            return self.usage_view(cx).into_any_element();
        }
        if self.desktop {
            return self.desktop(window, cx).into_any_element();
        }
        let width = (f32::from(bounds.width) - SIDEBAR_WIDTH).max(0.0);
        let height = (f32::from(bounds.height) - HEADER_HEIGHT).max(0.0);
        let left = (width - DIVIDER).max(0.0) * self.layout.left;
        let right = (width - DIVIDER - left).max(0.0);
        let top = (height - DIVIDER).max(0.0) * self.layout.top;
        let bottom = (height - DIVIDER - top).max(0.0);
        if self.panes.len() != PANE_COUNT {
            return div().child(self.status.clone()).into_any_element();
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
            .into_any_element()
    }
}

struct AgentIcons;

impl AssetSource for AgentIcons {
    fn load(&self, path: &str) -> Result<Option<Cow<'static, [u8]>>> {
        let bytes: &[u8] = match path {
            "claude.svg" => include_bytes!("../assets/agents/claude.svg"),
            "codex.svg" => include_bytes!("../assets/agents/codex.svg"),
            "copilot.svg" => include_bytes!("../assets/agents/copilot.svg"),
            _ => return Ok(None),
        };
        Ok(Some(Cow::Borrowed(bytes)))
    }

    fn list(&self, _path: &str) -> Result<Vec<SharedString>> {
        Ok(vec![
            "claude.svg".into(),
            "codex.svg".into(),
            "copilot.svg".into(),
        ])
    }
}

fn agent_icon(agent: Agent) -> impl IntoElement {
    let (path, color) = match agent {
        Agent::Claude => ("claude.svg", 0xe89172),
        Agent::Codex => ("codex.svg", 0xf0f1f2),
        Agent::Copilot => ("copilot.svg", 0xc8bcf4),
    };
    svg()
        .path(path)
        .size(px(15.0))
        .flex_none()
        .text_color(rgb(color))
}

fn icon_button(agent: Agent, text: impl Into<SharedString>) -> Div {
    button_style()
        .flex()
        .items_center()
        .gap(px(6.0))
        .child(agent_icon(agent))
        .child(div().min_w_0().truncate().child(text.into()))
}

fn button(text: impl Into<SharedString>) -> Div {
    button_style().child(text.into())
}

fn button_style() -> Div {
    div()
        .px_2()
        .py_1()
        .rounded_md()
        .text_size(px(12.0))
        .bg(rgb(0x222b3b))
        .hover(|style| style.bg(rgb(0x34465e)))
        .cursor(CursorStyle::PointingHand)
        .whitespace_nowrap()
}

fn project_name(path: &Path) -> &str {
    path.file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("Repositorio")
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
