#![cfg(unix)]

use std::{
    fs,
    io::{BufReader, Write},
    os::unix::{fs::PermissionsExt, net::UnixStream},
    path::PathBuf,
    process::{Child, Command},
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

fn start_host() -> Host {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let state = PathBuf::from(format!("/tmp/edith-test-{nonce:x}"));
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

#[test]
fn codex_uses_the_repository_root_and_custom_executable_and_survives_reconnection() {
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
    let detected = host.state.join("codex");
    fs::write(&detected, "#!/bin/sh\necho DETECTED\n").unwrap();
    fs::set_permissions(&detected, fs::Permissions::from_mode(0o700)).unwrap();
    let custom = host.state.join("custom codex");
    fs::write(&custom, "#!/bin/sh\nprintf 'CUSTOM %s %s\\n' \"${PWD##*/}\" \"$CODEX_HOME\"\nread line\nprintf 'FINAL=%s\\n' \"$line\"\n").unwrap();
    fs::set_permissions(&custom, fs::Permissions::from_mode(0o700)).unwrap();

    assert_eq!(
        host.request(json!({"AddProject": repo.join("subfolder")})),
        "Ok"
    );
    let canonical = repo.canonicalize().unwrap();
    assert_eq!(host.request(json!({"SetCodex": custom})), "Ok");
    assert_eq!(host.request(json!({"StartCodex": canonical})), "Ok");

    let first = host.request(json!({"Poll": []}));
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
    assert_eq!(host.request(json!({"SetCodex": null})), "Ok");
    assert_eq!(host.request(json!({"StartCodex": canonical})), "Ok");
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let response = host.request(json!({"Poll": []}));
        let pane = &response["State"]["panes"][1];
        if pane["exited"] == true && marker(pane).contains("DETECTED") {
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
    assert_eq!(host.request(json!({"StartCodex":second})), "Ok");
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
