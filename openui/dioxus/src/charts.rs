//! Charts: bars, lines and shares of a whole, drawn with the page's own elements so they
//! take its width and its colours. A chart says its values itself where there's room (over
//! each bar, beside each row), and any mark can be tapped for its exact figure.

use dioxus::prelude::*;
use openui::text as t;
use openui::{Node, Value};

use super::{nodes, rich};

/// One series: its name, its values (a missing one is a gap) and the class its colour
/// comes from.
#[derive(Clone, PartialEq)]
struct Series {
    name: String,
    values: Vec<Option<f64>>,
    color: String,
}

impl Series {
    fn at(&self, i: usize) -> Option<f64> {
        self.values.get(i).copied().flatten()
    }
}

/// The colour a series takes: its tone's, or else the palette's, in order.
fn color(tone: &str, index: usize) -> String {
    match tone {
        "accent" => "ct-accent".into(),
        "good" | "success" => "ct-good".into(),
        "bad" | "danger" => "ct-bad".into(),
        "warning" => "ct-warning".into(),
        "violet" => "ct-violet".into(),
        "neutral" => "ct-neutral".into(),
        _ => format!("c{}", index % 8),
    }
}

fn number(v: &Value) -> Option<f64> {
    match v {
        Value::Null => None,
        Value::Str(s) if s.trim().is_empty() => None,
        v => Some(v.num()).filter(|n| n.is_finite()),
    }
}

/// Labels and each series.
fn chart_data(node: &Node) -> (Vec<String>, Vec<Series>) {
    let labels: Vec<String> = node.arg("labels").items().iter().map(Value::text).collect();
    let series = nodes(node.arg("series"), "Series")
        .iter()
        .enumerate()
        .map(|(i, s)| Series {
            name: s.str("name"),
            values: s.arg("values").items().iter().map(number).collect(),
            color: color(&s.str("tone"), i),
        })
        .collect();
    (labels, series)
}

fn legend(series: &[Series]) -> Element {
    rsx! {
        if series.len() > 1 {
            div { class: "oui-legend",
                for s in series {
                    span { span { class: "oui-swatch {s.color}" } span { {rich(&s.name)} } }
                }
            }
        }
    }
}

/// What was tapped, in words.
fn picked_text(
    picked: Option<(usize, usize)>,
    labels: &[String],
    series: &[Series],
) -> Option<String> {
    let (l, s) = picked?;
    let one = series.get(s)?;
    let v = one.at(l)?;
    let label = labels.get(l).cloned().unwrap_or_default();
    Some(if series.len() > 1 {
        format!("{label} · {}: {}", one.name, t::grouped(v))
    } else {
        format!("{label}: {}", t::grouped(v))
    })
}

/// The line over a chart: what its values are in, and what was tapped.
fn top(y_title: &str, picked: Option<String>) -> Element {
    let picked = picked.unwrap_or_default();
    rsx! {
        div { class: "oui-chart-top",
            span { class: "oui-axis-title", "{y_title}" }
            span { class: "oui-readout", "aria-live": "polite", "{picked}" }
        }
    }
}

fn x_title(node: &Node) -> Element {
    let x = node.str("xLabel");
    rsx! { if !x.is_empty() { div { class: "oui-axis-title x", "{x}" } } }
}

fn y_axis(ticks: &[f64], pct: impl Fn(f64) -> f64) -> Element {
    rsx! {
        for tick in ticks {
            div { class: if *tick == 0.0 { "oui-gridline zero" } else { "oui-gridline" }, style: "bottom: {pct(*tick)}%",
                span { "{t::short_num(*tick)}" }
            }
        }
    }
}

/// How many marks a chart can label one by one: few fit on any screen, some only on a
/// wide one, and past that the axis and a tap have to do.
fn density(marks: usize) -> &'static str {
    match marks {
        0..=8 => "few",
        9..=18 => "some",
        _ => "many",
    }
}

#[component]
pub(crate) fn BarChart(node: Node) -> Element {
    let (labels, series) = chart_data(&node);
    let stacked = node.str("variant") == "stacked" && series.len() > 1;
    let mut picked = use_signal(|| None::<(usize, usize)>);
    let n = labels
        .len()
        .max(series.iter().map(|s| s.values.len()).max().unwrap_or(0));
    let (mut lo, mut hi) = (0f64, 0f64);
    for i in 0..n {
        if stacked {
            hi = hi.max(
                series
                    .iter()
                    .filter_map(|s| s.at(i))
                    .map(|v| v.max(0.0))
                    .sum(),
            );
            lo = lo.min(
                series
                    .iter()
                    .filter_map(|s| s.at(i))
                    .map(|v| v.min(0.0))
                    .sum(),
            );
        } else {
            for v in series.iter().filter_map(|s| s.at(i)) {
                hi = hi.max(v);
                lo = lo.min(v);
            }
        }
    }
    let (ticks, bottom, top_of) = t::ticks(lo, hi);
    let pct = move |v: f64| (v - bottom) / (top_of - bottom) * 100.0;
    let zero = pct(0.0);
    let marks = if stacked { n } else { n * series.len().max(1) };
    let dense = density(marks);
    rsx! {
        figure { class: "oui-chart",
            {top(&node.str("yLabel"), picked_text(picked(), &labels, &series))}
            div { class: "oui-plot {dense}",
                {y_axis(&ticks, pct)}
                div { class: "oui-bars",
                    for i in 0..n {
                        div { class: "oui-slot",
                            if stacked {
                                div { class: "oui-col",
                                    {
                                        let (mut up, mut down) = (0.0, 0.0);
                                        let segs: Vec<(usize, f64, f64)> = series.iter().enumerate().filter_map(|(s, one)| {
                                            let v = one.at(i)?;
                                            let (from, to) = if v >= 0.0 { let f = up; up += v; (f, up) } else { let f = down; down += v; (down, f) };
                                            Some((s, pct(from), pct(to) - pct(from)))
                                        }).collect();
                                        let total = up + down;
                                        rsx! {
                                            for (s, b, h) in segs {
                                                button { class: if picked() == Some((i, s)) { "oui-bar on {series[s].color}" } else { "oui-bar {series[s].color}" },
                                                    style: "bottom: {b}%; height: {h}%",
                                                    "aria-label": "{labels.get(i).cloned().unwrap_or_default()} · {series[s].name}: {series[s].at(i).unwrap_or(0.0)}",
                                                    onclick: move |_| picked.set(Some((i, s))),
                                                }
                                            }
                                            if !series.iter().all(|s| s.at(i).is_none()) {
                                                span { class: "oui-bar-v", style: "bottom: {pct(up)}%", "{t::short_num(total)}" }
                                            }
                                        }
                                    }
                                }
                            } else {
                                for (s, one) in series.iter().enumerate() {
                                    div { class: "oui-col",
                                        if let Some(v) = one.at(i) {
                                            {
                                                let (b, h) = if v >= 0.0 { (zero, pct(v) - zero) } else { (pct(v), zero - pct(v)) };
                                                let on = picked() == Some((i, s));
                                                let below = v < 0.0;
                                                rsx! {
                                                    button { class: if on { "oui-bar on {one.color}" } else { "oui-bar {one.color}" },
                                                        class: if below { "down" },
                                                        style: "bottom: {b}%; height: {h.max(0.4)}%",
                                                        "aria-label": "{labels.get(i).cloned().unwrap_or_default()}: {v}",
                                                        onclick: move |_| picked.set(Some((i, s))),
                                                    }
                                                    if below {
                                                        span { class: "oui-bar-v down", style: "top: {100.0 - b}%", "{t::short_num(v)}" }
                                                    } else {
                                                        span { class: "oui-bar-v", style: "bottom: {b + h}%", "{t::short_num(v)}" }
                                                    }
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
            div { class: "oui-xlabels", for l in labels.iter() { span { title: "{l}", {rich(l)} } } }
            {x_title(&node)}
            {legend(&series)}
        }
    }
}

#[component]
pub(crate) fn HBarChart(node: Node) -> Element {
    let (labels, series) = chart_data(&node);
    let stacked = node.str("variant") == "stacked" && series.len() > 1;
    let at = |s: &Series, i: usize| s.at(i).unwrap_or(0.0).max(0.0);
    let n = labels.len();
    let longest = (0..n)
        .map(|i| {
            if stacked {
                series.iter().map(|s| at(s, i)).sum::<f64>()
            } else {
                series.iter().map(|s| at(s, i)).fold(0.0, f64::max)
            }
        })
        .fold(0.0, f64::max)
        .max(f64::MIN_POSITIVE);
    rsx! {
        figure { class: "oui-chart",
            div { class: "oui-hbars",
                for i in 0..n {
                    div { class: "oui-hrow",
                        div { class: "oui-hlabel", {rich(&labels[i])} }
                        div { class: "oui-htrack",
                            if stacked {
                                div { class: "oui-hbar-line",
                                    div { class: "oui-hbar-space",
                                        for s in series.iter() {
                                            div { class: "oui-hbar {s.color}", title: "{s.name}: {t::grouped(at(s, i))}", style: "width: {at(s, i) / longest * 100.0}%" }
                                        }
                                    }
                                    span { class: "oui-num", "{t::short_num(series.iter().map(|s| at(s, i)).sum())}" }
                                }
                            } else {
                                for s in series.iter() {
                                    div { class: "oui-hbar-line",
                                        div { class: "oui-hbar-space",
                                            div { class: "oui-hbar {s.color}", style: "width: {at(s, i) / longest * 100.0}%" }
                                        }
                                        span { class: "oui-num", if s.at(i).is_some() { "{t::short_num(at(s, i))}" } }
                                    }
                                }
                            }
                        }
                    }
                }
            }
            {legend(&series)}
        }
    }
}

/// Which of `n` labels along an axis are written when at most `room` fit, so they don't
/// run into each other: all of a few, every second or third of many, and always the first.
fn written(n: usize, room: usize) -> Vec<bool> {
    let every = n.div_ceil(room).max(1);
    let mut out: Vec<bool> = (0..n).map(|i| i % every == 0).collect();
    // The last, too, when it's at least half a step past the last one that's written.
    if n > 1 {
        let past = (n - 1) % every;
        if past > 0 && past * 2 >= every {
            out[n - 1] = true;
        }
    }
    out
}

#[component]
pub(crate) fn LineChart(node: Node) -> Element {
    let (labels, series) = chart_data(&node);
    let area = node.name == "AreaChart";
    let mut picked = use_signal(|| None::<(usize, usize)>);
    let n = labels
        .len()
        .max(series.iter().map(|s| s.values.len()).max().unwrap_or(0));
    let all: Vec<f64> = series
        .iter()
        .flat_map(|s| s.values.iter().copied().flatten())
        .collect();
    let lo = all.iter().copied().fold(f64::INFINITY, f64::min);
    let hi = all.iter().copied().fold(f64::NEG_INFINITY, f64::max);
    let (lo, hi) = if all.is_empty() { (0.0, 1.0) } else { (lo, hi) };
    // A line far from zero is read by its shape: the axis starts near it. An area is read
    // by its size, so it starts at zero.
    let near = !area && lo > 0.0 && (hi - lo) < hi * 0.5;
    let (ticks, bottom, top_of) = if near {
        t::ticks_near(lo, hi)
    } else {
        t::ticks(lo.min(0.0), hi)
    };
    let y = move |v: f64| (v - bottom) / (top_of - bottom) * 100.0;
    let x = move |i: usize| {
        if n <= 1 {
            50.0
        } else {
            i as f64 / (n - 1) as f64 * 100.0
        }
    };
    let floor = bottom.max(0.0).min(top_of);
    // A wide chart has room for a dozen labels, a narrow one for four or five.
    let (wide, narrow) = (written(n, 12), written(n, 5));
    let dense = if series.len() == 1 {
        density(n)
    } else {
        "many"
    };
    rsx! {
        figure { class: "oui-chart",
            {top(&node.str("yLabel"), picked_text(picked(), &labels, &series))}
            div { class: "oui-plot line {dense}",
                {y_axis(&ticks, y)}
                svg { class: "oui-lines-svg", view_box: "0 0 100 100", preserve_aspect_ratio: "none", "aria-hidden": "true",
                    for s in series.iter() {
                        {
                            // The runs of points between its gaps.
                            let mut runs: Vec<Vec<(usize, f64)>> = vec![Vec::new()];
                            for i in 0..s.values.len() {
                                match s.at(i) {
                                    Some(v) => runs.last_mut().unwrap().push((i, v)),
                                    None if !runs.last().unwrap().is_empty() => runs.push(Vec::new()),
                                    None => {}
                                }
                            }
                            rsx! {
                                for run in runs.into_iter().filter(|r| !r.is_empty()) {
                                    {
                                        let line = run.iter().map(|(i, v)| format!("{:.3},{:.3}", x(*i), 100.0 - y(*v))).collect::<Vec<_>>().join(" ");
                                        let (first, last) = (run[0].0, run[run.len() - 1].0);
                                        let fill = format!("{:.3},{:.3} {line} {:.3},{:.3}", x(first), 100.0 - y(floor), x(last), 100.0 - y(floor));
                                        rsx! {
                                            if area { polygon { class: "oui-area {s.color}", points: "{fill}" } }
                                            polyline { class: "oui-line-path {s.color}", points: "{line}" }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
                for (si, s) in series.iter().enumerate() {
                    for i in 0..s.values.len() {
                        if let Some(v) = s.at(i) {
                            button { class: if picked() == Some((i, si)) { "oui-dot on {s.color}" } else { "oui-dot {s.color}" },
                                style: "left: {x(i)}%; bottom: {y(v)}%",
                                "aria-label": "{labels.get(i).cloned().unwrap_or_default()}: {v}",
                                onclick: move |_| picked.set(Some((i, si))),
                                span { class: "oui-dot-v", "{t::short_num(v)}" }
                            }
                        }
                    }
                }
            }
            div { class: "oui-xlabels at",
                for (i, l) in labels.iter().enumerate() {
                    if wide.get(i).copied().unwrap_or(false) {
                        span {
                            class: if i == 0 { "first" } else if i + 1 == n { "last" } else { "" },
                            class: if !narrow.get(i).copied().unwrap_or(false) { "wide" },
                            style: "left: {x(i)}%",
                            title: "{l}",
                            {rich(l)}
                        }
                    }
                }
            }
            {x_title(&node)}
            {legend(&series)}
        }
    }
}

#[component]
pub(crate) fn PieChart(node: Node) -> Element {
    let labels: Vec<String> = node.arg("labels").items().iter().map(Value::text).collect();
    let values: Vec<f64> = node
        .arg("values")
        .items()
        .iter()
        .map(|v| v.num().max(0.0))
        .collect();
    let donut = node.str("variant") == "donut";
    let total: f64 = values.iter().sum();
    let mut picked = use_signal(|| None::<usize>);
    let mut a0 = -std::f64::consts::FRAC_PI_2;
    let slices: Vec<(usize, String)> = values
        .iter()
        .enumerate()
        .map(|(i, v)| {
            let sweep = if total > 0.0 {
                v / total * std::f64::consts::TAU
            } else {
                0.0
            };
            let path = arc(a0, a0 + sweep, if donut { 0.62 } else { 0.0 });
            a0 += sweep;
            (i, path)
        })
        .collect();
    // What the middle of a donut says: the slice that's picked, or the whole.
    let middle = match picked().and_then(|i| values.get(i).map(|v| (i, *v))) {
        Some((i, v)) => (t::short_num(v), labels.get(i).cloned().unwrap_or_default()),
        None => (t::short_num(total), "total".to_string()),
    };
    rsx! {
        figure { class: "oui-chart oui-pie",
            div { class: "oui-pie-plot",
                svg { view_box: "-1.05 -1.05 2.1 2.1", class: "oui-pie-svg", role: "img",
                    for (i, d) in slices {
                        path { class: if picked() == Some(i) { "on c{i % 8}" } else { "c{i % 8}" }, d: "{d}",
                            onclick: move |_| picked.set(if picked() == Some(i) { None } else { Some(i) }),
                        }
                    }
                }
                if donut {
                    div { class: "oui-pie-middle",
                        strong { "{middle.0}" }
                        span { "{middle.1}" }
                    }
                }
            }
            div { class: "oui-pie-legend",
                for (i, l) in labels.iter().enumerate() {
                    {
                        let v = values.get(i).copied().unwrap_or(0.0);
                        let share = if total > 0.0 { v / total * 100.0 } else { 0.0 };
                        rsx! {
                            button { class: if picked() == Some(i) { "on" } else { "" },
                                onclick: move |_| picked.set(if picked() == Some(i) { None } else { Some(i) }),
                                span { class: "oui-swatch c{i % 8}" }
                                span { class: "oui-pie-label", {rich(l)} }
                                span { class: "oui-num", "{t::short_num(v)} · {share:.0}%" }
                            }
                        }
                    }
                }
            }
        }
    }
}

/// An SVG path for the slice from angle `a0` to `a1` of a unit circle, a ring when `inner`
/// is above 0.
fn arc(a0: f64, a1: f64, inner: f64) -> String {
    // A whole circle can't be one arc.
    let a1 = if a1 - a0 >= std::f64::consts::TAU {
        a0 + std::f64::consts::TAU - 1e-4
    } else {
        a1
    };
    let large = if a1 - a0 > std::f64::consts::PI { 1 } else { 0 };
    let (x0, y0, x1, y1) = (a0.cos(), a0.sin(), a1.cos(), a1.sin());
    if inner <= 0.0 {
        format!("M0 0 L{x0:.4} {y0:.4} A1 1 0 {large} 1 {x1:.4} {y1:.4} Z")
    } else {
        let r = inner;
        format!(
            "M{:.4} {:.4} L{x0:.4} {y0:.4} A1 1 0 {large} 1 {x1:.4} {y1:.4} L{:.4} {:.4} A{r} {r} 0 {large} 0 {:.4} {:.4} Z",
            x0 * r,
            y0 * r,
            x1 * r,
            y1 * r,
            x0 * r,
            y0 * r
        )
    }
}
