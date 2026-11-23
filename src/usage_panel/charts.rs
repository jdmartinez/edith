use std::collections::BTreeMap;

use chrono::DateTime;
use gpui::{Context, Div, Window, div, prelude::*, px, rgb};

use super::{Metric, Totals, card};

fn chart_gap(days: i64) -> Div {
    let mut gap = div().flex_1().min_w_0();
    gap.style().flex_grow = Some(days as f32);
    gap
}

pub(super) fn daily_chart(
    days: &BTreeMap<i64, Totals>,
    first_day: Option<i64>,
    current_day: i64,
) -> Div {
    let first = first_day.unwrap_or_else(|| *days.first_key_value().unwrap().0);
    let last = first_day.map_or_else(|| *days.last_key_value().unwrap().0, |_| current_day);
    let peak_input = days
        .range(first..)
        .filter_map(|(_, time)| time.input.sum)
        .max();
    let peak_output = days
        .range(first..)
        .filter_map(|(_, time)| time.output.sum)
        .max();
    let bars = days
        .iter()
        .scan(first, |next, (&day, totals)| {
            let gap = day - *next;
            *next = day + 1;
            Some((day, gap, totals))
        })
        .flat_map(|(day, gap, totals)| {
            let filler = (gap > 0).then(|| chart_gap(gap));
            let input = totals.input.sum;
            let output = totals.output.sum;
            let tooltip = format!(
                "{} UTC · Entrada {} · Salida {}",
                date_label(day),
                totals.input.label(),
                totals.output.label()
            );
            let bar = div()
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
                );
            let bar = bar
                .id(("usage-day", day as usize))
                .tooltip(move |_, cx| cx.new(|_| ChartTooltip(tooltip.clone())).into());
            filler
                .into_iter()
                .map(IntoElement::into_any_element)
                .chain(std::iter::once(bar.into_any_element()))
        });
    let trailing = last - *days.last_key_value().unwrap().0;
    card()
        .child(div().text_lg().child("Tokens por día (UTC)"))
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
        .child(
            div()
                .flex()
                .children(bars)
                .when(trailing > 0, |row| row.child(chart_gap(trailing))),
        )
        .child(
            div()
                .flex()
                .justify_between()
                .text_sm()
                .text_color(rgb(0xaab8cc))
                .child(date_label(first))
                .child(if first == last {
                    String::new()
                } else {
                    date_label(last)
                }),
        )
}

fn date_label(day: i64) -> String {
    DateTime::from_timestamp(day * 86_400, 0)
        .map(|date| date.format("%d/%m/%Y").to_string())
        .unwrap_or_default()
}

fn bar_ratio(amount: Option<u64>, peak: Option<u64>) -> f32 {
    amount.unwrap_or_default() as f32 / peak.unwrap_or_default().max(1) as f32
}

fn peak_label(peak: Option<u64>) -> String {
    peak.map_or_else(|| "Sin datos".into(), |peak| peak.to_string())
}

pub(super) fn heatmap(hours: &BTreeMap<(u32, u32), Totals>) -> Div {
    card()
        .child(
            div()
                .text_lg()
                .child("Tokens por día de la semana y hora (UTC)"),
        )
        .child(hour_grid("Entrada", hours, |totals| totals.input, 0x87e0cd))
        .child(hour_grid("Salida", hours, |totals| totals.output, 0xf0b687))
}

fn hour_grid(
    label: &'static str,
    hours: &BTreeMap<(u32, u32), Totals>,
    select: impl Fn(&Totals) -> Metric,
    color: u32,
) -> Div {
    let peak = hours.values().filter_map(|totals| select(totals).sum).max();
    let week = ["Lun", "Mar", "Mié", "Jue", "Vie", "Sáb", "Dom"];
    div()
        .flex()
        .flex_col()
        .gap_1()
        .child(div().child(format!("{label} · máx. {}", peak_label(peak))))
        .child(
            div()
                .flex()
                .gap_1()
                .child(div().w(px(32.0)).flex_none())
                .children((0..24).map(|hour| {
                    div()
                        .flex_1()
                        .min_w_0()
                        .text_center()
                        .text_xs()
                        .child(if hour % 3 == 0 {
                            format!("{hour:02}")
                        } else {
                            String::new()
                        })
                })),
        )
        .children(week.into_iter().enumerate().map(|(day, name)| {
            div()
                .flex()
                .gap_1()
                .child(div().w(px(32.0)).flex_none().text_xs().child(name))
                .children((0..24).map(|hour| {
                    let metric = hours
                        .get(&(day as u32, hour))
                        .map(&select)
                        .unwrap_or_default();
                    let mut fill = gpui::Hsla::from(rgb(color));
                    fill.a = 0.15 + 0.85 * bar_ratio(metric.sum, peak);
                    let tooltip = format!("{name} {hour:02}:00 UTC · {label} {}", metric.label());
                    div()
                        .id((label, day * 24 + hour as usize))
                        .tooltip(move |_, cx| cx.new(|_| ChartTooltip(tooltip.clone())).into())
                        .flex_1()
                        .min_w_0()
                        .h(px(14.0))
                        .rounded_sm()
                        .bg(if metric.sum.is_some() {
                            fill
                        } else {
                            rgb(0x273242).into()
                        })
                }))
        }))
}

struct ChartTooltip(String);

impl Render for ChartTooltip {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
            .p_2()
            .rounded_md()
            .bg(rgb(0x273242))
            .text_color(rgb(0xd9e0ee))
            .text_sm()
            .child(self.0.clone())
    }
}
