#![cfg(unix)]

use std::{
    fs,
    io::{BufReader, Write},
    os::unix::{fs::PermissionsExt, net::UnixStream},
    path::PathBuf,
    process::{Child, Command},
    sync::atomic::{AtomicUsize, Ordering},
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use serde_json::{Value, json};

struct Host {
    child: Child,
    state: PathBuf,
}

impl Drop for Host {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = fs::remove_dir_all(&self.state);
    }
}

impl Host {
    fn request(&self, request: Value) -> Value {
        let mut socket = UnixStream::connect(self.state.join("host.sock")).unwrap();
        writeln!(socket, "{request}").unwrap();
        serde_json::from_reader(BufReader::new(socket)).unwrap()
    }
}

#[test]
fn codex_uses_the_repository_root_and_custom_executable_and_survives_reconnection() {
    exercise_agent("Codex", "codex");
}

#[test]
fn claude_uses_its_own_executable_and_keeps_its_identity() {
    exercise_agent("Claude", "claude");
}

#[test]
fn copilot_uses_its_own_executable_and_keeps_its_identity() {
    exercise_agent("Copilot", "copilot");
}

fn start_host() -> Host {
    static NEXT_HOST: AtomicUsize = AtomicUsize::new(0);
    let index = NEXT_HOST.fetch_add(1, Ordering::Relaxed);
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let state = PathBuf::from(format!(
        "/tmp/edith-test-{}-{nonce:x}-{index}",
        std::process::id()
    ));
    fs::create_dir(&state).unwrap();
    fs::set_permissions(&state, fs::Permissions::from_mode(0o700)).unwrap();
    let child = Command::new(env!("CARGO_BIN_EXE_edith-gpui-prototype"))
        .arg("--desktop-host")
        .env("EDITH_PROTOTYPE_STATE", &state)
        .env("PATH", format!("{}:/usr/bin:/bin", state.display()))
        .env("CODEX_HOME", "edith-test-home")
        .spawn()
        .unwrap();
    let mut host = Host { child, state };
    let deadline = Instant::now() + Duration::from_secs(5);
    while !host.state.join("host.sock").exists() {
        assert!(
            host.child.try_wait().unwrap().is_none(),
            "desktop host exited"
        );
        assert!(Instant::now() < deadline, "desktop host did not start");
        thread::sleep(Duration::from_millis(20));
    }

    host
}

fn exercise_agent(agent: &str, executable: &str) {
    let mut host = start_host();
    let repo = host.state.join("project");
    fs::create_dir_all(repo.join("subfolder")).unwrap();
    assert!(
        Command::new("git")
            .args(["init", "-q"])
            .arg(&repo)
            .status()
            .unwrap()
            .success()
    );
    let detected = host.state.join(executable);
    fs::write(&detected, "#!/bin/sh\necho DETECTED\n").unwrap();
    fs::set_permissions(&detected, fs::Permissions::from_mode(0o700)).unwrap();
    let custom = host.state.join("custom agent");
    fs::write(&custom, "#!/bin/sh\nprintf 'CUSTOM %s %s\\n' \"${PWD##*/}\" \"$CODEX_HOME\"\nread line\nprintf 'FINAL=%s\\n' \"$line\"\n").unwrap();
    fs::set_permissions(&custom, fs::Permissions::from_mode(0o700)).unwrap();

    assert_eq!(
        host.request(json!({"AddProject": repo.join("subfolder")})),
        "Ok"
    );
    let canonical = repo.canonicalize().unwrap();
    assert_eq!(
        host.request(json!({"SetAgentExecutable": {"agent": agent, "path": custom}})),
        "Ok"
    );
    assert_eq!(
        host.request(json!({"StartSession": {"agent": agent, "project": canonical}})),
        "Ok"
    );

    let first = host.request(json!({"Poll": []}));
    assert_eq!(first["State"]["agent_executables"], json!({agent: custom}),);
    let pid = first["State"]["panes"][0]["pid"].as_u64().unwrap();
    let marker = |pane: &Value| pane["lines"].to_string();
    assert_eq!(
        host.request(json!({"Input":{"pane":0,"bytes": b"hello\r"}})),
        "Ok"
    );
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let response = host.request(json!({"Poll": []}));
        let pane = &response["State"]["panes"][0];
        assert_eq!(pane["pid"], pid);
        assert_eq!(pane["agent"], agent);
        let output = marker(pane);
        if pane["exited"] == true && output.contains("FINAL=hello") {
            assert!(
                output.contains("CUSTOM project edith-test-home"),
                "{output}"
            );
            assert!(!output.contains("DETECTED"));
            break;
        }
        assert!(Instant::now() < deadline, "{response}");
        thread::sleep(Duration::from_millis(20));
    }
    assert_eq!(
        host.request(json!({"SetAgentExecutable": {"agent": agent, "path": null}})),
        "Ok"
    );
    assert_eq!(
        host.request(json!({"StartSession": {"agent": agent, "project": canonical}})),
        "Ok"
    );
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let response = host.request(json!({"Poll": []}));
        let pane = &response["State"]["panes"][1];
        if pane["exited"] == true && marker(pane).contains("DETECTED") {
            assert_eq!(pane["agent"], agent);
            assert_ne!(pane["pid"], pid);
            break;
        }
        assert!(Instant::now() < deadline, "{response}");
        thread::sleep(Duration::from_millis(20));
    }
    assert_eq!(host.request(json!("Stop")), "Ok");
    assert!(host.child.wait().unwrap().success());
}

#[test]
fn scanning_only_adds_selected_clones_and_preserves_running_sessions() {
    let host = start_host();
    let container = host.state.join("container");
    let first = container.join("a/clone");
    let second = container.join("b/clone");
    fs::create_dir_all(&first).unwrap();
    fs::create_dir_all(&second).unwrap();
    fs::create_dir(container.join("plain")).unwrap();

    fn git(directory: &std::path::Path, args: &[&str]) {
        assert!(
            Command::new("git")
                .current_dir(directory)
                .args(args)
                .status()
                .unwrap()
                .success()
        );
    }
    for clone in [&first, &second] {
        git(clone, &["init", "-q"]);
        git(
            clone,
            &[
                "-c",
                "user.name=Edith",
                "-c",
                "user.email=edith@example.com",
                "commit",
                "-qm",
                "initial",
                "--allow-empty",
            ],
        );
        git(
            clone,
            &["remote", "add", "origin", "https://example.com/shared.git"],
        );
    }
    git(
        &first,
        &["worktree", "add", "-q", "--detach", "../../first worktree"],
    );
    git(
        &second,
        &[
            "worktree",
            "add",
            "-q",
            "--detach",
            "../../../second worktree",
        ],
    );
    let first = first.canonicalize().unwrap();
    let second = second.canonicalize().unwrap();
    let first_tree = container.join("first worktree").canonicalize().unwrap();
    let second_tree = host.state.join("second worktree").canonicalize().unwrap();

    let results = host.request(json!({"ScanProjects": container}));
    assert_eq!(
        results,
        json!({"Discovered":[
            {"root":first,"worktrees":[first_tree]},
            {"root":second,"worktrees":[second_tree]}
        ]})
    );
    assert_eq!(
        host.request(json!({"Poll": []}))["State"]["projects"],
        json!([])
    );
    assert_eq!(
        host.request(json!({"AddDiscoveredProjects":[second]})),
        "Ok"
    );

    let codex = host.state.join("codex");
    fs::write(&codex, "#!/bin/sh\nread line\n").unwrap();
    fs::set_permissions(&codex, fs::Permissions::from_mode(0o700)).unwrap();
    assert_eq!(
        host.request(json!({"StartSession":{"project":second,"agent":"Codex"}})),
        "Ok"
    );
    let before = host.request(json!({"Poll": []}));
    assert_eq!(before["State"]["projects"], json!([second]));
    assert_eq!(
        before["State"]["worktrees"][second.to_str().unwrap()],
        json!([second_tree])
    );
    assert_eq!(before["State"]["terminal_projects"], json!([second]));

    let remaining = host.request(json!({"ScanProjects":container}));
    assert_eq!(
        remaining,
        json!({"Discovered":[{"root":first,"worktrees":[first_tree]}]})
    );
    assert_eq!(
        host.request(json!({"AddDiscoveredProjects":[first, first]})),
        "Ok"
    );
    let after = host.request(json!({"Poll": []}));
    assert_eq!(after["State"]["projects"], json!([second, first]));
    assert_eq!(
        after["State"]["worktrees"][first.to_str().unwrap()],
        json!([first_tree])
    );
    assert_eq!(after["State"]["terminal_projects"], json!([second]));
    assert_eq!(
        after["State"]["panes"][0]["pid"],
        before["State"]["panes"][0]["pid"]
    );
    assert_eq!(after["State"]["panes"][0]["exited"], false);
}

#[test]
fn sessions_keep_their_checkout_when_opening_or_creating_worktrees() {
    let host = start_host();
    let repo = host.state.join("project");
    let git = |directory: &std::path::Path, args: &[&str]| {
        let output = Command::new("git")
            .current_dir(directory)
            .args(args)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout).unwrap().trim().to_owned()
    };
    fs::create_dir(&repo).unwrap();
    let repo = repo.canonicalize().unwrap();
    git(&repo, &["init", "-q", "-b", "main"]);
    git(
        &repo,
        &[
            "-c",
            "user.name=Test",
            "-c",
            "user.email=test@example.com",
            "commit",
            "-qm",
            "base",
            "--allow-empty",
        ],
    );
    git(&repo, &["branch", "free"]);
    git(
        &repo,
        &[
            "-c",
            "user.name=Test",
            "-c",
            "user.email=test@example.com",
            "commit",
            "-qm",
            "main only",
            "--allow-empty",
        ],
    );
    let existing = repo.parent().unwrap().join("existing worktree");
    let program = host.state.join("codex");
    fs::write(
        &program,
        "#!/bin/sh\nread line\nprintf 'CHECKOUT=%s\\n' \"$(git branch --show-current)\"\n",
    )
    .unwrap();
    fs::set_permissions(&program, fs::Permissions::from_mode(0o700)).unwrap();
    assert_eq!(host.request(json!({"AddProject": repo})), "Ok");
    git(
        &repo,
        &["worktree", "add", "-qb", "busy", "../existing worktree"],
    );

    let start = |target: Value| {
        host.request(json!({"StartSession": {
            "project": repo, "agent": "Codex", "target": target
        }}))
    };
    let options = host.request(json!({"SessionDirectories": repo}));
    assert_eq!(options["Directories"]["worktrees"], json!([existing]));
    let branches = options["Directories"]["branches"].as_array().unwrap();
    assert!(branches.contains(&json!({"name":"busy","occupied":existing})));
    assert!(branches.contains(&json!({"name":"free","occupied":null})));

    assert_eq!(start(json!("Main")), "Ok");
    assert_eq!(
        start(json!({"Branch":"busy"})),
        json!({"BranchOccupied":existing})
    );
    assert!(start(json!({"Existing":host.state}))["Error"].is_string());
    assert_eq!(start(json!({"Existing":existing})), "Ok");
    assert_eq!(start(json!({"Branch":"free"})), "Ok");
    assert_eq!(
        start(json!({"NewBranch":{"name":"feature/new","base":"free"}})),
        "Ok"
    );

    let state = host.request(json!({"Poll":[]}));
    let panes = state["State"]["panes"].as_array().unwrap();
    assert_eq!(panes.len(), 4);
    assert_eq!(panes[0]["directory"], json!(repo));
    assert_eq!(panes[1]["directory"], json!(existing));
    let worktrees = state["State"]["worktrees"][repo.to_str().unwrap()]
        .as_array()
        .unwrap();
    assert_eq!(worktrees.len(), 3);
    for pane in &panes[1..] {
        assert!(worktrees.contains(&pane["directory"]));
    }
    assert_eq!(
        state["State"]["terminal_projects"],
        json!([repo, repo, repo, repo])
    );
    for (pane, branch) in panes.iter().zip(["main", "busy", "free", "feature/new"]) {
        let path = std::path::Path::new(pane["directory"].as_str().unwrap());
        assert_eq!(git(path, &["branch", "--show-current"]), branch);
    }
    assert_eq!(
        git(&repo, &["rev-parse", "free"]),
        git(&repo, &["rev-parse", "feature/new"])
    );
    assert_ne!(
        git(&repo, &["rev-parse", "main"]),
        git(&repo, &["rev-parse", "feature/new"])
    );

    assert_eq!(host.request(json!({"Input":{"pane":0,"bytes":[10]}})), "Ok");
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let state = host.request(json!({"Poll":[]}));
        let pane = &state["State"]["panes"][0];
        if pane["exited"] == true && pane["lines"].to_string().contains("CHECKOUT=main") {
            break;
        }
        assert!(Instant::now() < deadline);
        thread::sleep(Duration::from_millis(20));
    }
}

fn panel_host() -> (Host, PathBuf) {
    let host = start_host();
    let repo = host.state.join("panels");
    assert!(
        Command::new("git")
            .args(["init", "-q"])
            .arg(&repo)
            .status()
            .unwrap()
            .success()
    );
    let repo = repo.canonicalize().unwrap();
    let executable = host.state.join("codex");
    fs::write(
        &executable,
        "#!/bin/sh\nwhile read line; do printf 'RECEIVED:%s\\n' \"$line\"; done\n",
    )
    .unwrap();
    fs::set_permissions(executable, fs::Permissions::from_mode(0o700)).unwrap();
    assert_eq!(host.request(json!({"AddProject":repo})), "Ok");
    (host, repo)
}

fn open_panel(host: &Host, repo: &std::path::Path) {
    assert_eq!(
        host.request(json!({"StartSession":{"project":repo,"agent":"Codex"}})),
        "Ok"
    );
}

fn panel_action(host: &Host, action: Value) {
    assert_eq!(host.request(json!({"Panel":action})), "Ok");
}

#[test]
fn nested_panel_assignments_and_ratios_survive_client_reconnection() {
    let (host, repo) = panel_host();
    open_panel(&host, &repo);
    panel_action(&host, json!({"Split":{"pane":0,"axis":"Columns"}}));
    open_panel(&host, &repo);
    panel_action(&host, json!({"Split":{"pane":1,"axis":"Rows"}}));
    open_panel(&host, &repo);
    panel_action(&host, json!({"Resize":{"path":[],"ratio":0.625}}));
    panel_action(&host, json!({"Resize":{"path":[true],"ratio":0.375}}));
    let before = host.request(json!({"Poll":[]}));
    let after = host.request(json!({"Poll":[]}));
    assert_eq!(
        after["State"]["desktop_layout"]["root"],
        json!({
            "Split":{"axis":"Columns","ratio":0.625,"first":{"Pane":0},"second":{
                "Split":{"axis":"Rows","ratio":0.375,"first":{"Pane":1},"second":{"Pane":2}}
            }}
        })
    );
    for pane in 0..3 {
        let pid = &after["State"]["panes"][pane]["pid"];
        assert!(pid.is_number());
        assert_eq!(*pid, before["State"]["panes"][pane]["pid"]);
    }
}

#[test]
fn hiding_keeps_the_process_running_and_showing_restores_the_same_terminal() {
    let (host, repo) = panel_host();
    open_panel(&host, &repo);
    panel_action(&host, json!({"Split":{"pane":0,"axis":"Columns"}}));
    open_panel(&host, &repo);
    let before = host.request(json!({"Poll":[]}));
    let pid = &before["State"]["panes"][1]["pid"];

    panel_action(&host, json!({"Hide":{"pane":1}}));
    let hidden = host.request(json!({"Poll":[]}));
    assert_eq!(hidden["State"]["desktop_layout"]["root"], json!({"Pane":0}));
    assert_eq!(hidden["State"]["panes"][1]["exited"], false);
    assert_eq!(hidden["State"]["panes"][1]["pid"], *pid);
    assert_eq!(
        host.request(json!({"Input":{"pane":1,"bytes":b"hidden\r"}})),
        "Ok"
    );

    panel_action(&host, json!({"Show":{"pane":1}}));
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let shown = host.request(json!({"Poll":[]}));
        assert_eq!(shown["State"]["desktop_layout"]["focused"], 1);
        assert_eq!(
            shown["State"]["desktop_layout"]["root"]["Split"]["first"],
            json!({"Pane":0})
        );
        assert_eq!(
            shown["State"]["desktop_layout"]["root"]["Split"]["second"],
            json!({"Pane":1})
        );
        assert_eq!(shown["State"]["panes"][1]["pid"], *pid);
        if shown["State"]["panes"][1]["lines"]
            .to_string()
            .contains("RECEIVED:hidden")
        {
            break;
        }
        assert!(Instant::now() < deadline, "{shown}");
        thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn closing_a_panel_stops_its_terminal_and_keeps_the_other_panel_assigned() {
    let (host, repo) = panel_host();
    open_panel(&host, &repo);
    panel_action(&host, json!({"Split":{"pane":0,"axis":"Rows"}}));
    open_panel(&host, &repo);
    let before = host.request(json!({"Poll":[]}));
    let other_pid = &before["State"]["panes"][0]["pid"];

    panel_action(&host, json!({"Close":{"pane":1}}));
    let after = host.request(json!({"Poll":[]}));
    assert_eq!(after["State"]["desktop_layout"]["root"], json!({"Pane":0}));
    assert_eq!(after["State"]["desktop_layout"]["focused"], 0);
    assert_eq!(after["State"]["panes"][1]["exited"], true);
    assert_eq!(after["State"]["panes"][0]["pid"], *other_pid);
    assert_eq!(after["State"]["panes"][0]["exited"], false);
    assert!(host.request(json!({"Input":{"pane":1,"bytes":b"closed\r"}}))["Error"].is_string());
}
