use std::path::PathBuf;
use std::sync::mpsc::{Receiver, Sender};

use anyhow::Result;
use gpui::{Context, FocusHandle, SharedString, Window, prelude::*};

use crate::host::{Layout, Reply, Request, Snapshot};
use crate::panels::{DesktopLayout, Panel};
use crate::usage_panel::UsagePanel;

use super::client::INTERVAL;
use super::notifications::{Notifications, deliver_attention};
use desktop::PanelDrag;
use prototype::Divider;
use sessions::Sessions;

mod desktop;
mod prototype;
mod sessions;
mod terminal;
mod usage;

const HEADER_HEIGHT: f32 = 44.0;
const SIDEBAR_WIDTH: f32 = 252.0;
const DIVIDER: f32 = 6.0;

pub(super) struct Workspace {
    focus: FocusHandle,
    active: usize,
    requests: Sender<Request>,
    updates: Receiver<Result<Reply>>,
    panes: Vec<Snapshot>,
    desktop: bool,
    sidebar_visible: bool,
    usage: UsagePanel,
    notifications: Notifications,
    sessions: Sessions,
    terminal_projects: Vec<PathBuf>,
    retention_days: Option<u32>,
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
    pub(super) fn new(
        requests: Sender<Request>,
        updates: Receiver<Result<Reply>>,
        desktop: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
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
        let cell_width = terminal::cell_width(cx);
        Self {
            focus,
            active: 0,
            requests,
            updates,
            panes: Vec::new(),
            desktop,
            sidebar_visible: true,
            usage: UsagePanel::default(),
            notifications: Notifications::default(),
            sessions: Sessions::new(cx.focus_handle()),
            terminal_projects: Vec::new(),
            retention_days: Some(30),
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
                    retention_days,
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
                    self.sessions
                        .update_projects(projects, worktrees, agent_executables);
                    self.terminal_projects = terminal_projects;
                    self.retention_days = retention_days;
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
                    let initial_connection = !self.connected;
                    self.connected = true;
                    for (index, pane) in panes.into_iter().enumerate() {
                        if let Some(snapshot) = pane {
                            if self.notifications.observe(
                                index,
                                snapshot.pid,
                                snapshot.attention,
                                initial_connection,
                            ) {
                                deliver_attention();
                            }
                            self.panes[index] = snapshot;
                        }
                    }
                }
                Ok(Reply::Directories(directories)) => self.sessions.set_directories(directories),
                Ok(Reply::BranchOccupied(path)) => self.sessions.set_occupied(path),
                Ok(Reply::WorktreeRemoval(preview)) => {
                    self.sessions.set_worktree_removal(Some(preview))
                }
                Ok(Reply::WorktreeRemoved { retained_branch }) => {
                    self.sessions.set_worktree_removal(None);
                    self.status = match retained_branch {
                        Some(branch) => {
                            format!("Worktree retirado. Git conservó la rama {branch}.").into()
                        }
                        None => "Worktree retirado".into(),
                    };
                }
                Ok(Reply::SessionStarted) => self.sessions.finish_session(),
                Ok(Reply::Error(error)) => {
                    self.sessions.allow_session_retry();
                    self.status = format!("Host: {error}").into();
                }
                Ok(Reply::Discovered(candidates)) => {
                    self.sessions.set_discovery(Some(candidates));
                    self.status = "Host conectado".into();
                }
                Ok(Reply::Ok) => {
                    self.sessions.set_discovery(None);
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

    fn send(&mut self, request: Request, cx: &mut Context<Self>) {
        self.status = if self.requests.send(request).is_ok() {
            "Host conectado".into()
        } else {
            "Sin conexión con el host".into()
        };
        cx.notify();
    }
}

impl Render for Workspace {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if self.usage.open {
            return self.usage_view(cx).into_any_element();
        }
        if self.desktop {
            return self.desktop(window, cx).into_any_element();
        }
        self.prototype(window, cx).into_any_element()
    }
}
