use std::borrow::Cow;
use std::path::Path;

use anyhow::Result;
use gpui::{AssetSource, CursorStyle, Div, SharedString, div, prelude::*, px, rgb, svg};

use crate::host::Agent;
use crate::panels::Axis;

pub(super) struct AgentIcons;

impl AssetSource for AgentIcons {
    fn load(&self, path: &str) -> Result<Option<Cow<'static, [u8]>>> {
        let bytes: &[u8] = match path {
            "claude.svg" => include_bytes!("../../assets/agents/claude.svg"),
            "codex.svg" => include_bytes!("../../assets/agents/codex.svg"),
            "copilot.svg" => include_bytes!("../../assets/agents/copilot.svg"),
            _ => return Ok(None),
        };
        Ok(Some(Cow::Borrowed(bytes)))
    }

    fn list(&self, _path: &str) -> Result<Vec<SharedString>> {
        Ok(vec![
            "claude.svg".into(),
            "codex.svg".into(),
            "copilot.svg".into(),
        ])
    }
}

pub(super) fn agent_icon(agent: Agent) -> impl IntoElement {
    let (path, color) = match agent {
        Agent::Claude => ("claude.svg", 0xe89172),
        Agent::Codex => ("codex.svg", 0xf0f1f2),
        Agent::Copilot => ("copilot.svg", 0xc8bcf4),
    };
    svg()
        .path(path)
        .size(px(15.0))
        .flex_none()
        .text_color(rgb(color))
}

pub(super) fn icon_button(agent: Agent, text: impl Into<SharedString>) -> Div {
    button_style()
        .flex()
        .items_center()
        .gap(px(6.0))
        .child(agent_icon(agent))
        .child(div().min_w_0().truncate().child(text.into()))
}

pub(super) fn button(text: impl Into<SharedString>) -> Div {
    button_style().child(text.into())
}

fn button_style() -> Div {
    div()
        .px_2()
        .py_1()
        .rounded_md()
        .text_size(px(12.0))
        .bg(rgb(0x222b3b))
        .hover(|style| style.bg(rgb(0x34465e)))
        .cursor(CursorStyle::PointingHand)
        .whitespace_nowrap()
}

pub(super) fn project_name(path: &Path) -> &str {
    path.file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("Repositorio")
}

pub(super) fn separator_style(axis: Axis, width: f32, height: f32) -> Div {
    div()
        .w(px(width))
        .h(px(height))
        .flex_none()
        .bg(rgb(0x222a38))
        .hover(|style| style.bg(rgb(0x599ce8)))
        .cursor(match axis {
            Axis::Columns => CursorStyle::ResizeLeftRight,
            Axis::Rows => CursorStyle::ResizeUpDown,
        })
}
