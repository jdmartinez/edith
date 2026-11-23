use std::collections::BTreeMap;

use anyhow::{Context, Result, ensure};
use chrono::{DateTime, NaiveDate};
use serde::Deserialize;

use crate::usage::{UsageAgent, UsageEvent};

pub(super) const CURRENT_RATE_DATE: &str = "23/11/2026";

#[derive(Default)]
pub(super) struct Estimate {
    pub(super) historical: EstimatedAmount,
    pub(super) current: EstimatedAmount,
    pub(super) unpriced_events: usize,
}

#[derive(Default)]
pub(super) struct EstimatedAmount {
    lower_nanos: i128,
    upper_nanos: i128,
    pub(super) events: usize,
}

impl EstimatedAmount {
    pub(super) fn label(&self) -> String {
        match self.events {
            0 => "Sin datos".into(),
            _ if self.lower_nanos == self.upper_nanos => format_money(self.lower_nanos),
            _ => format!(
                "{}–{}",
                format_money(self.lower_nanos),
                format_money(self.upper_nanos)
            ),
        }
    }
}

pub(super) fn summarize<'a>(events: impl IntoIterator<Item = &'a UsageEvent>) -> Estimate {
    let mut estimate = Estimate::default();
    for event in events {
        let Some((range, historical)) = evaluate(event) else {
            estimate.unpriced_events += 1;
            continue;
        };
        let amount = if historical {
            &mut estimate.historical
        } else {
            &mut estimate.current
        };
        amount.lower_nanos += range.0;
        amount.upper_nanos += range.1;
        amount.events += 1;
    }
    estimate
}

struct Rate {
    // Thousandths of a USD per million tokens, equivalent to nanodollars per token.
    input: i128,
    cached: i128,
    output: i128,
    effective: Option<(i32, u32, u32)>,
    max_input: Option<i128>,
}

fn rate(agent: UsageAgent, model: &str) -> Option<Rate> {
    let (input, cached, output, effective, max_input) = match (agent, model) {
        // https://openai.com/index/introducing-gpt-5-4/
        // https://developers.openai.com/api/docs/models/gpt-5.4
        (UsageAgent::Codex, "gpt-5.4" | "gpt-5.4-2026-03-05") => {
            (2_500, 250, 15_000, Some((2026, 3, 5)), Some(272_000))
        }
        // Current snapshot: https://developers.openai.com/api/docs/models/gpt-4.1
        (UsageAgent::Codex, "gpt-4.1" | "gpt-4.1-2025-04-14") => (2_000, 500, 8_000, None, None),
        // https://www.anthropic.com/news/claude-sonnet-4-5
        (UsageAgent::Claude, "claude-sonnet-4-5" | "claude-sonnet-4-5-20250929") => {
            (3_000, 300, 15_000, Some((2025, 9, 29)), Some(200_000))
        }
        // https://www.anthropic.com/news/claude-opus-4-5
        (UsageAgent::Claude, "claude-opus-4-5" | "claude-opus-4-5-20251101") => {
            (5_000, 500, 25_000, Some((2025, 11, 24)), None)
        }
        // Current snapshot: https://platform.claude.com/docs/en/about-claude/pricing
        (UsageAgent::Claude, "claude-sonnet-4-6") => (3_000, 300, 15_000, None, Some(200_000)),
        (UsageAgent::Claude, "claude-opus-4-6") => (5_000, 500, 25_000, None, Some(200_000)),
        _ => return None,
    };
    Some(Rate {
        input,
        cached,
        output,
        effective,
        max_input,
    })
}

fn evaluate(event: &UsageEvent) -> Option<((i128, i128), bool)> {
    let rate = rate(event.agent, &event.model)?;
    let date = DateTime::from_timestamp(event.timestamp, 0)?.date_naive();
    let historical = match rate.effective {
        Some((year, month, day)) => {
            if date < NaiveDate::from_ymd_opt(year, month, day)? {
                return None;
            }
            true
        }
        None => false,
    };
    let input = nonnegative(event.input?)?;
    let cached = nonnegative(event.cached?)?;
    let output = nonnegative(event.output?)?;
    if cached > input || rate.max_input.is_some_and(|max| input > max) {
        return None;
    }

    let other_input = input - cached;
    let base = other_input * rate.input + cached * rate.cached + output * rate.output;
    let (lower, upper) = if event.agent == UsageAgent::Claude {
        // Old Claude rows include cache writes in input without isolating their count
        // or TTL. Bound their possible surcharge using the documented 5m/1h rates.
        let (min_written, max_written) = match event.cache_write {
            Some(written) => {
                let written = nonnegative(written)?;
                if written > other_input {
                    return None;
                }
                (written, written)
            }
            None => (0, other_input),
        };
        (
            base + min_written * (rate.input / 4),
            base + max_written * rate.input,
        )
    } else {
        (base, base)
    };
    Some(((lower, upper), historical))
}

fn nonnegative(count: i64) -> Option<i128> {
    u64::try_from(count).ok().map(i128::from)
}

pub(super) struct BillingReport {
    pub(super) provider: &'static str,
    pub(super) start: i64,
    pub(super) end_exclusive: i64,
    pub(super) amounts: BTreeMap<String, i128>,
    pub(super) has_more: bool,
}

#[derive(Deserialize)]
struct OpenAiPage {
    object: String,
    data: Vec<OpenAiBucket>,
    has_more: bool,
}

#[derive(Deserialize)]
struct OpenAiBucket {
    object: String,
    start_time: i64,
    end_time: i64,
    results: Vec<OpenAiCost>,
}

#[derive(Deserialize)]
struct OpenAiCost {
    object: String,
    amount: OpenAiAmount,
}

#[derive(Deserialize)]
struct OpenAiAmount {
    value: f64,
    currency: String,
}

#[derive(Deserialize)]
struct AnthropicPage {
    data: Vec<AnthropicCostRow>,
    has_more: bool,
}

#[derive(Deserialize)]
struct AnthropicCostRow {
    starting_at: String,
    ending_at: String,
    cost: AnthropicCost,
}

#[derive(Deserialize)]
struct AnthropicCost {
    amount: String,
    currency: String,
}

pub(super) fn parse_billing_report(bytes: &[u8]) -> Result<BillingReport> {
    // https://developers.openai.com/api/reference/resources/admin/subresources/organization/subresources/usage/methods/costs
    // https://platform.claude.com/docs/en/manage-claude/usage-cost-analytics-api
    let value: serde_json::Value = serde_json::from_slice(bytes)?;
    let mut report = BillingReport {
        provider: "OpenAI",
        start: i64::MAX,
        end_exclusive: i64::MIN,
        amounts: BTreeMap::new(),
        has_more: false,
    };
    if value["object"] == "page" {
        let page: OpenAiPage = serde_json::from_value(value)?;
        ensure!(page.object == "page");
        report.has_more = page.has_more;
        for bucket in page.data {
            ensure!(bucket.object == "bucket", "Intervalo de OpenAI inválido");
            report.include_period(bucket.start_time, bucket.end_time)?;
            for cost in bucket.results {
                ensure!(
                    cost.object == "organization.costs.result",
                    "El informe debe proceder del endpoint Costs de OpenAI"
                );
                let nanos = (cost.amount.value * 1_000_000_000.0).round();
                ensure!(
                    nanos.is_finite() && nanos.abs() < i64::MAX as f64,
                    "Importe fuera de rango"
                );
                report.include_amount(cost.amount.currency, nanos as i128)?;
            }
        }
    } else {
        let page: AnthropicPage = serde_json::from_value(value)?;
        report.provider = "Anthropic";
        report.has_more = page.has_more;
        for row in page.data {
            report.include_period(
                DateTime::parse_from_rfc3339(&row.starting_at)?.timestamp(),
                DateTime::parse_from_rfc3339(&row.ending_at)?.timestamp(),
            )?;
            report.include_amount(row.cost.currency, cents_to_nanos(&row.cost.amount)?)?;
        }
    }
    ensure!(
        !report.amounts.is_empty(),
        "El informe no contiene importes"
    );
    Ok(report)
}

impl BillingReport {
    fn include_period(&mut self, start: i64, end: i64) -> Result<()> {
        ensure!(
            start < end
                && DateTime::from_timestamp(start, 0).is_some()
                && DateTime::from_timestamp(end, 0).is_some(),
            "Periodo del informe inválido"
        );
        self.start = self.start.min(start);
        self.end_exclusive = self.end_exclusive.max(end);
        Ok(())
    }

    fn include_amount(&mut self, currency: String, nanos: i128) -> Result<()> {
        ensure!(
            currency.len() == 3 && currency.bytes().all(|byte| byte.is_ascii_alphabetic()),
            "Moneda del informe inválida"
        );
        let current = self
            .amounts
            .entry(currency.to_ascii_uppercase())
            .or_default();
        *current = current
            .checked_add(nanos)
            .context("Importe fuera de rango")?;
        Ok(())
    }
}

fn cents_to_nanos(value: &str) -> Result<i128> {
    // Anthropic reports decimal cents with up to six fractional places.
    let (negative, value) = value
        .strip_prefix('-')
        .map_or((false, value), |value| (true, value));
    let (whole, fractional) = value.split_once('.').unwrap_or((value, ""));
    ensure!(
        !whole.is_empty()
            && whole.bytes().all(|byte| byte.is_ascii_digit())
            && fractional.len() <= 6
            && fractional.bytes().all(|byte| byte.is_ascii_digit()),
        "Importe de Anthropic inválido"
    );
    let whole: i128 = whole.parse()?;
    let fractional = format!("{fractional:0<7}").parse::<i128>()?;
    let absolute = whole
        .checked_mul(10_000_000)
        .and_then(|whole| whole.checked_add(fractional))
        .context("Importe fuera de rango")?;
    Ok(if negative { -absolute } else { absolute })
}

pub(super) fn format_money(nanos: i128) -> String {
    let absolute = nanos.unsigned_abs();
    let whole = absolute / 1_000_000_000;
    let fraction = format!("{:09}", absolute % 1_000_000_000);
    let significant = fraction.trim_end_matches('0');
    let digits = &fraction[..significant.len().max(2)];
    let sign = if nanos < 0 { "-" } else { "" };
    format!("{sign}{whole}.{digits}")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn codex(model: &str, year: i32, month: u32, day: u32) -> UsageEvent {
        UsageEvent {
            agent: UsageAgent::Codex,
            timestamp: NaiveDate::from_ymd_opt(year, month, day)
                .unwrap()
                .and_hms_opt(0, 0, 0)
                .unwrap()
                .and_utc()
                .timestamp(),
            model: model.to_owned(),
            input: Some(100_000),
            cached: Some(20_000),
            output: Some(10_000),
            cache_write: None,
            reasoning: Some(5_000),
            api_duration_ms: None,
        }
    }

    #[test]
    fn separates_historical_fallback_and_unpriced_usage() {
        let events = [
            codex("gpt-5.4", 2026, 3, 5),
            codex("gpt-5.4", 2026, 3, 4),
            codex("gpt-4.1", 2026, 3, 5),
            codex("gpt-5.4-unlisted", 2026, 3, 5),
        ];
        let estimate = summarize(&events);
        assert_eq!(estimate.historical.label(), "0.355");
        assert_eq!(estimate.historical.events, 1);
        assert_eq!(estimate.current.label(), "0.25");
        assert_eq!(estimate.current.events, 1);
        assert_eq!(estimate.unpriced_events, 2);
    }

    #[test]
    fn claude_cache_write_range_does_not_assume_ttl_or_fill_missing_tokens() {
        let mut event = codex("claude-sonnet-4-5-20250929", 2026, 3, 5);
        event.agent = UsageAgent::Claude;
        assert_eq!(summarize([&event]).historical.label(), "0.396–0.636");
        event.cache_write = Some(10_000);
        assert_eq!(summarize([&event]).historical.label(), "0.4035–0.426");
        event.cached = None;
        assert_eq!(summarize([&event]).unpriced_events, 1);
    }

    #[test]
    fn openai_billing_keeps_currency_and_pagination_independent() {
        let report = parse_billing_report(
            br#"{
            "object":"page", "has_more":true,
            "data":[{"object":"bucket", "start_time":1730419200, "end_time":1730505600,
            "results":[
                {"object":"organization.costs.result","amount":{"value":0.06,"currency":"usd"}},
                {"object":"organization.costs.result","amount":{"value":-0.01,"currency":"USD"}},
                {"object":"organization.costs.result","amount":{"value":1.5,"currency":"eur"}}
            ]}]
        }"#,
        )
        .unwrap();
        assert_eq!(report.provider, "OpenAI");
        assert_eq!(
            (report.start, report.end_exclusive),
            (1730419200, 1730505600)
        );
        assert!(report.has_more);
        assert_eq!(format_money(report.amounts["USD"]), "0.05");
        assert_eq!(format_money(report.amounts["EUR"]), "1.50");
    }

    #[test]
    fn anthropic_uses_discounted_cents_and_rejects_non_cost_reports() {
        let report = parse_billing_report(
            br#"{
            "has_more":false, "data":[{
                "starting_at":"2026-09-01T00:00:00Z","ending_at":"2026-10-01T00:00:00Z",
                "cost":{"currency":"USD","amount":"41280.000001","list_amount":"51600.000000"}
            }]
        }"#,
        )
        .unwrap();
        assert_eq!(report.provider, "Anthropic");
        assert_eq!(format_money(report.amounts["USD"]), "412.80000001");
        assert!(parse_billing_report(br#"{"object":"page","data":[],"has_more":false}"#).is_err());
    }
}
