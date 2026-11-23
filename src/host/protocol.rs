use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::panels::{DesktopLayout, PanelAction};

use super::git::{Repository, SessionDirectories, WorktreeRemoval};

pub const PANE_COUNT: usize = 3;

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Layout {
    pub left: f32,
    pub top: f32,
}

impl Default for Layout {
    fn default() -> Self {
        Self {
            left: 0.5,
            top: 0.5,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
pub enum Agent {
    Claude,
    Codex,
    Copilot,
}

impl Agent {
    pub const ALL: [Self; 3] = [Self::Claude, Self::Codex, Self::Copilot];

    pub fn name(self) -> &'static str {
        match self {
            Self::Claude => "Claude Code",
            Self::Codex => "Codex",
            Self::Copilot => "GitHub Copilot CLI",
        }
    }

    pub(super) fn executable(self) -> &'static str {
        match self {
            Self::Claude => "claude",
            Self::Codex => "codex",
            Self::Copilot => "copilot",
        }
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Snapshot {
    pub revision: u64,
    pub label: String,
    #[serde(default)]
    pub agent: Option<Agent>,
    #[serde(default)]
    pub directory: PathBuf,
    pub pid: Option<u32>,
    pub exited: bool,
    #[serde(default)]
    pub closed_by_user: bool,
    pub application_cursor: bool,
    pub bracketed_paste: bool,
    pub lines: Vec<Vec<Run>>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Run {
    pub text: String,
    pub cells: u16,
    pub fg: u32,
    pub bg: u32,
    pub bold: bool,
    pub underline: bool,
}

#[derive(Debug, Serialize, Deserialize)]
pub enum Request {
    Poll(Vec<u64>),
    DesktopReady,
    AddProject(PathBuf),
    ScanProjects(PathBuf),
    AddDiscoveredProjects(Vec<PathBuf>),
    SetAgentExecutable {
        agent: Agent,
        path: Option<PathBuf>,
    },
    SessionDirectories(PathBuf),
    InspectWorktreeRemoval {
        project: PathBuf,
        path: PathBuf,
    },
    RemoveWorktree {
        project: PathBuf,
        path: PathBuf,
        force: bool,
        delete_branch: bool,
    },
    StartSession {
        project: PathBuf,
        agent: Agent,
        #[serde(default)]
        target: SessionTarget,
    },
    Input {
        pane: usize,
        bytes: Vec<u8>,
    },
    Resize {
        pane: usize,
        rows: u16,
        cols: u16,
    },
    SetLayout(Layout),
    Panel(PanelAction),
    Stop,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub enum SessionTarget {
    #[default]
    Main,
    Existing(PathBuf),
    Branch(String),
    NewBranch {
        name: String,
        base: String,
    },
}

#[derive(Debug, Serialize, Deserialize)]
pub enum Reply {
    State {
        layout: Layout,
        #[serde(default)]
        desktop_layout: Option<DesktopLayout>,
        #[serde(default)]
        project_dir: String,
        panes: Vec<Option<Snapshot>>,
        #[serde(default)]
        projects: Vec<PathBuf>,
        #[serde(default)]
        worktrees: BTreeMap<PathBuf, Vec<PathBuf>>,
        #[serde(default)]
        agent_executables: BTreeMap<Agent, PathBuf>,
        #[serde(default)]
        terminal_projects: Vec<PathBuf>,
    },
    Discovered(Vec<Repository>),
    Directories(SessionDirectories),
    WorktreeRemoval(WorktreeRemoval),
    WorktreeRemoved {
        retained_branch: Option<String>,
    },
    BranchOccupied(PathBuf),
    SessionStarted,
    Ok,
    Error(String),
}
