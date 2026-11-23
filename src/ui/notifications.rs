use std::collections::BTreeMap;

use crate::host::{AgentState, AttentionStatus};

#[derive(Default)]
pub(super) struct Notifications {
    enabled: bool,
    seen: BTreeMap<usize, (u32, u64)>,
}

impl Notifications {
    pub(super) fn enabled(&self) -> bool {
        self.enabled
    }

    pub(super) fn set_enabled(&mut self, enabled: bool) {
        self.enabled = enabled;
    }

    pub(super) fn observe(
        &mut self,
        pane: usize,
        pid: Option<u32>,
        status: AttentionStatus,
        initial: bool,
    ) -> bool {
        let (Some(pid), Some(request)) = (pid, status.confirmed_request) else {
            return false;
        };
        if status.estimated || status.state != AgentState::Attention {
            return false;
        }
        let seen = self.seen.entry(pane).or_insert((pid, 0));
        let fresh = seen.0 != pid || request > seen.1;
        if fresh {
            *seen = (pid, request);
        }
        self.enabled && fresh && !initial
    }
}

pub(super) fn deliver_attention() {
    #[cfg(any(target_os = "macos", windows))]
    std::thread::spawn(|| {
        use std::process::Stdio;

        let _ = native_command()
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    });
}

#[cfg(target_os = "macos")]
fn native_command() -> std::process::Command {
    // AppleScript Standard Additions dispatches through Notification Center.
    let mut command = std::process::Command::new("/usr/bin/osascript");
    command.args([
        "-e",
        r#"display notification "Una sesión necesita tu atención." with title "Edith""#,
    ]);
    command
}

#[cfg(windows)]
fn native_command() -> std::process::Command {
    use std::os::windows::process::CommandExt;

    // NotifyIcon uses a native system tray notification without package identity.
    // The icon must remain alive long enough for Windows to display the balloon.
    const SCRIPT: &str = r#"
Add-Type -AssemblyName System.Windows.Forms
Add-Type -AssemblyName System.Drawing
$notice = New-Object System.Windows.Forms.NotifyIcon
try {
    $notice.Icon = [System.Drawing.SystemIcons]::Information
    $notice.Visible = $true
    $notice.ShowBalloonTip(5000, 'Edith', 'Una sesión necesita tu atención.', [System.Windows.Forms.ToolTipIcon]::Info)
    $deadline = [DateTime]::UtcNow.AddSeconds(6)
    while ([DateTime]::UtcNow -lt $deadline) {
        [System.Windows.Forms.Application]::DoEvents()
        Start-Sleep -Milliseconds 100
    }
} finally {
    $notice.Dispose()
}
"#;
    const CREATE_NO_WINDOW: u32 = 0x08000000;
    let mut command = std::process::Command::new("powershell.exe");
    command.creation_flags(CREATE_NO_WINDOW).args([
        "-NoProfile",
        "-NonInteractive",
        "-STA",
        "-Command",
        SCRIPT,
    ]);
    command
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(id: u64) -> AttentionStatus {
        AttentionStatus {
            state: AgentState::Attention,
            confirmed_request: Some(id),
            estimated: false,
        }
    }

    #[test]
    fn notifications_are_opt_in_and_only_fresh_confirmed_requests_notify() {
        let mut notifications = Notifications::default();
        assert!(!notifications.observe(0, Some(21), request(1), false));
        notifications.set_enabled(true);
        assert!(!notifications.observe(0, Some(21), request(1), false));
        assert!(!notifications.observe(0, Some(21), request(2), true));
        assert!(!notifications.observe(0, Some(21), request(2), false));
        assert!(!notifications.observe(
            0,
            Some(21),
            AttentionStatus {
                estimated: true,
                ..request(3)
            },
            false,
        ));
        assert!(notifications.observe(0, Some(21), request(3), false));
        assert!(!notifications.observe(0, Some(21), request(3), false));
        assert!(notifications.observe(0, Some(22), request(1), false));
    }
}
