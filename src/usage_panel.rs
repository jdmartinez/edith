use std::{
    collections::BTreeSet,
    env,
    path::PathBuf,
    sync::mpsc::{self, Receiver, TryRecvError},
    thread,
};

use anyhow::{Context, Result};
use chrono::DateTime;
use gpui::{Div, div, prelude::*, px, rgb};

use crate::usage::{UsageAgent, UsageEvent, UsageStore};

mod analytics;
mod charts;
mod pricing;

use analytics::{Metric, Summary, Totals, selected, summarize};
use charts::{daily_chart, heatmap};
use pricing::{BillingReport, CURRENT_RATE_DATE, Estimate, EstimatedAmount, format_money};

type History = Result<Vec<UsageEvent>>;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum Period {
    Week,
    Month,
    Quarter,
    #[default]
    All,
}

impl Period {
    pub(crate) const OPTIONS: [(Self, &'static str); 4] = [
        (Self::Week, "7 días"),
        (Self::Month, "30 días"),
        (Self::Quarter, "90 días"),
        (Self::All, "Todo"),
    ];

    fn first_day(self, current_day: i64) -> Option<i64> {
        let days = match self {
            Self::Week => 7,
            Self::Month => 30,
            Self::Quarter => 90,
            Self::All => return None,
        };
        Some(current_day - days + 1)
    }
}

#[derive(Default)]
pub(crate) struct Filters {
    pub(crate) period: Period,
    pub(crate) agent: Option<UsageAgent>,
    pub(crate) model: Option<String>,
}

pub(crate) fn agent_name(agent: UsageAgent) -> &'static str {
    match agent {
        UsageAgent::Codex => "Codex",
        UsageAgent::Claude => "Claude Code",
        UsageAgent::Copilot => "GitHub Copilot CLI",
    }
}

enum Update {
    Saved(Vec<UsageEvent>),
    Finished { history: History, incomplete: bool },
}

#[derive(Default)]
pub struct UsagePanel {
    pub open: bool,
    updates: Option<Receiver<Update>>,
    events: Vec<UsageEvent>,
    pub(crate) filters: Filters,
    message: &'static str,
    billing_report: Option<BillingReport>,
    billing_error: bool,
    billing_updates: Option<Receiver<Result<BillingReport>>>,
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
                    history: store.map(|_| Vec::new()),
                    incomplete: true,
                });
                return;
            };
            if let Ok(events) = store.saved()
                && send.send(Update::Saved(events)).is_err()
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
                history: store.saved(),
                incomplete,
            });
        });
    }

    pub fn refresh(&mut self) -> bool {
        let mut changed = self.refresh_billing();
        while let Some(updates) = &self.updates {
            match updates.try_recv() {
                Ok(Update::Saved(events)) => self.events = events,
                Ok(Update::Finished {
                    history,
                    incomplete,
                }) => {
                    self.message = match history {
                        Ok(history) => {
                            self.events = history;
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

    fn refresh_billing(&mut self) -> bool {
        let Some(updates) = &self.billing_updates else {
            return false;
        };
        match updates.try_recv() {
            Ok(Ok(report)) => self.billing_report = Some(report),
            Ok(Err(_)) | Err(TryRecvError::Disconnected) => self.billing_error = true,
            Err(TryRecvError::Empty) => return false,
        }
        self.billing_updates = None;
        true
    }

    pub fn status(&self) -> &'static str {
        if self.scanning() {
            "Escaneando historial local…"
        } else {
            self.message
        }
    }

    pub(crate) fn models(&self) -> BTreeSet<&str> {
        self.events
            .iter()
            .map(|event| event.model.as_str())
            .collect()
    }

    pub(crate) fn load_billing_report(&mut self, path: PathBuf) {
        if self.loading_billing() {
            return;
        }
        let (send, receive) = mpsc::channel();
        self.billing_updates = Some(receive);
        self.billing_error = false;
        thread::spawn(move || {
            let result = std::fs::read(path)
                .map_err(Into::into)
                .and_then(|bytes| pricing::parse_billing_report(&bytes));
            let _ = send.send(result);
        });
    }

    pub(crate) fn loading_billing(&self) -> bool {
        self.billing_updates.is_some()
    }

    pub(crate) fn billing_content(&self, import_button: Div) -> Div {
        let mut panel = card()
            .mx_6()
            .mb_6()
            .child(
                div()
                    .text_lg()
                    .child("Importes informados por el proveedor"),
            )
            .child(div().text_sm().text_color(rgb(0xaab8cc)).child(
                "Informe de cuenta u organización, independiente de los filtros de uso local.",
            ))
            .child(import_button);
        if self.billing_error {
            panel = panel.child(
                div()
                    .text_sm()
                    .text_color(rgb(0xf0b687))
                    .child("No se pudo abrir el informe. Elige un JSON de OpenAI Costs o Anthropic Cost Analytics con importes."),
            );
        }
        if let Some(report) = &self.billing_report {
            let date = |timestamp| {
                DateTime::from_timestamp(timestamp, 0).map_or_else(String::new, |date| {
                    date.format("%d/%m/%Y %H:%M").to_string()
                })
            };
            panel = panel
                .child(format!(
                    "{} · {} a {} UTC (fin excluido)",
                    report.provider,
                    date(report.start),
                    date(report.end_exclusive),
                ))
                .when(report.has_more, |panel| {
                    panel.child("Página parcial. El proveedor indica que quedan resultados.")
                })
                .children(report.amounts.iter().map(|(currency, amount)| {
                    div().child(format!("{} {currency}", format_money(*amount)))
                }));
        }
        panel
    }

    pub fn content(&self) -> Div {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |duration| duration.as_secs() as i64);
        let current_day = now.div_euclid(86_400);
        let summary = summarize(&self.events, &self.filters, current_day);
        if summary.days.is_empty() {
            return div()
                .p_6()
                .text_color(rgb(0xaab8cc))
                .child(if self.scanning() {
                    "Buscando métricas locales…"
                } else if self.events.is_empty() {
                    "Sin métricas locales verificables guardadas."
                } else {
                    "Sin métricas para esta selección."
                });
        }
        let estimate = pricing::summarize(
            selected(&self.events, &self.filters, current_day).map(|(event, _)| event),
        );
        summary
            .content(self.filters.period.first_day(current_day), current_day)
            .child(cost_estimate(&estimate))
    }
}

fn cost_estimate(estimate: &Estimate) -> Div {
    let amount = |title: String, amount: &EstimatedAmount| {
        let label = if amount.events == 0 {
            amount.label()
        } else {
            format!("{} USD · {} registros", amount.label(), amount.events)
        };
        div().child(format!("{title} · {label}"))
    };
    card()
        .child(div().text_lg().child("Coste estimado de los tokens"))
        .child(
            div().text_sm().text_color(rgb(0xaab8cc))
                .child("Tarifas estándar de API. El cálculo excluye recargos y planes que el historial local no registra."),
        )
        .child(amount("Tarifa histórica".into(), &estimate.historical))
        .child(amount(
            format!("Tarifa de referencia del {CURRENT_RATE_DATE}"),
            &estimate.current,
        ))
        .child(format!(
            "Sin estimación · {} registros",
            estimate.unpriced_events
        ))
}

impl Summary {
    fn content(&self, first_day: Option<i64>, current_day: i64) -> Div {
        let total = &self.total;
        div()
            .flex()
            .flex_col()
            .gap_4()
            .p_6()
            .child(
                div()
                    .flex()
                    .gap_3()
                    .child(metric("Tokens de entrada", total.input, 0x87e0cd))
                    .child(metric("Tokens de salida", total.output, 0xf0b687))
                    .child(metric("Entrada en caché", total.cached, 0xc4b6ff)),
            )
            .child(
                div()
                    .flex()
                    .gap_3()
                    .child(metric("Tareas", Metric::default(), 0x87e0cd))
                    .child(metric("Tiempo de actividad", Metric::default(), 0xf0b687)),
            )
            .when(self.agents.contains_key(&UsageAgent::Copilot), |panel| {
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
            .child(daily_chart(&self.days, first_day, current_day))
            .child(heatmap(&self.hours))
            .child(breakdown(
                "Agentes",
                self.agents
                    .iter()
                    .map(|(&agent, totals)| (agent_name(agent), totals)),
            ))
            .child(breakdown(
                "Modelos",
                self.models
                    .iter()
                    .map(|(model, totals)| (model.as_str(), totals)),
            ))
    }
}

fn breakdown<'a>(title: &'static str, rows: impl Iterator<Item = (&'a str, &'a Totals)>) -> Div {
    card()
        .child(div().text_lg().child(title))
        .child(model_row("Nombre", "Entrada".into(), "Salida".into()).text_color(rgb(0xaab8cc)))
        .children(
            rows.map(|(name, totals)| model_row(name, totals.input.label(), totals.output.label())),
        )
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
