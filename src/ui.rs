use std::env;
use std::sync::mpsc;
use std::thread;

use anyhow::Result;
use gpui::{App, AppContext, Application, Bounds, WindowBounds, WindowOptions, px, size};

use crate::host::Endpoint;

use controls::AgentIcons;
use workspace::Workspace;

mod client;
mod controls;
mod workspace;

pub fn run(endpoint: Endpoint, agents: bool, desktop: bool) -> Result<()> {
    endpoint.ensure_host(agents, desktop, &env::current_dir()?)?;
    let (send, requests) = mpsc::channel();
    let (updates, receive) = mpsc::channel();
    thread::spawn(move || client::poll(endpoint, requests, updates));

    let app = Application::new().with_assets(AgentIcons);
    app.run(move |cx: &mut App| {
        cx.on_window_closed(|cx| {
            if cx.windows().is_empty() {
                cx.quit();
            }
        })
        .detach();
        let bounds = Bounds::centered(None, size(px(1200.0), px(740.0)), cx);
        cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                window_min_size: Some(size(px(780.0), px(380.0))),
                ..Default::default()
            },
            |window, cx| {
                window.set_window_title("Edith");
                cx.new(move |cx| Workspace::new(send, receive, desktop, window, cx))
            },
        )
        .expect("could not open the GPUI prototype window");
        cx.activate(true);
    });
    Ok(())
}
