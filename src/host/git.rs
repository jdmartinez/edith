use std::collections::BTreeMap;
use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Repository {
    pub root: PathBuf,
    pub worktrees: Vec<PathBuf>,
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

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorktreeRemoval {
    pub project: PathBuf,
    pub path: PathBuf,
    pub branch: Option<String>,
    pub changes: Vec<String>,
    pub active_sessions: Vec<usize>,
    head: Vec<u8>,
    git_dir: Vec<u8>,
}

fn git(path: &Path, args: &[impl AsRef<OsStr>]) -> Result<Vec<u8>> {
    let output = Command::new("git")
        .args(args)
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_COMMON_DIR")
        .env_remove("GIT_INDEX_FILE")
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

pub(super) fn repository(path: &Path) -> Result<Repository> {
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
pub(super) fn checkouts(project: &Path) -> Result<Vec<(PathBuf, Option<String>)>> {
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

pub(super) fn inspect_worktree_removal(
    project: &Path,
    path: &Path,
    active_sessions: impl FnOnce(&Path) -> Result<Vec<usize>>,
) -> Result<WorktreeRemoval> {
    let path = path.canonicalize()?;
    let mut registered = checkouts(project)?.into_iter();
    let (main, _) = registered.next().context("Falta el directorio principal")?;
    ensure!(
        main == project && path != main,
        "El directorio principal no se puede retirar"
    );
    let (_, branch) = registered
        .find(|(checkout, _)| *checkout == path)
        .context("El directorio no es un worktree de este proyecto")?;
    let common_dir = ["rev-parse", "--path-format=absolute", "--git-common-dir"];
    ensure!(
        git(project, &common_dir)? == git(&path, &common_dir)?,
        "El directorio ya no pertenece a este repositorio"
    );
    let git_dir = git(&path, &["rev-parse", "--absolute-git-dir"])?;
    let head = git(&path, &["rev-parse", "--verify", "HEAD"])?;
    // Porcelain quotes unusual paths and emits one entry per line.
    // Ignored files can also be lost when Git removes the directory.
    let status = git(
        &path,
        &[
            "-c",
            "core.quotePath=true",
            "status",
            "--porcelain=v1",
            "--untracked-files=all",
            "--ignored",
            "--ignore-submodules=none",
            "--no-renames",
        ],
    )?;
    let active_sessions = active_sessions(&path)?;
    Ok(WorktreeRemoval {
        project: project.into(),
        path,
        branch,
        changes: String::from_utf8_lossy(&status)
            .lines()
            .map(str::to_owned)
            .collect(),
        active_sessions,
        head,
        git_dir,
    })
}

pub(super) fn delete_integrated_branch(removal: &WorktreeRemoval, branch: &str) -> Result<()> {
    let project = &removal.project;
    let reference = format!("refs/heads/{branch}");
    ensure!(
        git(project, &["rev-parse", "--verify", &reference])? == removal.head,
        "La rama ha cambiado"
    );
    // Check the principal checkout even when the branch has an upstream.
    git(
        project,
        &["merge-base", "--is-ancestor", &reference, "HEAD"],
    )?;
    git(project, &["branch", "-d", "--", branch])?;
    Ok(())
}

pub(super) fn session_directories(project: &Path) -> Result<SessionDirectories> {
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

pub(super) fn occupied_checkout(
    checkouts: &[(PathBuf, Option<String>)],
    name: &str,
) -> Option<PathBuf> {
    checkouts
        .iter()
        .find(|(_, branch)| branch.as_deref() == Some(name))
        .map(|(path, _)| path.clone())
}

pub(super) fn create_worktree(project: &Path, name: &str, base: Option<&str>) -> Result<PathBuf> {
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

pub(super) fn scan_projects(container: &Path, added: &[Repository]) -> Result<Vec<Repository>> {
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

pub(super) fn remove_worktree(removal: &WorktreeRemoval, force: bool) -> Result<()> {
    let mut arguments = vec![OsStr::new("worktree"), OsStr::new("remove")];
    if force {
        arguments.push(OsStr::new("--force"));
    }
    arguments.extend([OsStr::new("--"), removal.path.as_os_str()]);
    git(&removal.project, &arguments)?;
    Ok(())
}

pub(super) fn validate_branch_name(project: &Path, name: &str) -> Result<()> {
    git(
        project,
        &["check-ref-format", &format!("refs/heads/{name}")],
    )?;
    Ok(())
}
