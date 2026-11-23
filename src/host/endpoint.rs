use std::env;
use std::fs::{self, File, OpenOptions};
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;
use std::time::Duration;

use anyhow::{Context, Result, bail, ensure};
use interprocess::local_socket::{ListenerOptions, Name, Stream, prelude::*};
use serde::Serialize;

use super::protocol::{Agent, Layout, PANE_COUNT, Reply, Request};
use super::terminal::{Terminal, shell_program, unavailable_agent};
use super::workspace::{WorkspaceState, dispatch};

const MAX_REQUEST_BYTES: u64 = 256 * 1024;

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
                        desktop.then_some(&mut workspace),
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
