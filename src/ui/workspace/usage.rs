use gpui::{Context, Div, KeyDownEvent, MouseButton, PathPromptOptions, div, prelude::*, rgb};

use crate::usage::UsageAgent;
use crate::usage_panel::{Period, agent_name};

use crate::ui::controls::button;

use super::Workspace;

impl Workspace {
    pub(super) fn usage_view(&self, cx: &mut Context<Self>) -> Div {
        div()
            .size_full()
            .flex()
            .flex_col()
            .bg(rgb(0x11151d))
            .text_color(rgb(0xd9e0ee))
            .track_focus(&self.focus)
            .on_key_down(cx.listener(|view, event: &KeyDownEvent, _, cx| {
                if event.keystroke.key == "escape" {
                    view.usage.open = false;
                    cx.notify();
                }
            }))
            .child(
                div()
                    .flex()
                    .flex_none()
                    .items_center()
                    .gap_3()
                    .p_4()
                    .bg(rgb(0x171d28))
                    .border_b_1()
                    .border_color(rgb(0x2a3344))
                    .child(button("Volver").on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|view, _, _, cx| {
                            view.usage.open = false;
                            cx.notify();
                        }),
                    ))
                    .child(div().text_lg().child("Historial de uso local"))
                    .child(
                        div()
                            .flex_1()
                            .text_sm()
                            .text_color(rgb(0xaab8cc))
                            .child(self.usage.status()),
                    )
                    .child(if self.usage.scanning() {
                        div()
                            .text_sm()
                            .text_color(rgb(0xaab8cc))
                            .child("Actualizando…")
                    } else {
                        button("Actualizar").on_mouse_down(
                            MouseButton::Left,
                            cx.listener(|view, _, _, cx| {
                                view.usage.scan();
                                cx.notify();
                            }),
                        )
                    }),
            )
            .child(
                div()
                    .id("usage-history")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .child(self.usage_filters(cx))
                    .child(self.usage.content())
                    .child(self.usage.billing_content(self.billing_button(cx))),
            )
    }

    fn billing_button(&self, cx: &Context<Self>) -> Div {
        if self.usage.loading_billing() {
            return div().text_sm().child("Leyendo informe…");
        }
        div().child(button("Abrir informe JSON").on_mouse_down(
            MouseButton::Left,
            cx.listener(|_, _, _, cx| {
                let selected = cx.prompt_for_paths(PathPromptOptions {
                    files: true,
                    directories: false,
                    multiple: false,
                    prompt: Some("Abrir informe de costes".into()),
                });
                cx.spawn(async move |view, cx| {
                    if let Ok(Ok(Some(paths))) = selected.await
                        && let Some(path) = paths.into_iter().next()
                    {
                        _ = view.update(cx, |view, cx| {
                            view.usage.load_billing_report(path);
                            cx.notify();
                        });
                    }
                })
                .detach();
            }),
        ))
    }

    fn usage_filters(&self, cx: &Context<Self>) -> Div {
        let filters = &self.usage.filters;
        let periods = Period::OPTIONS.into_iter().map(|(period, label)| {
            filter_button(label, filters.period == period).on_mouse_down(
                MouseButton::Left,
                cx.listener(move |view, _, _, cx| {
                    view.usage.filters.period = period;
                    cx.notify();
                }),
            )
        });
        let agents = [
            None,
            Some(UsageAgent::Codex),
            Some(UsageAgent::Claude),
            Some(UsageAgent::Copilot),
        ]
        .into_iter()
        .map(|agent| {
            filter_button(agent.map_or("Todos", agent_name), filters.agent == agent).on_mouse_down(
                MouseButton::Left,
                cx.listener(move |view, _, _, cx| {
                    view.usage.filters.agent = agent;
                    cx.notify();
                }),
            )
        });
        let models = std::iter::once(None)
            .chain(self.usage.models().into_iter().map(Some))
            .map(|model| {
                let selected = filters.model.as_deref() == model;
                let value = model.map(str::to_owned);
                filter_button(model.unwrap_or("Todos"), selected).on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |view, _, _, cx| {
                        view.usage.filters.model = value.clone();
                        cx.notify();
                    }),
                )
            });
        div()
            .flex()
            .flex_col()
            .gap_2()
            .p_4()
            .child(filter_row("Periodo").children(periods))
            .child(filter_row("Agente").children(agents))
            .child(filter_row("Modelo").children(models))
    }
}

fn filter_row(label: &'static str) -> Div {
    div()
        .flex()
        .flex_wrap()
        .items_center()
        .gap_2()
        .child(div().text_sm().child(label))
}

fn filter_button(label: &str, selected: bool) -> Div {
    button(label.to_owned()).when(selected, |control| {
        control.bg(rgb(0x385371)).border_color(rgb(0x87e0cd))
    })
}
