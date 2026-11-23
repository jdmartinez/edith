use std::collections::BTreeMap;

use chrono::{DateTime, Datelike, Timelike, Utc};

use crate::usage::{UsageAgent, UsageEvent};

use super::Filters;

#[derive(Default)]
pub(super) struct Summary {
    pub(super) total: Totals,
    pub(super) days: BTreeMap<i64, Totals>,
    pub(super) hours: BTreeMap<(u32, u32), Totals>,
    pub(super) agents: BTreeMap<UsageAgent, Totals>,
    pub(super) models: BTreeMap<String, Totals>,
}

pub(super) fn selected<'a>(
    events: &'a [UsageEvent],
    filters: &'a Filters,
    current_day: i64,
) -> impl Iterator<Item = (&'a UsageEvent, DateTime<Utc>)> {
    let first_day = filters.period.first_day(current_day);
    events.iter().filter_map(move |event| {
        let day = event.timestamp.div_euclid(86_400);
        if first_day.is_some_and(|first| day < first || day > current_day)
            || filters.agent.is_some_and(|agent| agent != event.agent)
            || filters
                .model
                .as_ref()
                .is_some_and(|model| *model != event.model)
        {
            return None;
        }
        DateTime::from_timestamp(event.timestamp, 0).map(|date| (event, date))
    })
}

pub(super) fn summarize(events: &[UsageEvent], filters: &Filters, current_day: i64) -> Summary {
    let mut summary = Summary::default();
    for (event, date) in selected(events, filters, current_day) {
        let day = event.timestamp.div_euclid(86_400);
        summary.total.add(event);
        summary.days.entry(day).or_default().add(event);
        summary
            .hours
            .entry((date.weekday().num_days_from_monday(), date.hour()))
            .or_default()
            .add(event);
        summary.agents.entry(event.agent).or_default().add(event);
        summary
            .models
            .entry(event.model.clone())
            .or_default()
            .add(event);
    }
    summary
}

#[derive(Default)]
pub(super) struct Totals {
    pub(super) input: Metric,
    pub(super) cached: Metric,
    pub(super) output: Metric,
    pub(super) cache_write: Metric,
    pub(super) reasoning: Metric,
    pub(super) api_duration_ms: Metric,
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
pub(super) struct Metric {
    pub(super) sum: Option<u64>,
    partial: bool,
}

impl Metric {
    fn add(&mut self, amount: Option<i64>) {
        match amount.and_then(|amount| u64::try_from(amount).ok()) {
            Some(amount) => self.sum = Some(self.sum.unwrap_or_default().saturating_add(amount)),
            None => self.partial = true,
        }
    }

    pub(super) fn label(self) -> String {
        match self.sum {
            Some(sum) if self.partial => format!("{sum} (parcial)"),
            Some(sum) => sum.to_string(),
            None => "Sin datos".into(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::usage_panel::Period;

    fn event(
        day: i64,
        hour: i64,
        agent: UsageAgent,
        model: &str,
        input: Option<i64>,
    ) -> UsageEvent {
        UsageEvent {
            agent,
            timestamp: day * 86_400 + hour * 3600,
            model: model.into(),
            input,
            output: Some(4),
            cached: None,
            cache_write: None,
            reasoning: None,
            api_duration_ms: Some(100),
        }
    }

    #[test]
    fn period_boundaries_include_today_and_exclude_the_preceding_day() {
        let events = [
            event(10, 0, UsageAgent::Codex, "a", Some(1)),
            event(12, 0, UsageAgent::Codex, "a", Some(1)),
            event(70, 0, UsageAgent::Codex, "a", Some(1)),
            event(71, 0, UsageAgent::Codex, "a", Some(1)),
            event(93, 0, UsageAgent::Codex, "a", Some(1)),
            event(94, 0, UsageAgent::Codex, "a", Some(1)),
            event(100, 23, UsageAgent::Codex, "a", Some(1)),
        ];
        for (period, count) in [
            (Period::Week, 2),
            (Period::Month, 4),
            (Period::Quarter, 6),
            (Period::All, 7),
        ] {
            let summary = summarize(
                &events,
                &Filters {
                    period,
                    ..Filters::default()
                },
                100,
            );
            assert_eq!(summary.total.input.sum, Some(count));
        }
    }

    #[test]
    fn every_breakdown_uses_the_same_filtered_events_and_has_the_same_totals() {
        let events = [
            event(3, 9, UsageAgent::Codex, "a", Some(0)),
            event(10, 9, UsageAgent::Codex, "a", None),
            event(10, 21, UsageAgent::Codex, "a", Some(11)),
            event(10, 21, UsageAgent::Claude, "a", Some(100)),
            event(10, 21, UsageAgent::Codex, "b", Some(100)),
            event(1, 21, UsageAgent::Codex, "a", Some(100)),
        ];
        let filters = Filters {
            period: Period::Week,
            agent: Some(UsageAgent::Codex),
            model: Some("a".into()),
        };
        let summary = summarize(&events, &filters, 9);
        assert_eq!(summary.total.input.label(), "0");

        let summary = summarize(
            &events,
            &Filters {
                period: Period::All,
                ..filters
            },
            10,
        );
        assert_eq!(summary.total.input.label(), "111 (parcial)");
        assert_eq!(summary.total.output.sum, Some(16));
        assert_eq!(summary.hours[&(6, 9)].input.label(), "0 (parcial)"); // Unix days 3 and 10 are Sundays.
        for rows in [
            summary.days.values().collect::<Vec<_>>(),
            summary.hours.values().collect(),
            summary.agents.values().collect(),
            summary.models.values().collect(),
        ] {
            assert_eq!(
                rows.iter()
                    .filter_map(|bucket| bucket.input.sum)
                    .sum::<u64>(),
                111
            );
            assert_eq!(
                rows.iter()
                    .filter_map(|bucket| bucket.output.sum)
                    .sum::<u64>(),
                16
            );
        }
    }
}
