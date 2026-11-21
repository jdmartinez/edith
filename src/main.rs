mod host;
#[cfg(feature = "ui")]
mod ui;

use std::env;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{Result, bail, ensure};
use host::{Endpoint, Layout, PANE_COUNT, Reply, Request, Snapshot};

fn main() -> Result<()> {
    let args: Vec<_> = env::args().skip(1).collect();
    let endpoint = Endpoint::find()?;
    match args
        .iter()
        .map(String::as_str)
        .collect::<Vec<_>>()
        .as_slice()
    {
        ["--host"] => endpoint.serve(false),
        ["--host", "--agents"] => endpoint.serve(true),
        ["--stop-host"] => {
            endpoint.request(Request::Stop)?;
            Ok(())
        }
        ["--status"] => {
            let (layout, panes) = state(&endpoint)?;
            println!("Layout: {layout:?}");
            for pane in panes {
                println!("{} PID {:?}; exited: {}", pane.label, pane.pid, pane.exited);
            }
            Ok(())
        }
        ["--smoke-test"] => smoke_test(),
        [] | ["--agents"] => {
            #[cfg(feature = "ui")]
            return ui::run(endpoint, !args.is_empty());
            #[cfg(not(feature = "ui"))]
            bail!("rebuild with the default ui feature to open a window");
        }
        _ => {
            bail!("usage: edith-gpui-prototype [--agents | --status | --stop-host | --smoke-test]")
        }
    }
}

fn state(endpoint: &Endpoint) -> Result<(Layout, [Snapshot; PANE_COUNT])> {
    let Reply::State { layout, panes, .. } = endpoint.request(Request::Poll([0; PANE_COUNT]))?
    else {
        bail!("host did not return terminal state");
    };
    ensure!(
        panes.iter().all(Option::is_some),
        "host did not return every pane"
    );
    Ok((layout, panes.map(Option::unwrap)))
}

fn smoke_test() -> Result<()> {
    let nonce = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos();
    let path = env::temp_dir().join(format!("edith-{}-{nonce:x}", std::process::id()));
    // Keep the local socket path within macOS's sockaddr_un length limit.
    #[cfg(target_os = "macos")]
    let path = std::path::Path::new("/tmp").join(path.file_name().unwrap());
    let endpoint = Endpoint::at(path);
    endpoint.ensure_host(false, &env::current_dir()?)?;
    let result = exercise_shells(&endpoint);
    let stop = endpoint.request(Request::Stop);
    result?;
    stop?;
    println!(
        "Verified three real PTYs, keyboard input, resize and reconnection with identical PIDs."
    );
    Ok(())
}

fn exercise_shells(endpoint: &Endpoint) -> Result<()> {
    let (_, before) = state(endpoint)?;
    let pids = before.map(|pane| pane.pid);
    ensure!(
        pids.iter().all(Option::is_some),
        "host did not return shell PIDs"
    );
    ensure!(
        pids[0] != pids[1] && pids[1] != pids[2] && pids[0] != pids[2],
        "panes share a shell"
    );

    let split = Layout {
        left: 0.63,
        top: 0.37,
    };
    endpoint.request(Request::SetLayout(split))?;
    for pane in 0..PANE_COUNT {
        endpoint.request(Request::Resize {
            pane,
            rows: 18,
            cols: 60,
        })?;
        endpoint.request(Request::Input {
            pane,
            bytes: shell_probe(pane).into_bytes(),
        })?;
    }

    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let (layout, panes) = state(endpoint)?;
        ensure!(layout == split, "reconnected client did not recover layout");
        let pids_after = panes.each_ref().map(|pane| pane.pid);
        ensure!(pids == pids_after, "reconnection replaced a shell");
        ensure!(
            panes.iter().all(|pane| pane.lines.len() == 18),
            "PTY resize was not applied"
        );
        if panes.iter().enumerate().all(|(pane, snapshot)| {
            let marker = format!("EDITH_PTY_{pane} 18 60");
            snapshot.lines.iter().any(|line| {
                line.iter()
                    .map(|run| run.text.as_str())
                    .collect::<String>()
                    .trim()
                    == marker
            })
        }) {
            println!("PTY PIDs: {pids:?}; each shell reported 18 rows and 60 columns");
            return Ok(());
        }
        ensure!(
            Instant::now() < deadline,
            "one or more shells did not return the expected output"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn shell_probe(pane: usize) -> String {
    #[cfg(unix)]
    return format!("printf '%s%s %s\\n' EDITH_ PTY_{pane} \"$(stty size | xargs)\"\r");
    #[cfg(windows)]
    return format!(
        "powershell -NoProfile -Command \"'EDITH_' + 'PTY_{pane} ' + [Console]::WindowHeight + ' ' + [Console]::WindowWidth\"\r"
    );
}
