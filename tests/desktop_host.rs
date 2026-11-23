#![cfg(unix)]

use std::{
    fs,
    io::{BufReader, Write},
    os::unix::{fs::PermissionsExt, net::UnixStream},
    path::{Path, PathBuf},
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

    fn restart(&mut self) {
        self.child.kill().unwrap();
        self.child.wait().unwrap();
        fs::remove_file(self.state.join("host.sock")).unwrap();
        self.child = spawn_host(&self.state);
        self.wait_until_ready();
    }

    fn wait_until_ready(&mut self) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while UnixStream::connect(self.state.join("host.sock")).is_err() {
            assert!(
                self.child.try_wait().unwrap().is_none(),
                "desktop host exited"
            );
            assert!(Instant::now() < deadline, "desktop host did not start");
            thread::sleep(Duration::from_millis(20));
        }
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
    let child = spawn_host(&state);
    let mut host = Host { child, state };
    host.wait_until_ready();
    host
}

fn spawn_host(state: &Path) -> Child {
    Command::new(env!("CARGO_BIN_EXE_edith-gpui-prototype"))
        .arg("--desktop-host")
        .env("EDITH_PROTOTYPE_STATE", state)
        .env("PATH", format!("{}:/usr/bin:/bin", state.display()))
        .env("CODEX_HOME", "edith-test-home")
        .spawn()
        .unwrap()
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

#[test]
fn worktree_removal_requires_review_and_keeps_unintegrated_branch() {
    let host = start_host();
    let repo = host.state.join("project");
    let tree = host.state.join("linked tree");
    fs::create_dir(&repo).unwrap();
    let git = |directory: &std::path::Path, arguments: &[&str]| {
        let output = Command::new("git")
            .current_dir(directory)
            .args([
                "-c",
                "user.name=Edith",
                "-c",
                "user.email=edith@example.com",
            ])
            .args(arguments)
            .output()
            .unwrap();
        assert!(output.status.success(), "{output:?}");
    };
    git(&repo, &["init", "-qb", "main"]);
    fs::write(repo.join("tracked"), "base").unwrap();
    git(&repo, &["add", "tracked"]);
    git(&repo, &["commit", "-qm", "base"]);
    git(
        &repo,
        &["worktree", "add", "-qb", "feature", tree.to_str().unwrap()],
    );
    git(&tree, &["commit", "-qm", "pending", "--allow-empty"]);
    // Git's upstream check alone would permit deletion of this branch.
    git(&tree, &["branch", "upstream"]);
    git(&tree, &["branch", "--set-upstream-to=upstream", "feature"]);
    let repo = repo.canonicalize().unwrap();
    let tree = tree.canonicalize().unwrap();
    assert_eq!(host.request(json!({"AddProject": repo})), "Ok");

    let inspect = |path: &std::path::Path| {
        host.request(json!({"InspectWorktreeRemoval":{"project":repo,"path":path}}))
    };
    let remove = |force, delete_branch| {
        host.request(json!({"RemoveWorktree":{"project":repo,"path":tree,"force":force,"delete_branch":delete_branch}}))
    };
    assert!(inspect(&repo).get("Error").is_some());
    assert!(remove(true, true).get("Error").is_some());
    fs::write(tree.join("tracked"), "modified").unwrap();
    fs::write(tree.join("untracked"), "local").unwrap();
    let preview = inspect(&tree);
    assert_eq!(
        preview["WorktreeRemoval"]["changes"],
        json!([" M tracked", "?? untracked"]),
        "{preview}"
    );
    assert!(remove(false, true).get("WorktreeRemoval").is_some());
    fs::write(tree.join("new file"), "created after review").unwrap();
    assert!(remove(true, true).get("WorktreeRemoval").is_some());
    assert!(tree.is_dir());
    let removed = remove(true, true);
    assert_eq!(
        removed["WorktreeRemoved"]["retained_branch"], "feature",
        "{removed}"
    );
    assert!(!tree.exists());
    git(&repo, &["show-ref", "--verify", "refs/heads/feature"]);
    assert_eq!(fs::read_to_string(repo.join("tracked")).unwrap(), "base");
    assert_eq!(
        host.request(json!({"Poll": []}))["State"]["worktrees"][repo.to_str().unwrap()],
        json!([])
    );
}

#[test]
fn worktree_removal_rechecks_hidden_sessions_and_allows_branch_choice() {
    let (host, repo) = panel_host();
    let tree = host.state.join("linked");
    let git = |arguments: &[&str]| {
        Command::new("git")
            .current_dir(&repo)
            .args([
                "-c",
                "user.name=Edith",
                "-c",
                "user.email=edith@example.com",
            ])
            .args(arguments)
            .output()
            .unwrap()
            .status
            .success()
    };
    assert!(git(&["commit", "-qm", "base", "--allow-empty"]));
    assert!(git(&[
        "worktree",
        "add",
        "-qb",
        "keep",
        tree.to_str().unwrap()
    ]));
    let tree = tree.canonicalize().unwrap();
    let inspect = || host.request(json!({"InspectWorktreeRemoval":{"project":repo,"path":tree}}));
    let remove = |delete_branch| {
        host.request(
        json!({"RemoveWorktree":{"project":repo,"path":tree,"force":false,"delete_branch":delete_branch}}),
    )
    };
    assert_eq!(inspect()["WorktreeRemoval"]["active_sessions"], json!([]));
    open_panel(&host, &repo);
    panel_action(&host, json!({"Split":{"pane":0,"axis":"Columns"}}));
    assert_eq!(
        host.request(
            json!({"StartSession":{"project":repo,"agent":"Codex","target":{"Existing":tree}}})
        ),
        "Ok"
    );
    panel_action(&host, json!({"Hide":{"pane":1}}));
    assert_eq!(
        remove(false)["WorktreeRemoval"]["active_sessions"],
        json!([1])
    );
    assert!(tree.exists());
    panel_action(&host, json!({"Show":{"pane":1}}));
    panel_action(&host, json!({"Close":{"pane":1}}));
    assert_eq!(inspect()["WorktreeRemoval"]["active_sessions"], json!([]));
    assert_eq!(
        remove(false),
        json!({"WorktreeRemoved":{"retained_branch":null}})
    );
    assert!(git(&["show-ref", "--verify", "refs/heads/keep"]));

    assert!(git(&[
        "worktree",
        "add",
        "-qb",
        "delete",
        tree.to_str().unwrap()
    ]));
    assert_eq!(inspect()["WorktreeRemoval"]["branch"], "delete");
    assert_eq!(
        remove(true),
        json!({"WorktreeRemoved":{"retained_branch":null}})
    );
    assert!(!git(&["show-ref", "--verify", "refs/heads/delete"]));
    assert_eq!(
        host.request(json!({"Poll": []}))["State"]["panes"][0]["exited"],
        false
    );
}

#[test]
fn a_failed_resume_keeps_closed_output_and_retries_the_same_conversation() {
    let host = start_host();
    let repo = host.state.join("project");
    fs::create_dir(&repo).unwrap();
    assert!(
        Command::new("git")
            .arg("init")
            .arg("-q")
            .current_dir(&repo)
            .status()
            .unwrap()
            .success()
    );
    let repo = repo.canonicalize().unwrap();
    let executable = host.state.join("claude-fixture");
    fs::write(
        &executable,
        "#!/bin/sh\ncase \"$1\" in\n--session-id) printf '%s\\n' \"$2\" > conversation; echo CLOSED_OUTPUT; read line;;\n--resume) test \"$2\" = \"$(cat conversation)\" || exit 7; test -f resume-enabled || exit 8; echo RESUMED_OUTPUT; read line;;\n*) exit 9;;\nesac\n",
    ).unwrap();
    fs::set_permissions(&executable, fs::Permissions::from_mode(0o700)).unwrap();
    assert_eq!(host.request(json!({"AddProject": repo})), "Ok");
    assert_eq!(
        host.request(json!({"SetAgentExecutable": {"agent": "Claude", "path": executable}})),
        "Ok"
    );
    assert_eq!(
        host.request(json!({"StartSession": {"agent": "Claude", "project": repo}})),
        "Ok"
    );

    let await_pane = |predicate: &dyn Fn(&Value) -> bool| {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let state = host.request(json!({"Poll": []}));
            let pane = &state["State"]["panes"][0];
            if predicate(pane) {
                return pane.clone();
            }
            assert!(Instant::now() < deadline, "{pane}");
            thread::sleep(Duration::from_millis(20));
        }
    };

    let before = await_pane(&|pane| pane["lines"].to_string().contains("CLOSED_OUTPUT"));
    assert_eq!(before["can_resume"], true);
    assert!(
        uuid::Uuid::parse_str(
            fs::read_to_string(repo.join("conversation"))
                .unwrap()
                .trim()
        )
        .is_ok()
    );
    panel_action(&host, json!({"Close": {"pane": 0}}));
    let closed = await_pane(&|pane| pane["exited"] == true);
    let resume = || host.request(json!({"ResumeSession": {"pane": 0}}));
    assert_eq!(resume(), "Ok");
    let failed = await_pane(&|pane| pane["resume_error"].is_string());
    assert_eq!(failed["exited"], true);
    assert!(failed["lines"].to_string().contains("CLOSED_OUTPUT"));
    assert!(failed["revision"].as_u64() > closed["revision"].as_u64());

    fs::write(repo.join("resume-enabled"), "").unwrap();
    assert_eq!(resume(), "Ok");
    let resumed = await_pane(&|pane| pane["lines"].to_string().contains("RESUMED_OUTPUT"));
    assert_eq!(resumed["agent"], "Claude");
    assert_eq!(resumed["exited"], false);
    assert!(resumed["resume_error"].is_null());
    assert!(resumed["revision"].as_u64() > failed["revision"].as_u64());
    assert_ne!(before["pid"], resumed["pid"]);
    assert_eq!(
        host.request(json!({"Poll": []}))["State"]["desktop_layout"]["root"],
        json!({"Pane": 0})
    );
}

#[test]
fn restarting_the_host_restores_hidden_sessions_and_keeps_failed_retries_separate() {
    let (mut host, repo) = panel_host();
    let executable = host.state.join("agent-fixture");
    fs::write(
        &executable,
        r#"#!/bin/sh
case "$1" in
--session-id) printf '%s\n' "$2" > conversation; echo ORIGINAL; read line;;
--resume)
    test "$#" = 2 && test "$2" = "$(cat conversation)" || exit 7
    test -f resume-enabled || exit 8
    echo REATTACHED
    read line
    ;;
resume)
    test "$#" = 2 && test "$2" = "0804cb57-2fd0-4bc5-86c6-7e9cfd3e4521" || exit 9
    echo CODEX_REATTACHED
    read line
    ;;
*) echo CODEX_ORIGINAL; read line;;
esac
"#,
    )
    .unwrap();
    fs::set_permissions(&executable, fs::Permissions::from_mode(0o700)).unwrap();
    let failed_executable = host.state.join("copilot-fixture");
    fs::write(
        &failed_executable,
        "#!/bin/sh\ncase \"$1\" in\n--session-id) printf '%s\\n' \"$2\" > copilot-conversation; echo COPILOT_ORIGINAL; read line;;\n--resume) test \"$#\" = 2 && test \"$2\" = \"$(cat copilot-conversation)\" || exit 8; test -f copilot-enabled || exit 9; echo COPILOT_REATTACHED; read line;;\n*) exit 10;;\nesac\n",
    ).unwrap();
    fs::set_permissions(&failed_executable, fs::Permissions::from_mode(0o700)).unwrap();

    for (agent, path) in [
        ("Claude", &executable),
        ("Codex", &executable),
        ("Copilot", &failed_executable),
    ] {
        assert_eq!(
            host.request(json!({"SetAgentExecutable": {"agent": agent, "path": path}})),
            "Ok"
        );
        assert_eq!(
            host.request(json!({"StartSession": {"agent": agent, "project": repo}})),
            "Ok"
        );
    }
    assert_eq!(
        host.request(json!({"LinkConversation": {
            "pane": 1,
            "conversation": "0804cb57-2fd0-4bc5-86c6-7e9cfd3e4521"
        }})),
        "Ok"
    );

    fn await_state(host: &Host, predicate: impl Fn(&Value) -> bool) -> Value {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let reply = host.request(json!({"Poll": []}));
            let state = &reply["State"];
            if predicate(state) {
                return state.clone();
            }
            assert!(Instant::now() < deadline, "{reply}");
            thread::sleep(Duration::from_millis(20));
        }
    }

    await_state(&host, |state| {
        state["panes"][0]["lines"].to_string().contains("ORIGINAL")
            && state["panes"][1]["lines"]
                .to_string()
                .contains("CODEX_ORIGINAL")
            && state["panes"][2]["lines"]
                .to_string()
                .contains("COPILOT_ORIGINAL")
    });
    panel_action(&host, json!({"Show": {"pane": 1}}));
    panel_action(&host, json!({"Resize": {"path": [], "ratio": 0.62}}));
    let before = host.request(json!({"Poll": []}))["State"].clone();
    fs::write(repo.join("resume-enabled"), "").unwrap();

    host.restart();
    let recovered = await_state(&host, |state| {
        state["panes"][0]["lines"]
            .to_string()
            .contains("REATTACHED")
            && state["panes"][1]["lines"]
                .to_string()
                .contains("CODEX_REATTACHED")
            && state["panes"][2]["resume_error"].is_string()
    });
    assert_eq!(recovered["desktop_layout"], before["desktop_layout"]);
    assert_eq!(recovered["projects"], before["projects"]);
    assert_eq!(recovered["agent_executables"], before["agent_executables"]);
    assert_eq!(recovered["panes"][0]["exited"], false);
    assert_eq!(recovered["panes"][1]["exited"], false);
    assert_eq!(recovered["panes"][2]["exited"], true);
    assert!(
        recovered["panes"][2]["lines"]
            .to_string()
            .contains("COPILOT_ORIGINAL")
    );
    assert_ne!(recovered["panes"][0]["pid"], before["panes"][0]["pid"]);

    fs::write(repo.join("copilot-enabled"), "").unwrap();
    assert_eq!(host.request(json!({"ResumeSession": {"pane": 2}})), "Ok");
    let retried = await_state(&host, |state| {
        state["panes"][2]["lines"]
            .to_string()
            .contains("COPILOT_REATTACHED")
    });
    assert_eq!(retried["panes"][2]["exited"], false);
    assert_eq!(retried["panes"][0]["pid"], recovered["panes"][0]["pid"]);
    assert_eq!(retried["panes"][1]["pid"], recovered["panes"][1]["pid"]);
}

#[test]
fn closed_session_cleanup_preserves_live_panes_and_honors_persisted_policy() {
    let (mut host, repo) = panel_host();
    let marker = repo.join("retained");
    fs::write(&marker, "keep").unwrap();
    let poll = || json!({"Poll": []});
    assert_eq!(host.request(poll())["State"]["retention_days"], 30);
    for _ in 0..3 {
        assert_eq!(
            host.request(json!({"StartSession": {"agent": "Codex", "project": repo}})),
            "Ok"
        );
    }
    assert!(
        host.request(json!({"ForgetClosedSession": 2}))
            .get("Error")
            .is_some()
    );
    assert_eq!(host.request(json!({"Panel": {"Close": {"pane": 2}}})), "Ok");
    assert_eq!(host.request(json!({"ForgetClosedSession": 2})), "Ok");
    let state = host.request(poll());
    assert_eq!(state["State"]["terminal_projects"][2], "");
    assert_eq!(state["State"]["panes"][2]["lines"], json!([]));
    assert!(
        host.request(json!({"Panel": {"Show": {"pane": 2}}}))
            .get("Error")
            .is_some()
    );
    assert_eq!(host.request(json!({"Panel": {"Show": {"pane": 0}}})), "Ok");
    assert_eq!(host.request(json!({"Panel": {"Close": {"pane": 0}}})), "Ok");
    assert_eq!(host.request(json!({"SetRetentionDays": null})), "Ok");

    let manifest_path = host.state.join("workspace.json");
    let mut manifest: Value = serde_json::from_slice(&fs::read(&manifest_path).unwrap()).unwrap();
    manifest["sessions"][0]["closed_at"] = json!(1);
    fs::write(&manifest_path, serde_json::to_vec(&manifest).unwrap()).unwrap();
    host.restart();
    let state = host.request(poll());
    assert!(state["State"]["retention_days"].is_null());
    assert_ne!(state["State"]["terminal_projects"][0], "");

    assert_eq!(host.request(json!({"SetRetentionDays": 7})), "Ok");
    let state = host.request(poll());
    assert_eq!(state["State"]["retention_days"], 7);
    assert_eq!(state["State"]["terminal_projects"][0], "");
    assert_ne!(state["State"]["terminal_projects"][1], "");
    assert_eq!(state["State"]["panes"][0]["lines"], json!([]));
    assert_eq!(fs::read_to_string(marker).unwrap(), "keep");
}
