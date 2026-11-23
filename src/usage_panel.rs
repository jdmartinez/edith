use std::{
    collections::BTreeMap,
    env,
    path::PathBuf,
    sync::mpsc::{self, Receiver, TryRecvError},
    thread,
};

use anyhow::{Context, Result};
use chrono::DateTime;
use gpui::{Div, div, prelude::*, px, rgb};

use crate::usage::{UsageAgent, UsageEvent, UsageStore};

type Summaries = BTreeMap<UsageAgent, Summary>;
type History = Result<Summaries>;

enum Update {
    Saved(Summaries),
    Finished { history: History, incomplete: bool },
}

#[derive(Default)]
pub struct UsagePanel {
    pub open: bool,
    updates: Option<Receiver<Update>>,
    summaries: Summaries,
    message: &'static str,
}

impl UsagePanel {
    pub fn scanning(&self) -> bool {
        self.updates.is_some()
    }

    pub fn scan(&mut self) {
        if self.scanning() {
            return;
        }
        self.message = "";
        let (send, receive) = mpsc::channel();
        self.updates = Some(receive);
        thread::spawn(move || {
            let store = usage_database().map(UsageStore::new);
            let Ok(store) = store else {
                let _ = send.send(Update::Finished {
                    history: store.map(|_| Summaries::default()),
                    incomplete: true,
                });
                return;
            };
            if let Ok(events) = store.saved()
                && send.send(Update::Saved(summarize(events))).is_err()
            {
                return;
            }
            let codex = agent_home("CODEX_HOME", ".codex").and_then(|home| store.scan(&home));
            let claude = agent_home("CLAUDE_CONFIG_DIR", ".claude")
                .and_then(|home| store.scan_claude(&home));
            let copilot =
                agent_home("COPILOT_HOME", ".copilot").and_then(|home| store.scan_copilot(&home));
            let incomplete = codex.is_err() || claude.is_err() || copilot.is_err();
            let _ = send.send(Update::Finished {
                history: store.saved().map(summarize),
                incomplete,
            });
        });
    }

    pub fn refresh(&mut self) -> bool {
        let mut changed = false;
        while let Some(updates) = &self.updates {
            match updates.try_recv() {
                Ok(Update::Saved(summary)) => self.summaries = summary,
                Ok(Update::Finished {
                    history,
                    incomplete,
                }) => {
                    self.message = match history {
                        Ok(history) => {
                            self.summaries = history;
                            if incomplete {
                                "El escaneo quedó incompleto. Puedes reintentarlo."
                            } else {
                                ""
                            }
                        }
                        Err(_) => "No se pudo leer el historial guardado.",
                    };
                    self.updates = None;
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    self.message = "El escaneo se interrumpió. Puedes reintentarlo.";
                    self.updates = None;
                }
            }
            changed = true;
        }
        changed
    }

    pub fn status(&self) -> &'static str {
        if self.scanning() {
            "Escaneando historial local…"
        } else {
            self.message
        }
    }

    pub fn content(&self) -> Div {
        if self.summaries.is_empty() && self.scanning() {
            return div()
                .p_6()
                .text_color(rgb(0xaab8cc))
                .child("Buscando métricas locales…");
        }
        div()
            .flex()
            .flex_col()
            .gap_6()
            .p_6()
            .children(self.summaries.iter().map(|(agent, summary)| {
                let name = match agent {
                    UsageAgent::Codex => "Codex",
                    UsageAgent::Claude => "Claude Code",
                    UsageAgent::Copilot => "GitHub Copilot CLI",
                };
                summary.content(name, *agent)
            }))
            .when(
                !self.summaries.contains_key(&UsageAgent::Copilot),
                |panel| {
                    panel.child(
                        card()
                            .child(div().text_xl().child("GitHub Copilot CLI"))
                            .child("Sin métricas locales verificables guardadas."),
                    )
                },
            )
    }
}

impl Summary {
    fn content(&self, name: &'static str, agent: UsageAgent) -> Div {
        let Self {
            total,
            days,
            models,
        } = self;
        let mut models = models.iter().collect::<Vec<_>>();
        models.sort_by(|(a, first), (b, second)| {
            second
                .input
                .sum
                .cmp(&first.input.sum)
                .then_with(|| a.cmp(b))
        });
        div()
            .flex()
            .flex_col()
            .gap_4()
            .child(div().text_xl().child(name))
            .child(
                div()
                    .flex()
                    .gap_3()
                    .child(metric("Tokens de entrada", total.input, 0x87e0cd))
                    .child(metric("Tokens de salida", total.output, 0xf0b687))
                    .child(metric("Entrada en caché", total.cached, 0xc4b6ff)),
            )
            .when(agent == UsageAgent::Copilot, |panel| {
                panel.child(
                    div()
                        .flex()
                        .gap_3()
                        .child(metric("Escritura en caché", total.cache_write, 0xc4b6ff))
                        .child(metric("Tokens de razonamiento", total.reasoning, 0xf0b687))
                        .child(metric(
                            "Tiempo de API (ms)",
                            total.api_duration_ms,
                            0x87e0cd,
                        )),
                )
            })
            .child(daily_chart(days))
            .child(
                card()
                    .child(div().text_lg().child("Modelos"))
                    .child(
                        model_row("Modelo", "Entrada".into(), "Salida".into())
                            .text_color(rgb(0xaab8cc)),
                    )
                    .children(models.iter().map(|(model, tokens)| {
                        model_row(model, tokens.input.label(), tokens.output.label())
                    })),
            )
    }
}

fn usage_database() -> Result<PathBuf> {
    let directory = if let Some(directory) = env::var_os("EDITH_PROTOTYPE_STATE") {
        env::current_dir()?.join(directory)
    } else {
        dirs::data_local_dir()
            .context("missing local data directory")?
            .join("edith")
    };
    Ok(directory.join("usage.sqlite"))
}

fn agent_home(variable: &str, default: &str) -> Result<PathBuf> {
    if let Some(home) = env::var_os(variable).filter(|value| !value.is_empty()) {
        return Ok(env::current_dir()?.join(home));
    }
    Ok(dirs::home_dir()
        .context("missing home directory")?
        .join(default))
}

#[derive(Default)]
struct Summary {
    total: Totals,
    days: BTreeMap<i64, Totals>,
    models: BTreeMap<String, Totals>,
}

fn summarize(events: Vec<UsageEvent>) -> Summaries {
    let mut summaries = Summaries::new();
    for event in events {
        let summary = summaries.entry(event.agent).or_default();
        summary.total.add(&event);
        summary
            .days
            .entry(event.timestamp.div_euclid(86_400))
            .or_default()
            .add(&event);
        summary
            .models
            .entry(event.model.clone())
            .or_default()
            .add(&event);
    }
    summaries
}

#[derive(Default)]
struct Totals {
    input: Metric,
    cached: Metric,
    output: Metric,
    cache_write: Metric,
    reasoning: Metric,
    api_duration_ms: Metric,
}

impl Totals {
    fn add(&mut self, event: &UsageEvent) {
        self.input.add(event.input);
        self.cached.add(event.cached);
        self.output.add(event.output);
        self.cache_write.add(event.cache_write);
        self.reasoning.add(event.reasoning);
        self.api_duration_ms.add(event.api_duration_ms);
    }
}

#[derive(Clone, Copy, Default)]
struct Metric {
    sum: Option<u64>,
    partial: bool,
}

impl Metric {
    fn add(&mut self, amount: Option<i64>) {
        match amount.and_then(|amount| u64::try_from(amount).ok()) {
            Some(amount) => self.sum = Some(self.sum.unwrap_or_default().saturating_add(amount)),
            None => self.partial = true,
        }
    }

    fn label(self) -> String {
        match self.sum {
            Some(sum) if self.partial => format!("{sum} (parcial)"),
            Some(sum) => sum.to_string(),
            None => "Sin datos".into(),
        }
    }
}

fn card() -> Div {
    div()
        .flex()
        .flex_col()
        .gap_2()
        .p_4()
        .rounded_lg()
        .bg(rgb(0x192332))
        .border_1()
        .border_color(rgb(0x2a3a50))
}

fn metric(label: &'static str, amount: Metric, color: u32) -> Div {
    card()
        .flex_1()
        .child(div().text_sm().text_color(rgb(0xaab8cc)).child(label))
        .child(
            div()
                .text_2xl()
                .text_color(rgb(color))
                .child(amount.label()),
        )
}

fn model_row(model: &str, input: String, output: String) -> Div {
    div()
        .flex()
        .gap_3()
        .py_2()
        .border_t_1()
        .border_color(rgb(0x2a3a50))
        .child(div().flex_1().min_w_0().truncate().child(model.to_owned()))
        .child(div().w(px(125.0)).text_right().child(input))
        .child(div().w(px(125.0)).text_right().child(output))
}

fn daily_chart(days: &BTreeMap<i64, Totals>) -> Div {
    let (&last, _) = days.last_key_value().unwrap();
    let first = (*days.first_key_value().unwrap().0).max(last - 29);
    let peak_input = days
        .range(first..)
        .filter_map(|(_, time)| time.input.sum)
        .max();
    let peak_output = days
        .range(first..)
        .filter_map(|(_, time)| time.output.sum)
        .max();
    let bars = (first..=last).map(|day| {
        let (input, output) = days
            .get(&day)
            .map_or((None, None), |totals| (totals.input.sum, totals.output.sum));
        div()
            .flex()
            .flex_1()
            .min_w_0()
            .flex_col()
            .h(px(100.0))
            .child(
                div().flex().flex_col().justify_end().h(px(50.0)).child(
                    div()
                        .w_full()
                        .h(px(50.0 * bar_ratio(input, peak_input)))
                        .bg(rgb(0x87e0cd)),
                ),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .h(px(50.0))
                    .border_t_1()
                    .border_color(rgb(0x435873))
                    .child(
                        div()
                            .w_full()
                            .h(px(49.0 * bar_ratio(output, peak_output)))
                            .bg(rgb(0xf0b687)),
                    ),
            )
    });
    let date = |day| {
        DateTime::from_timestamp(day * 86_400, 0)
            .map(|date| date.format("%d/%m/%Y").to_string())
            .unwrap_or_default()
    };
    card()
        .child(div().text_lg().child("Tokens por día"))
        .child(
            div()
                .flex()
                .gap_4()
                .text_sm()
                .child(
                    div()
                        .text_color(rgb(0x87e0cd))
                        .child(format!("Entrada · máx. {}", peak_label(peak_input))),
                )
                .child(
                    div()
                        .text_color(rgb(0xf0b687))
                        .child(format!("Salida · máx. {}", peak_label(peak_output))),
                ),
        )
        .child(div().flex().gap_1().children(bars))
        .child(
            div()
                .flex()
                .justify_between()
                .text_sm()
                .text_color(rgb(0xaab8cc))
                .child(date(first))
                .child(if first == last {
                    String::new()
                } else {
                    date(last)
                }),
        )
}

fn bar_ratio(amount: Option<u64>, peak: Option<u64>) -> f32 {
    amount.unwrap_or_default() as f32 / peak.unwrap_or_default().max(1) as f32
}

fn peak_label(peak: Option<u64>) -> String {
    peak.map_or_else(|| "Sin datos".into(), |peak| peak.to_string())
}
