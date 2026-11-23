use std::env;
use std::ffi::OsStr;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::thread;

use anyhow::{Context, Result, ensure};
use portable_pty::{Child, CommandBuilder, MasterPty, PtySize, native_pty_system};

use super::protocol::{Agent, Run, Snapshot};

#[cfg(windows)]
mod conpty;

pub(super) fn shell_program() -> std::ffi::OsString {
    #[cfg(unix)]
    return env::var_os("SHELL").unwrap_or_else(|| "/bin/sh".into());
    #[cfg(windows)]
    return env::var_os("ComSpec").unwrap_or_else(|| "cmd.exe".into());
}

pub(super) fn detect_agent(agent: Agent) -> Option<PathBuf> {
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

pub(super) fn is_executable(path: &Path) -> bool {
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

pub(super) fn unavailable_agent(program: &std::ffi::OsStr, error: &anyhow::Error) -> Snapshot {
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

use super::attention::{self, AttentionStatus, ClaudeHook};

pub(super) struct Terminal {
    master: Box<dyn MasterPty + Send>,
    writer: PtyWriter,
    child: Box<dyn Child + Send + Sync>,
    pid: Option<u32>,
    label: String,
    agent: Option<Agent>,
    directory: PathBuf,
    screen: Arc<Mutex<Screen>>,
    exited: bool,
    resume_backup: Option<Snapshot>,
    claude_hook: Option<ClaudeHook>,
    attention: AttentionStatus,
    resume_error: Option<String>,
}

#[cfg(not(windows))]
type PtyWriter = Box<dyn Write + Send>;
#[cfg(windows)]
type PtyWriter = Arc<Mutex<Box<dyn Write + Send>>>;

impl Terminal {
    pub(super) fn link_codex_conversation(&mut self) -> Result<()> {
        ensure!(
            self.agent == Some(Agent::Codex),
            "Esta sesión no pertenece a Codex"
        );
        ensure!(!self.exited, "La sesión ya está cerrada");
        self.screen.lock().unwrap().revision += 1;
        Ok(())
    }

    pub(super) fn is_running_in(&mut self, path: &Path) -> Result<bool> {
        Ok(self.directory == path && !self.exited && self.child.try_wait()?.is_none())
    }

    pub(super) fn new(program: &std::ffi::OsStr, agent: bool, cwd: &Path) -> Result<Self> {
        Self::launch(program, agent, cwd, &[], 1, None)
    }

    pub(super) fn launch_agent(
        program: &OsStr,
        agent: Agent,
        cwd: &Path,
        args: &[&str],
        revision: u64,
        session: Option<uuid::Uuid>,
    ) -> Result<Self> {
        let hook = if agent == Agent::Claude && (!cfg!(windows) || is_exe(program)) {
            session.and_then(|session| ClaudeHook::create(session).ok())
        } else {
            None
        };
        let mut terminal = Self::launch(program, true, cwd, args, revision, hook)?;
        terminal.agent = Some(agent);
        Ok(terminal)
    }

    fn launch(
        program: &OsStr,
        agent: bool,
        cwd: &Path,
        args: &[&str],
        revision: u64,
        claude_hook: Option<ClaudeHook>,
    ) -> Result<Self> {
        let rows = 24;
        let cols = 80;
        let pair = native_pty_system().openpty(pty_size(rows, cols))?;
        let label = PathBuf::from(program)
            .file_name()
            .unwrap_or(program)
            .to_string_lossy()
            .into_owned();
        let mut command = if cfg!(windows) && agent && !is_exe(program) {
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
        command.args(args);
        if let Some(hook) = &claude_hook {
            command.arg("--settings");
            command.arg(hook.settings());
        }
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
        #[cfg(windows)]
        let writer = Arc::new(Mutex::new(writer));
        #[cfg(windows)]
        let query_writer = Arc::clone(&writer);
        let screen = Arc::new(Mutex::new(Screen {
            parser: vt100::Parser::new(rows, cols, 512),
            revision,
        }));
        let screen_reader = Arc::clone(&screen);
        thread::spawn(move || {
            let mut buffer = [0; 8192];
            #[cfg(windows)]
            let mut queries = conpty::Queries::default();
            while let Ok(count) = reader.read(&mut buffer) {
                if count == 0 {
                    break;
                }
                let mut screen = screen_reader.lock().unwrap();
                #[cfg(not(windows))]
                screen.parser.process(&buffer[..count]);
                #[cfg(windows)]
                let replies = queries.process(&mut screen.parser, &buffer[..count]);
                screen.revision += 1;
                #[cfg(windows)]
                {
                    drop(screen);
                    if !replies.is_empty()
                        && let Err(error) = query_writer.lock().unwrap().write_all(&replies)
                        && error.kind() != std::io::ErrorKind::BrokenPipe
                    {
                        eprintln!("could not answer a ConPTY query: {error}");
                    }
                }
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
            resume_backup: None,
            claude_hook,
            attention: AttentionStatus::default(),
            resume_error: None,
        })
    }

    pub(super) fn retain_on_resume_failure(&mut self, mut previous: Snapshot) {
        previous.resume_error = None;
        self.resume_backup = Some(previous);
    }

    pub(super) fn close(&mut self) -> Result<Snapshot> {
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

    pub(super) fn input(&mut self, bytes: &[u8]) -> Result<()> {
        ensure!(!self.exited, "terminal has exited");
        #[cfg(windows)]
        let mut writer = self.writer.lock().unwrap();
        #[cfg(not(windows))]
        let writer = &mut self.writer;
        writer.write_all(bytes)?;
        writer.flush()?;
        Ok(())
    }

    pub(super) fn resize(&mut self, rows: u16, cols: u16) -> Result<()> {
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

    pub(super) fn snapshot(&mut self, seen: u64) -> Result<Option<Snapshot>> {
        let exit = if self.exited {
            None
        } else {
            self.child.try_wait()?
        };
        let mut state = self.screen.lock().unwrap();
        if let Some(exit) = exit {
            self.exited = true;
            state.revision += 1;
            if exit.success() {
                self.resume_backup = None;
            } else if self.resume_backup.is_some() {
                self.resume_error =
                    Some(format!("El agente terminó al reanudar la sesión ({exit})."));
            }
        }
        let confirmed = self
            .claude_hook
            .as_mut()
            .and_then(|hook| hook.read_status().ok().flatten());
        let attention = if self.exited {
            AttentionStatus::closed()
        } else if let Some(status) = confirmed {
            status
        } else if state.revision > seen {
            self.agent.map_or_else(AttentionStatus::default, |agent| {
                attention::estimate(agent, &state.parser.screen().contents())
            })
        } else {
            self.attention
        };
        if attention != self.attention {
            self.attention = attention;
            state.revision = state.revision.saturating_add(1);
        }
        if state.revision <= seen {
            return Ok(None);
        }
        if let Some(error) = &self.resume_error
            && let Some(previous) = &self.resume_backup
        {
            return Ok(Some(Snapshot {
                revision: state.revision,
                resume_error: Some(error.clone()),
                attention,
                ..previous.clone()
            }));
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
            attention,
            label: self.label.clone(),
            agent: self.agent,
            directory: self.directory.clone(),
            pid: self.pid,
            exited: self.exited,
            closed_by_user: false,
            can_resume: false,
            resume_error: None,
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

fn is_exe(program: &OsStr) -> bool {
    Path::new(program)
        .extension()
        .is_some_and(|ext| ext.eq_ignore_ascii_case("exe"))
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
