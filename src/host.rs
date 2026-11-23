mod attention;
mod endpoint;
mod git;
mod protocol;
mod terminal;
mod workspace;

#[cfg(feature = "ui")]
pub(crate) use attention::{AgentState, AttentionStatus};
pub(crate) use attention::{HOOK_ARGUMENT, record_claude_hook};
pub use endpoint::Endpoint;
pub use protocol::{Layout, PANE_COUNT, Reply, Request, Snapshot};

#[cfg(feature = "ui")]
pub use git::{Repository, SessionDirectories, WorktreeRemoval};
#[cfg(feature = "ui")]
pub use protocol::{Agent, SessionTarget};
