use std::collections::BTreeMap;
use std::env;
use std::ffi::OsStr;
use std::fs::{self, File, OpenOptions};
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use anyhow::{Context, Result, bail, ensure};
use interprocess::local_socket::{ListenerOptions, Name, Stream, prelude::*};
use portable_pty::{Child, CommandBuilder, MasterPty, PtySize, native_pty_system};
use serde::{Deserialize, Serialize};

use crate::panels::{DesktopLayout, PanelAction};

pub const PANE_COUNT: usize = 3;
const MAX_REQUEST_BYTES: u64 = 256 * 1024;

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

    fn executable(self) -> &'static str {
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
pub struct SessionDirectories {
    pub project: PathBuf,
    pub worktrees: Vec<PathBuf>,
    pub branches: Vec<LocalBranch>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct LocalBranch {
    pub name: String,
    pub occupied: Option<PathBuf>,
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
    BranchOccupied(PathBuf),
    SessionStarted,
    Ok,
    Error(String),
}

pub struct Endpoint {
    dir: PathBuf,
}

impl Endpoint {
    pub fn find(desktop: bool) -> Result<Self> {
        let dir = match env::var_os("EDITH_PROTOTYPE_STATE") {
            Some(dir) => env::current_dir()?.join(dir),
            None => dirs::cache_dir()
                .context("could not find the current user's cache directory")?
                .join(if desktop {
                    "edith"
                } else {
                    "edith-gpui-prototype"
                }),
        };
        Ok(Self::at(dir))
    }

    pub fn at(dir: PathBuf) -> Self {
        Self { dir }
    }

    #[cfg(unix)]
    fn name(&self) -> Result<Name<'static>> {
        use interprocess::local_socket::GenericFilePath;
        Ok(self.dir.join("host.sock").to_fs_name::<GenericFilePath>()?)
    }

    #[cfg(windows)]
    fn name(&self) -> Result<Name<'static>> {
        use interprocess::local_socket::GenericNamespaced;
        use std::hash::{DefaultHasher, Hash, Hasher};

        let mut hasher = DefaultHasher::new();
        self.dir.hash(&mut hasher);
        Ok(format!("edith-gpui-prototype-{:016x}", hasher.finish())
            .to_ns_name::<GenericNamespaced>()?)
    }

    fn lock(&self) -> Result<File> {
        #[cfg(unix)]
        {
            use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
            fs::DirBuilder::new()
                .recursive(true)
                .mode(0o700)
                .create(&self.dir)?;
            let metadata = fs::symlink_metadata(&self.dir)?;
            ensure!(
                metadata.is_dir() && metadata.permissions().mode() & 0o077 == 0,
                "prototype state must be a private directory"
            );
        }
        #[cfg(windows)]
        fs::create_dir_all(&self.dir)?;
        let lock = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(self.dir.join("host.lock"))?;
        lock.try_lock()
            .context("another Edith prototype host owns this state directory")?;
        Ok(lock)
    }

    pub fn request(&self, request: Request) -> Result<Reply> {
        let mut connection = Stream::connect(self.name()?)?;
        write_message(&mut connection, &request)?;
        let reply = read_message(BufReader::new(connection), u64::MAX)?;
        match reply {
            Reply::Error(error) => bail!("{error}"),
            reply => Ok(reply),
        }
    }

    pub fn ensure_host(&self, agents: bool, desktop: bool, work_dir: &Path) -> Result<()> {
        let ready = || {
            self.request(if desktop {
                Request::DesktopReady
            } else {
                Request::Poll(vec![u64::MAX; PANE_COUNT])
            })
            .is_ok()
        };
        if ready() {
            return Ok(());
        }
        let mut command = Command::new(env::current_exe()?);
        command.arg(if desktop { "--desktop-host" } else { "--host" });
        if agents {
            command.arg("--agents");
        }
        command
            .env("EDITH_PROTOTYPE_STATE", &self.dir)
            .current_dir(work_dir)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped());
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            command.process_group(0);
        }
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
            const CREATE_NO_WINDOW: u32 = 0x0800_0000;
            command.creation_flags(CREATE_NEW_PROCESS_GROUP | CREATE_NO_WINDOW);
        }
        let mut child = command.spawn().context("could not launch the PTY host")?;
        for _ in 0..100 {
            if ready() {
                return Ok(());
            }
            if let Some(status) = child.try_wait()? {
                let mut error = String::new();
                if let Some(mut stderr) = child.stderr.take() {
                    stderr.read_to_string(&mut error)?;
                }
                bail!(
                    "the PTY host exited during startup ({status}): {}",
                    error.trim()
                );
            }
            thread::sleep(Duration::from_millis(50));
        }
        bail!("the PTY host did not become ready")
    }

    pub fn serve(&self, agents: bool, desktop: bool) -> Result<()> {
        let _lock = self.lock()?;
        let project_dir = env::current_dir()?.to_string_lossy().into_owned();
        // The lock guarantees that replacing a stale Unix socket cannot displace
        // another prototype host.
        let listener = ListenerOptions::new()
            .name(self.name()?)
            .try_overwrite(true)
            .create_sync()?;
        let programs = if agents {
            Agent::ALL.map(|agent| agent.executable().into())
        } else {
            [shell_program(), shell_program(), shell_program()]
        };
        let mut terminals = Vec::with_capacity(PANE_COUNT);
        for program in programs.into_iter().filter(|_| !desktop) {
            let terminal = match Terminal::new(&program, agents, Path::new(&project_dir)) {
                Ok(terminal) => Ok(terminal),
                Err(error) if agents => Err(unavailable_agent(&program, &error)),
                Err(error) => return Err(error),
            };
            terminals.push(terminal);
        }
        let mut layout = Layout::default();
        let mut workspace = WorkspaceState::default();

        for connection in listener.incoming() {
            let mut connection = connection?;
            let request = read_message(BufReader::new(&mut connection), MAX_REQUEST_BYTES);
            let stop = matches!(request, Ok(Request::Stop));
            let reply = request
                .and_then(|request| {
                    dispatch(
                        &mut terminals,
                        &mut layout,
                        &project_dir,
                        Some(&mut workspace).filter(|_| desktop),
                        request,
                    )
                })
                .unwrap_or_else(|error| Reply::Error(error.to_string()));
            // A UI may disconnect in the middle of a request.
            _ = write_message(&mut connection, &reply);
            if stop {
                break;
            }
        }
        Ok(())
    }
}

fn shell_program() -> std::ffi::OsString {
    #[cfg(unix)]
    return env::var_os("SHELL").unwrap_or_else(|| "/bin/sh".into());
    #[cfg(windows)]
    return env::var_os("ComSpec").unwrap_or_else(|| "cmd.exe".into());
}

fn write_message(writer: &mut impl Write, message: &impl Serialize) -> Result<()> {
    serde_json::to_writer(&mut *writer, message)?;
    writer.write_all(b"\n")?;
    writer.flush()?;
    Ok(())
}

fn read_message<T: serde::de::DeserializeOwned>(reader: impl BufRead, limit: u64) -> Result<T> {
    let mut line = Vec::new();
    reader.take(limit).read_until(b'\n', &mut line)?;
    ensure!(
        line.last() == Some(&b'\n'),
        "incomplete or oversized message"
    );
    Ok(serde_json::from_slice(&line)?)
}

#[derive(Default)]
struct WorkspaceState {
    layout: DesktopLayout,
    projects: Vec<Repository>,
    agent_executables: BTreeMap<Agent, PathBuf>,
    terminal_projects: Vec<PathBuf>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Repository {
    pub root: PathBuf,
    pub worktrees: Vec<PathBuf>,
}

fn git(path: &Path, args: &[impl AsRef<OsStr>]) -> Result<Vec<u8>> {
    let output = Command::new("git")
        .args(args)
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_COMMON_DIR")
        .current_dir(path)
        .output()
        .context("No se pudo ejecutar Git")?;
    ensure!(
        output.status.success(),
        "Git en {}: {}",
        path.display(),
        String::from_utf8_lossy(&output.stderr).trim()
    );
    Ok(output.stdout)
}

fn repository(path: &Path) -> Result<Repository> {
    let top = git(path, &["rev-parse", "--show-toplevel"])?;
    let top = std::str::from_utf8(&top)?
        .strip_suffix('\n')
        .unwrap_or_default();
    #[cfg(windows)]
    let top = top.strip_suffix('\r').unwrap_or(top);
    ensure!(!top.is_empty(), "Git no devolvió la raíz del repositorio");

    let mut checkouts = checkouts(Path::new(top))?.into_iter();
    let (main, _) = checkouts
        .next()
        .context("Git no devolvió el directorio principal")?;
    let root = main.canonicalize()?;
    ensure!(
        root.join(".git").exists(),
        "Selecciona una carpeta de un repositorio Git"
    );
    let worktrees = checkouts
        .map(|(path, _)| path)
        .filter(|path| path.is_dir())
        .collect();
    Ok(Repository { root, worktrees })
}

// Git lists the main checkout first. A branch can remain registered even
// when its directory is unavailable, so retain the original path in that case.
fn checkouts(project: &Path) -> Result<Vec<(PathBuf, Option<String>)>> {
    let listing = git(project, &["worktree", "list", "--porcelain", "-z"])?;
    let mut checkouts = Vec::new();
    let mut path = None;
    let mut branch = None;
    for field in listing.split(|byte| *byte == 0) {
        if let Some(value) = field.strip_prefix(b"worktree ") {
            path = std::str::from_utf8(value).ok().map(PathBuf::from);
        } else if let Some(value) = field.strip_prefix(b"branch refs/heads/") {
            branch = std::str::from_utf8(value).ok().map(str::to_owned);
        } else if field.is_empty() {
            let branch = branch.take();
            if let Some(path) = path.take() {
                checkouts.push((path.canonicalize().unwrap_or(path), branch));
            }
        }
    }
    Ok(checkouts)
}

fn session_directories(project: &Path) -> Result<SessionDirectories> {
    let checkouts = checkouts(project)?;
    let worktrees = checkouts
        .iter()
        .skip(1)
        .filter(|(path, _)| path.is_dir())
        .map(|(path, _)| path.clone())
        .collect();
    let branches = git(
        project,
        &["for-each-ref", "--format=%(refname:strip=2)", "refs/heads"],
    )?;
    let branches = std::str::from_utf8(&branches)?
        .lines()
        .map(|name| LocalBranch {
            name: name.into(),
            occupied: occupied_checkout(&checkouts, name),
        })
        .collect();
    Ok(SessionDirectories {
        project: project.into(),
        worktrees,
        branches,
    })
}

fn occupied_checkout(checkouts: &[(PathBuf, Option<String>)], name: &str) -> Option<PathBuf> {
    checkouts
        .iter()
        .find(|(_, branch)| branch.as_deref() == Some(name))
        .map(|(path, _)| path.clone())
}

fn create_worktree(project: &Path, name: &str, base: Option<&str>) -> Result<PathBuf> {
    let parent = project
        .parent()
        .context("El repositorio no tiene carpeta contenedora")?;
    let prefix = project
        .file_name()
        .context("El repositorio no tiene nombre")?
        .to_string_lossy();
    let suffix: String = name
        .chars()
        .take(48)
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ch == '-' {
                ch
            } else {
                '-'
            }
        })
        .collect();
    let stem = format!("{}-{}", prefix.chars().take(64).collect::<String>(), suffix);
    let mut destination = parent.join(&stem);
    let mut suffix = 2;
    while destination.symlink_metadata().is_ok() {
        destination = parent.join(format!("{stem}-{suffix}"));
        suffix += 1;
    }
    let mut args = vec![
        OsStr::new("worktree"),
        OsStr::new("add"),
        OsStr::new("--no-guess-remote"),
    ];
    let base_ref;
    if let Some(base) = base {
        base_ref = format!("refs/heads/{base}");
        args.extend([
            OsStr::new("-b"),
            OsStr::new(name),
            OsStr::new("--"),
            destination.as_os_str(),
            OsStr::new(&base_ref),
        ]);
    } else {
        args.extend([OsStr::new("--"), destination.as_os_str(), OsStr::new(name)]);
    }
    git(project, &args)?;
    Ok(destination.canonicalize()?)
}

fn scan_projects(container: &Path, added: &[Repository]) -> Result<Vec<Repository>> {
    let mut found = BTreeMap::new();
    let mut pending = vec![container.to_path_buf()];
    while let Some(path) = pending.pop() {
        let entries = match fs::read_dir(&path) {
            Ok(entries) => entries,
            Err(error) if path == container => {
                return Err(error).with_context(|| format!("No se pudo abrir {}", path.display()));
            }
            Err(_) => continue,
        };
        if path.join(".git").exists()
            && let Ok(project) = repository(&path)
        {
            found.insert(project.root.clone(), project);
        }
        pending.extend(entries.flatten().filter_map(|entry| {
            (entry.file_name() != ".git" && entry.file_type().is_ok_and(|kind| kind.is_dir()))
                .then(|| entry.path())
        }));
    }
    for project in added {
        found.remove(&project.root);
    }
    Ok(found.into_values().collect())
}

impl WorkspaceState {
    fn add(&mut self, project: Repository) {
        if let Some(existing) = self
            .projects
            .iter_mut()
            .find(|item| item.root == project.root)
        {
            *existing = project;
        } else {
            self.projects.push(project);
        }
    }
}

fn dispatch(
    terminals: &mut Vec<Result<Terminal, Snapshot>>,
    layout: &mut Layout,
    project_dir: &str,
    mut workspace: Option<&mut WorkspaceState>,
    request: Request,
) -> Result<Reply> {
    match request {
        Request::Poll(seen) => {
            let mut panes = Vec::with_capacity(terminals.len());
            for (pane, terminal) in terminals.iter_mut().enumerate() {
                let revision = seen.get(pane).copied().unwrap_or_default();
                panes.push(match terminal {
                    Ok(terminal) => terminal.snapshot(revision)?,
                    Err(snapshot) => (snapshot.revision > revision).then(|| snapshot.clone()),
                });
            }
            return Ok(Reply::State {
                layout: *layout,
                desktop_layout: workspace.as_ref().map(|state| state.layout.clone()),
                project_dir: project_dir.into(),
                panes,
                projects: workspace.as_ref().map_or_else(Vec::new, |state| {
                    state
                        .projects
                        .iter()
                        .map(|project| project.root.clone())
                        .collect()
                }),
                worktrees: workspace.as_ref().map_or_else(BTreeMap::new, |state| {
                    state
                        .projects
                        .iter()
                        .map(|project| (project.root.clone(), project.worktrees.clone()))
                        .collect()
                }),
                agent_executables: workspace
                    .as_ref()
                    .map_or_else(BTreeMap::new, |state| state.agent_executables.clone()),
                terminal_projects: workspace
                    .as_ref()
                    .map_or_else(Vec::new, |state| state.terminal_projects.clone()),
            });
        }
        Request::Input { pane, bytes } => terminals
            .get_mut(pane)
            .context("unknown terminal")?
            .as_mut()
            .map_err(|snapshot| anyhow::anyhow!("{} has exited", snapshot.label))?
            .input(&bytes)?,
        Request::Resize { pane, rows, cols } => {
            if let Ok(terminal) = terminals.get_mut(pane).context("unknown terminal")? {
                terminal.resize(rows, cols)?;
            }
        }
        Request::SetLayout(position) => {
            *layout = Layout {
                left: position.left.clamp(0.15, 0.85),
                top: position.top.clamp(0.15, 0.85),
            };
        }
        Request::Panel(action) => {
            let state = workspace.context("workspace action requires the desktop host")?;
            match action {
                PanelAction::Split { pane, axis } => state.layout.split(pane, axis)?,
                PanelAction::Resize { path, ratio } => state.layout.resize(&path, ratio)?,
                PanelAction::Show { pane } => {
                    ensure!(pane < terminals.len(), "Sesión desconocida");
                    state.layout.show(pane)?;
                }
                PanelAction::Hide { pane } => state.layout.hide(pane)?,
                PanelAction::Dismiss { path } => state.layout.dismiss(&path)?,
                PanelAction::Close { pane } => {
                    ensure!(
                        state.layout.root.contains(pane),
                        "El panel ya no está visible"
                    );
                    if let Ok(terminal) = &mut terminals[pane] {
                        terminals[pane] = Err(terminal.close()?);
                    }
                    state.layout.hide(pane)?;
                }
            }
        }
        Request::DesktopReady => ensure!(workspace.is_some(), "expected the desktop host"),
        Request::AddProject(path) => {
            let state = workspace
                .as_mut()
                .context("workspace action requires the desktop host")?;
            state.add(repository(&path)?);
        }
        Request::ScanProjects(path) => {
            let state = workspace.context("workspace action requires the desktop host")?;
            return Ok(Reply::Discovered(scan_projects(&path, &state.projects)?));
        }
        Request::AddDiscoveredProjects(paths) => {
            let state = workspace.context("workspace action requires the desktop host")?;
            // Resolve all selections before updating the workspace.
            let projects = paths
                .iter()
                .map(|path| repository(path))
                .collect::<Result<Vec<_>>>()?;
            for project in projects {
                state.add(project);
            }
        }
        Request::SetAgentExecutable { agent, path } => {
            let state = workspace
                .as_mut()
                .context("workspace action requires the desktop host")?;
            if let Some(path) = path {
                ensure!(
                    path.is_absolute() && is_executable(&path),
                    "Selecciona un ejecutable de {}",
                    agent.name()
                );
                state.agent_executables.insert(agent, path);
            } else {
                state.agent_executables.remove(&agent);
            }
        }
        Request::SessionDirectories(project) => {
            let state = workspace.context("workspace action requires the desktop host")?;
            let added = state
                .projects
                .iter_mut()
                .find(|item| item.root == project)
                .context("Añade el repositorio primero")?;
            let directories = session_directories(&project)?;
            added.worktrees.clone_from(&directories.worktrees);
            return Ok(Reply::Directories(directories));
        }
        Request::StartSession {
            project,
            agent,
            target,
        } => {
            let state = workspace
                .as_mut()
                .context("workspace action requires the desktop host")?;
            ensure!(
                state.projects.iter().any(|item| item.root == project),
                "Añade el repositorio antes de iniciar {}",
                agent.name()
            );
            let program = state
                .agent_executables
                .get(&agent)
                .cloned()
                .or_else(|| detect_agent(agent))
                .with_context(|| {
                    format!(
                        "No se encontró {}. Elige su ejecutable desde la barra lateral.",
                        agent.name()
                    )
                })?;
            let directory = match &target {
                SessionTarget::Main => project.clone(),
                SessionTarget::Existing(path) => {
                    let path = path.canonicalize()?;
                    ensure!(
                        path == project || repository(&project)?.worktrees.contains(&path),
                        "El directorio no pertenece a los worktrees del proyecto"
                    );
                    path
                }
                SessionTarget::Branch(name) | SessionTarget::NewBranch { name, base: _ } => {
                    let base = match &target {
                        SessionTarget::NewBranch { base, .. } => Some(base.as_str()),
                        _ => None,
                    };
                    let directories = session_directories(&project)?;
                    let branch = directories
                        .branches
                        .iter()
                        .find(|branch| branch.name == *name);
                    if let Some(path) = branch.and_then(|branch| branch.occupied.clone()) {
                        return Ok(Reply::BranchOccupied(path));
                    }
                    if let Some(base) = base {
                        ensure!(branch.is_none(), "La rama {name} ya existe");
                        ensure!(
                            directories
                                .branches
                                .iter()
                                .any(|branch| branch.name == base),
                            "La rama base {base} ya no existe"
                        );
                        git(
                            &project,
                            &["check-ref-format", &format!("refs/heads/{name}")],
                        )?;
                    } else {
                        ensure!(branch.is_some(), "La rama {name} ya no existe");
                    }
                    let created = match create_worktree(&project, name, base) {
                        Ok(path) => path,
                        Err(error) => {
                            if let Ok(current) = checkouts(&project)
                                && let Some(path) = occupied_checkout(&current, name)
                            {
                                return Ok(Reply::BranchOccupied(path));
                            }
                            return Err(error);
                        }
                    };
                    state.add(repository(&project)?);
                    created
                }
            };
            let mut terminal =
                Terminal::new(program.as_os_str(), true, &directory).with_context(|| {
                    format!(
                        "No se pudo iniciar {} desde {}",
                        agent.name(),
                        program.display()
                    )
                })?;
            terminal.agent = Some(agent);
            state.layout.assign(terminals.len());
            terminals.push(Ok(terminal));
            state.terminal_projects.push(project);
        }
        Request::Stop => {}
    }
    Ok(Reply::Ok)
}

fn detect_agent(agent: Agent) -> Option<PathBuf> {
    let path = env::var_os("PATH").unwrap_or_default();
    let directories = env::split_paths(&path);

    #[cfg(unix)]
    let directories = directories
        .chain(dirs::home_dir().map(|home| home.join(".local/bin")))
        .chain([
            PathBuf::from("/opt/homebrew/bin"),
            PathBuf::from("/usr/local/bin"),
        ]);
    #[cfg(windows)]
    let directories =
        directories.chain(dirs::data_local_dir().map(|local| local.join("Microsoft/WinGet/Links")));

    #[cfg(windows)]
    let names = ["exe", "cmd", "bat"].map(|ext| format!("{}.{ext}", agent.executable()));
    #[cfg(unix)]
    let names = [agent.executable()];

    directories
        .filter(|directory| directory.is_absolute())
        .flat_map(|directory| names.iter().map(move |name| directory.join(name)))
        .find(|candidate| is_executable(candidate))
}

fn is_executable(path: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        path.metadata()
            .is_ok_and(|metadata| metadata.is_file() && metadata.permissions().mode() & 0o111 != 0)
    }
    #[cfg(windows)]
    {
        path.is_file()
            && path.extension().is_some_and(|ext| {
                ["exe", "cmd", "bat"]
                    .iter()
                    .any(|kind| ext.eq_ignore_ascii_case(kind))
            })
    }
}

fn unavailable_agent(program: &std::ffi::OsStr, error: &anyhow::Error) -> Snapshot {
    let message = error.to_string();
    Snapshot {
        revision: 1,
        label: program.to_string_lossy().into_owned(),
        exited: true,
        lines: vec![vec![Run {
            cells: message.chars().count() as u16,
            text: message,
            fg: 0xd9e0ee,
            bg: 0x11151d,
            bold: false,
            underline: false,
        }]],
        ..Default::default()
    }
}

struct Screen {
    parser: vt100::Parser,
    revision: u64,
}

struct Terminal {
    master: Box<dyn MasterPty + Send>,
    writer: Box<dyn Write + Send>,
    child: Box<dyn Child + Send + Sync>,
    pid: Option<u32>,
    label: String,
    agent: Option<Agent>,
    directory: PathBuf,
    screen: Arc<Mutex<Screen>>,
    exited: bool,
}

impl Terminal {
    fn new(program: &std::ffi::OsStr, agent: bool, cwd: &Path) -> Result<Self> {
        let rows = 24;
        let cols = 80;
        let pair = native_pty_system().openpty(pty_size(rows, cols))?;
        let label = PathBuf::from(program)
            .file_name()
            .unwrap_or(program)
            .to_string_lossy()
            .into_owned();
        let direct_exe = Path::new(program)
            .extension()
            .is_some_and(|ext| ext.eq_ignore_ascii_case("exe"));
        let mut command = if cfg!(windows) && agent && !direct_exe {
            // Native npm installations can expose an agent as a .cmd shim.
            // Never interpret a user-selected path as shell syntax.
            ensure!(
                !program
                    .to_string_lossy()
                    .contains(['&', '|', '<', '>', '^', '%', '!', '(', ')', '"', '\n', '\r']),
                "La ruta del script no es compatible con cmd.exe"
            );
            let mut command = CommandBuilder::new(shell_program());
            command.args([
                OsStr::new("/d"),
                OsStr::new("/v:off"),
                OsStr::new("/c"),
                program,
            ]);
            command
        } else {
            CommandBuilder::new(program)
        };
        command.cwd(cwd);
        command.env("TERM", "xterm-256color");
        let child = pair
            .slave
            .spawn_command(command)
            .with_context(|| format!("could not start {label}"))?;
        drop(pair.slave);
        let pid = child.process_id();
        let mut reader = pair.master.try_clone_reader()?;
        let writer = pair.master.take_writer()?;
        let screen = Arc::new(Mutex::new(Screen {
            parser: vt100::Parser::new(rows, cols, 512),
            revision: 1,
        }));
        let screen_reader = Arc::clone(&screen);
        thread::spawn(move || {
            let mut buffer = [0; 8192];
            while let Ok(count) = reader.read(&mut buffer) {
                if count == 0 {
                    break;
                }
                let mut screen = screen_reader.lock().unwrap();
                screen.parser.process(&buffer[..count]);
                screen.revision += 1;
            }
        });
        Ok(Self {
            master: pair.master,
            writer,
            child,
            pid,
            label,
            agent: None,
            directory: cwd.into(),
            screen,
            exited: false,
        })
    }

    fn close(&mut self) -> Result<Snapshot> {
        if !self.exited {
            if self.child.try_wait()?.is_none() {
                self.child.kill()?;
                self.child.wait()?;
            }
            self.exited = true;
        }
        self.screen.lock().unwrap().revision += 1;
        let mut snapshot = self
            .snapshot(0)?
            .context("Falta la salida de la terminal")?;
        snapshot.closed_by_user = true;
        Ok(snapshot)
    }

    fn input(&mut self, bytes: &[u8]) -> Result<()> {
        ensure!(!self.exited, "terminal has exited");
        self.writer.write_all(bytes)?;
        self.writer.flush()?;
        Ok(())
    }

    fn resize(&mut self, rows: u16, cols: u16) -> Result<()> {
        let rows = rows.clamp(2, 200);
        let cols = cols.clamp(2, 300);
        let mut screen = self.screen.lock().unwrap();
        if screen.parser.screen().size() != (rows, cols) && !self.exited {
            self.master.resize(pty_size(rows, cols))?;
            screen.parser.set_size(rows, cols);
            screen.revision += 1;
        }
        Ok(())
    }

    fn snapshot(&mut self, seen: u64) -> Result<Option<Snapshot>> {
        let exited_now = !self.exited && self.child.try_wait()?.is_some();
        let mut state = self.screen.lock().unwrap();
        if exited_now {
            self.exited = true;
            state.revision += 1;
        }
        if state.revision <= seen {
            return Ok(None);
        }
        let screen = state.parser.screen();
        let (rows, cols) = screen.size();
        let cursor = (!self.exited && !screen.hide_cursor()).then(|| screen.cursor_position());
        let lines = (0..rows)
            .map(|row| {
                let mut runs: Vec<Run> = Vec::new();
                for col in 0..cols {
                    let Some(cell) = screen.cell(row, col) else {
                        continue;
                    };
                    if cell.is_wide_continuation() {
                        continue;
                    }
                    let mut fg = color(cell.fgcolor(), 0xd9e0ee);
                    let mut bg = color(cell.bgcolor(), 0x11151d);
                    if cell.inverse() ^ (cursor == Some((row, col))) {
                        std::mem::swap(&mut fg, &mut bg);
                    }
                    let bold = cell.bold();
                    let underline = cell.underline();
                    let contents = cell.contents();
                    let text = if contents.is_empty() { " " } else { &contents };
                    let cells = if cell.is_wide() { 2 } else { 1 };
                    match runs.last_mut() {
                        Some(run)
                            if (fg, bg, bold, underline)
                                == (run.fg, run.bg, run.bold, run.underline) =>
                        {
                            run.text.push_str(text);
                            run.cells += cells;
                        }
                        _ => runs.push(Run {
                            text: text.into(),
                            cells,
                            fg,
                            bg,
                            bold,
                            underline,
                        }),
                    }
                }
                runs
            })
            .collect();
        Ok(Some(Snapshot {
            revision: state.revision,
            label: self.label.clone(),
            agent: self.agent,
            directory: self.directory.clone(),
            pid: self.pid,
            exited: self.exited,
            closed_by_user: false,
            application_cursor: screen.application_cursor(),
            bracketed_paste: screen.bracketed_paste(),
            lines,
        }))
    }
}

impl Drop for Terminal {
    fn drop(&mut self) {
        if !self.exited {
            _ = self.child.kill();
            _ = self.child.wait();
        }
    }
}

fn pty_size(rows: u16, cols: u16) -> PtySize {
    PtySize {
        rows,
        cols,
        pixel_width: 0,
        pixel_height: 0,
    }
}

fn color(color: vt100::Color, default: u32) -> u32 {
    match color {
        vt100::Color::Default => default,
        vt100::Color::Rgb(red, green, blue) => {
            (u32::from(red) << 16) | (u32::from(green) << 8) | u32::from(blue)
        }
        vt100::Color::Idx(index) => {
            const ANSI: [u32; 16] = [
                0x171a21, 0xe56e78, 0x8bbc8a, 0xe6bd73, 0x799ee7, 0xb991df, 0x73c1d3, 0xc9ced7,
                0x6d7481, 0xff8790, 0xa1d9a1, 0xffd28d, 0x96b7ff, 0xd8b0fa, 0x8fdced, 0xf3f5fa,
            ];
            match index {
                0..=15 => ANSI[index as usize],
                16..=231 => {
                    let index = u32::from(index - 16);
                    let level = |n| if n == 0 { 0 } else { 55 + 40 * n };
                    (level(index / 36) << 16) | (level(index / 6 % 6) << 8) | level(index % 6)
                }
                _ => u32::from(8 + (index - 232) * 10) * 0x010101,
            }
        }
    }
}
