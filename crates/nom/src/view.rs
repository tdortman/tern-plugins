//! The state as a Tern card.

use std::{
    collections::{BTreeMap, HashSet},
    fmt::Write as _,
    time::Duration,
};

use tern_sdk::{
    nodes,
    ui::{self, Bound, Column, Extent, Status, TableRow},
};

use crate::{
    diff::Package,
    flake::{Lock, LockChange},
    nix::{store_name, strip_ansi},
    state::{Counts, DepNode, DrvStatus, Outcome, Severity, State},
};

/// What a click on the card asks for.
#[derive(Clone, Debug)]
pub enum Msg {
    /// Shows or hides this `.drv`'s log.
    ToggleLog(String),
    /// Opens this file in a Tern file block.
    OpenFile(String),
}

/// A link to `drv`'s whole log file. A click opens it while nom runs; after
/// nom exited, Tern opens it itself on ⌘-click or from its context menu.
fn full_log(state: &State, drv: &str, key: &str) -> Option<Node> {
    let path = state.full_logs.as_ref()?.get(drv)?;
    let path = path.to_string_lossy().into_owned();
    Some(
        ui::text(ui::span("Full log").style("link"))
            .href(format!("file://{path}"))
            .on_click(Msg::OpenFile(path))
            .key(key)
            .into(),
    )
}

/// `drv`'s log file as a link target, once it printed.
fn log_href(state: &State, drv: &str) -> Option<String> {
    let path = state.full_logs.as_ref()?.get(drv)?;
    Some(format!("file://{}", path.display()))
}

pub type Node = ui::Node<Msg>;
type View = ui::View<Msg>;

/// Running copies listed before the rest fold into "+N more".
const ROWS: usize = 8;

/// How long a build or copy runs before it gets a row.
const SETTLE: Duration = Duration::from_secs(1);

/// Build log lines sent to the window.
const LOG_TAIL: usize = 60;

/// Flake inputs shown while nix fetches them: the current one and the last
/// few done, so the card does not grow with every input.
const INPUT_ROWS: usize = 4;

/// Lines of a clicked build's log shown before "N more lines" expands it.
const PREVIEW: u32 = 8;

/// Lines of a clicked build's log sent, so expanding shows a longer tail.
const OPEN_TAIL: usize = 200;

/// `n` as the nearest `f64`, for progress ratios and sizes.
fn approx(n: u64) -> f64 {
    let hi = u32::try_from(n >> 32).unwrap_or(u32::MAX);
    let lo = u32::try_from(n & u64::from(u32::MAX)).unwrap_or(u32::MAX);
    f64::from(hi).mul_add(4_294_967_296.0, f64::from(lo))
}

/// `lines` for Tern's `ansi` element: each with colors reset, `\r\n`-ended.
fn ansi_text(lines: impl IntoIterator<Item = impl std::fmt::Display>) -> String {
    let mut out = String::new();
    for l in lines {
        let _ = write!(out, "{l}\x1b[0m\r\n");
    }
    out
}

/// `bytes` as KiB/MiB/GiB.
fn size(bytes: u64) -> String {
    let kib = approx(bytes) / 1024.0;
    if kib < 1024.0 {
        format!("{kib:.0} KiB")
    } else if kib < 1024.0 * 1024.0 {
        format!("{:.1} MiB", kib / 1024.0)
    } else {
        format!("{:.2} GiB", kib / (1024.0 * 1024.0))
    }
}

/// `d` as `1m 05s` / `4.2s`.
fn took(d: Duration) -> String {
    let s = d.as_secs();
    if s >= 60 {
        format!("{}m {:02}s", s / 60, s % 60)
    } else {
        format!("{:.1}s", d.as_secs_f64())
    }
}

/// Milliseconds of `d`, as a node prop.
fn ms(d: Duration) -> i64 {
    i64::try_from(d.as_millis()).unwrap_or(i64::MAX)
}

/// `done/expected label` while running, `done label` once finished (nix
/// expects builds it then finds already valid), plus the failed count.
fn count_badges(c: Counts, label: &str, key: &str, finished: bool) -> Vec<Node> {
    let mut out = Vec::new();
    if (c.expected == 0 || finished) && c.done == 0 && c.failed == 0 {
        return out;
    }
    let tone = if c.failed > 0 {
        "error"
    } else if finished || c.done == c.expected {
        "success"
    } else {
        "neutral"
    };
    let text = if finished {
        format!("{} {label}", c.done)
    } else {
        format!("{}/{} {label}", c.done, c.expected)
    };
    if !finished || c.done > 0 {
        out.push(ui::badge(text).tone(tone).key(format!("{key}.n")).into());
    }
    if c.failed > 0 {
        out.push(
            ui::badge(format!("{} failed", c.failed))
                .tone("error")
                .key(format!("{key}.f"))
                .into(),
        );
    }
    out
}

/// The header row: activity word, counts, timer.
fn summary(state: &State) -> Node {
    let mut row: Vec<Node> = Vec::new();
    match state.outcome {
        None => {
            let word = if !state.builds.is_empty() {
                "Building"
            } else if !state.copies.is_empty() {
                "Fetching"
            } else if state.inputs.last().is_some_and(|i| !i.done) {
                "Fetching inputs"
            } else {
                "Evaluating"
            };
            row.push(ui::spinner().style("dots").label(word).key("act").into());
        }
        Some((outcome, _)) => {
            let (icon, tone, label) = match outcome {
                Outcome::Done => ("check", "success", "Finished"),
                Outcome::Failed => ("x", "error", "Failed"),
                Outcome::Cancelled => ("stop", "warning", "Cancelled"),
            };
            row.push(ui::icon(icon).tone(tone).key("act").into());
            row.push(
                ui::text(ui::span(label).style("strong"))
                    .key("label")
                    .into(),
            );
        }
    }
    let finished = state.outcome.is_some();
    row.extend(count_badges(state.build_counts(), "built", "b", finished));
    row.extend(count_badges(state.copy_counts(), "fetched", "c", finished));
    // While running, the download bar below says this.
    let (fetched_bytes, _) = state.copy_bytes();
    if finished && fetched_bytes > 0 {
        row.push(
            ui::text(ui::span(format!("↓ {}", size(fetched_bytes))).style("muted"))
                .key("bytes")
                .into(),
        );
    }
    let warnings: usize = state
        .notes
        .iter()
        .filter(|n| n.severity == Severity::Warning)
        .map(|n| n.count)
        .sum();
    if warnings > 0 {
        let label = if warnings == 1 {
            "1 warning".to_owned()
        } else {
            format!("{warnings} warnings")
        };
        row.push(ui::badge(label).tone("warning").key("warn").into());
    }
    row.push(ui::spacer().grow(1.0).key("sp").into());
    let mut timer = ui::elapsed().age(ms(state.started.elapsed()));
    if let Some((_, d)) = state.outcome {
        timer = timer.stopped(u64::try_from(d.as_millis()).unwrap_or(u64::MAX));
    }
    row.push(timer.key("t").into());
    ui::row()
        .gap("sm")
        .align("center")
        .key("sum")
        .children(row)
        .into()
}

/// Everything nix did outside builds, copies and inputs, as a log: its last
/// lines while nix runs, then folded with a link to all of it in a file.
fn activity(state: &State) -> Option<Node> {
    if state.activity_log.is_empty() {
        return None;
    }
    // Faint like the rest of the card's secondary text; store paths cut
    // down to the package name, line by line so the text only grows.
    let text = format!("\x1b[2m{}", shorten(&state.activity_log));
    // Tern clips an `ansi` box instead of scrolling it, so the box shows
    // the tail and the file block has the rest. No preview: its fade would
    // cover the top lines.
    let tail = if state.outcome.is_none() { 4.0 } else { 12.0 };
    let log = ui::ansi(text)
        .follow(true)
        .max(Bound::h(Extent::Lines(tail)))
        .key("log");
    if state.outcome.is_none() {
        return Some(log.key("activity").into());
    }
    let lines = state.activity_log.matches("\r\n").count();
    let mut body: Vec<Node> = vec![log.into()];
    body.extend(full_log(state, crate::logs::ACTIVITY, "full"));
    Some(
        ui::section()
            .head(format!("Activity · {lines}"))
            .collapsible(true)
            .collapsed(true)
            .key("activity")
            .children(body)
            .into(),
    )
}

/// Builds or copies that have run long enough to get a row: the many that
/// end within a second would only flash in and out.
fn settled<'a, T>(state: &State, running: &'a BTreeMap<u64, T>) -> Vec<(u64, &'a T)> {
    running
        .iter()
        .filter(|(id, _)| state.outcome.is_some() || state.started_ago(**id) >= SETTLE)
        .map(|(&id, t)| (id, t))
        .collect()
}

/// One derivation's row under its tree guides, then its planned inputs in
/// `keep` (all of them without it).
/// While it builds, the row has its phase, last line, estimate bar and timer;
/// a click shows its log under the row.
fn dep_rows(
    state: &State,
    node: &DepNode<'_>,
    (guide, indent): (&str, &str),
    open: Option<&str>,
    keep: Option<&HashSet<&str>>,
    rows: &mut Vec<Node>,
) {
    let drv = node.drv;
    let status = state.status(drv);
    let ended = state.outcome.is_some();
    let mut row: Vec<Node> = Vec::new();
    if !guide.is_empty() {
        row.push(
            ui::text(ui::span(guide).style("dim"))
                .wrap("none")
                .shrink(0.0)
                .key("g")
                .into(),
        );
    }
    let (indicator, style): (Node, &str) = match status {
        // Still running when the invocation ended: interrupted.
        DrvStatus::Building(_) if ended => {
            (ui::icon("stop").tone("warning").key("s").into(), "warning")
        }
        DrvStatus::Building(_) => (
            ui::spinner()
                .style("braille")
                .tone("accent")
                .key("s")
                .into(),
            "accent",
        ),
        DrvStatus::Waiting if ended => (ui::icon("minus").tone("muted").key("s").into(), "muted"),
        DrvStatus::Waiting => (ui::icon("clock").tone("muted").key("s").into(), "muted"),
        DrvStatus::Built(_) => (ui::icon("check").tone("success").key("s").into(), "success"),
        DrvStatus::Failed => (ui::icon("x").tone("error").key("s").into(), "error"),
    };
    row.push(indicator);
    let mut name = ui::span(store_name(drv)).style(format!("{style} strong"));
    if let Some(href) = log_href(state, drv) {
        name = name.href(href);
    }
    // One line per row keeps the guides joined; a long name is cut after the
    // last line gave way, with the whole name as its tooltip.
    row.push(
        ui::text(name)
            .wrap("none")
            .truncate("end")
            .shrink(1.0)
            .title(store_name(drv))
            .key("n")
            .into(),
    );
    let detail = match status {
        DrvStatus::Building(id) => {
            building_cells(state, id, &mut row);
            None
        }
        DrvStatus::Waiting if ended => Some("not built".to_owned()),
        DrvStatus::Waiting => state.estimate(drv, None).map(|e| format!("~{}", took(e))),
        DrvStatus::Built(d) => Some(took(d)),
        DrvStatus::Failed => Some("failed".to_owned()),
    };
    if let Some(detail) = detail {
        row.push(
            ui::text(ui::span(detail).style("muted"))
                .wrap("none")
                .shrink(0.0)
                .key("d")
                .into(),
        );
    }
    let mut node_row = ui::row().gap("sm").align("center").key(drv).children(row);
    if matches!(status, DrvStatus::Building(_)) || state.logs.contains_key(drv) {
        let shown = open == Some(drv);
        node_row = node_row
            .title(if shown {
                "Hide the log"
            } else {
                "Show the log"
            })
            .on_click(Msg::ToggleLog(drv.to_owned()));
        if shown {
            node_row = node_row.mark("pick");
        }
    }
    rows.push(node_row.into());
    if open == Some(drv) {
        let lines = state.logs.get(drv);
        let skip = lines.map_or(0, |l| l.len().saturating_sub(OPEN_TAIL));
        let text = ansi_text(lines.into_iter().flatten().skip(skip));
        rows.push(
            ui::ansi(text)
                .follow(true)
                .preview(PREVIEW)
                .max(Bound::h(Extent::Lines(30.0)))
                .key(format!("{drv}.log"))
                .into(),
        );
        rows.extend(full_log(state, drv, &format!("{drv}.full")));
    }
    let kids: Vec<_> = node
        .children
        .iter()
        .filter(|k| keep.is_none_or(|keep| keep.contains(k.drv)))
        .collect();
    for (i, kid) in kids.iter().enumerate() {
        let (branch, rest) = if i + 1 == kids.len() {
            ("└─ ", "   ")
        } else {
            ("├─ ", "│  ")
        };
        let guides = (&*format!("{indent}{branch}"), &*format!("{indent}{rest}"));
        dep_rows(state, kid, guides, open, keep, rows);
    }
}

/// Adds to `keep` every derivation under `node`, itself included, that is
/// building or failed or needs one that is; whether `node` was added.
fn mark_active<'a>(state: &State, node: &DepNode<'a>, keep: &mut HashSet<&'a str>) -> bool {
    let mut active = matches!(
        state.status(node.drv),
        DrvStatus::Building(_) | DrvStatus::Failed
    );
    for kid in &node.children {
        active |= mark_active(state, kid, keep);
    }
    if active {
        keep.insert(node.drv);
    }
    active
}

/// A running build's phase, machine, last line, estimate bar and timer.
fn building_cells(state: &State, id: u64, row: &mut Vec<Node>) {
    let Some(b) = state.builds.get(&id) else {
        return;
    };
    if let Some(phase) = &b.phase {
        row.push(ui::badge(phase).tone("info").key("p").into());
    }
    if let Some(machine) = &b.machine {
        row.push(ui::badge(machine).tone("muted").key("m").into());
    }
    let line = b.last_line.as_deref().unwrap_or("");
    row.push(
        ui::text(ui::span(line).style("muted"))
            .truncate("end")
            .wrap("none")
            .grow(1.0)
            .shrink(100.0)
            .key("l")
            .into(),
    );
    let elapsed = state.started_ago(id);
    // nom's estimate from earlier builds of the same package.
    if let Some(est) = b.estimate {
        let label = match est.checked_sub(elapsed) {
            Some(left) => format!("~{} left", took(left)),
            None => format!("over ~{}", took(est)),
        };
        row.push(
            ui::progress()
                .value((elapsed.as_secs_f64() / est.as_secs_f64()).min(0.99))
                .label(label)
                .tone("accent")
                .min(Bound::w(Extent::Ch(24.0)))
                .key("e")
                .into(),
        );
    }
    let mut timer = ui::elapsed().age(ms(elapsed));
    if state.outcome.is_some() {
        timer = timer.stopped(u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX));
    }
    row.push(timer.shrink(0.0).key("t").into());
}

/// One row per running copy with its own progress bar.
fn running_copies(state: &State) -> Option<Node> {
    let copies = settled(state, &state.copies);
    if copies.is_empty() {
        return None;
    }
    let mut rows: Vec<Node> = Vec::new();
    for &(id, c) in copies.iter().take(ROWS) {
        let mut bar = ui::progress().tone("info").grow(1.0).key("b");
        if c.expected > 0 {
            bar = bar
                .value(approx(c.done) / approx(c.expected))
                .label(format!("{} / {}", size(c.done), size(c.expected)));
        }
        let from = c
            .from
            .as_deref()
            .map_or("", |f| f.trim_start_matches("https://"));
        rows.push(
            ui::row()
                .gap("sm")
                .align("center")
                .key(id.to_string())
                .children(nodes![
                    ui::icon("download").tone("info").key("i"),
                    ui::text(ui::span(&c.name).style("strong"))
                        .wrap("char")
                        .shrink(1.0)
                        .key("n"),
                    ui::text(ui::span(from).style("muted"))
                        .truncate("start")
                        .wrap("none")
                        .shrink(100.0)
                        .key("f"),
                    bar,
                ])
                .into(),
        );
    }
    if copies.len() > ROWS {
        rows.push(
            ui::text(ui::span(format!("+{} more", copies.len() - ROWS)).style("muted"))
                .key("more")
                .into(),
        );
    }
    Some(ui::col().key("copies").children(rows).into())
}

/// Builds as one bar, beside the download bar for fetched paths: nix fetches
/// and builds at the same time, so the two never share a bar.
fn progress(state: &State) -> Option<Node> {
    if state.outcome.is_some() {
        return None;
    }
    let b = state.build_counts();
    if b.expected == 0 {
        return None;
    }
    let mut label = format!("{}/{} built", b.done, b.expected);
    if b.running > 0 {
        let _ = write!(label, " · {} running", b.running);
    }
    Some(
        ui::row()
            .gap("sm")
            .align("center")
            .key("bar")
            .children(nodes![
                ui::icon("wrench").tone("accent").key("i"),
                ui::progress()
                    .value(approx(b.done + b.failed) / approx(b.expected))
                    .label(label)
                    .tone("accent")
                    .grow(1.0)
                    .key("b"),
            ])
            .into(),
    )
}

/// All copies as one bar, in unpacked bytes like nix's own `copied (x/y MiB)`.
fn downloads(state: &State) -> Option<Node> {
    if state.outcome.is_some() {
        return None;
    }
    let (done, total) = state.copy_bytes();
    if total == 0 {
        return None;
    }
    let paths = state.copy_counts();
    let label = format!(
        "{} / {} · {}/{} fetched",
        size(done),
        size(total),
        paths.done,
        paths.expected
    );
    Some(
        ui::row()
            .gap("sm")
            .align("center")
            .key("dl")
            .children(nodes![
                ui::icon("download").tone("info").key("i"),
                ui::progress()
                    .value(approx(done) / approx(total))
                    .label(label)
                    .tone("info")
                    .grow(1.0)
                    .key("b"),
            ])
            .into(),
    )
}

/// `text` with store paths cut down to the name and the path inside it:
/// `/nix/store/<hash>-source/lib/x.nix` reads `source/lib/x.nix`.
fn shorten(text: &str) -> String {
    let is_path = |c: u8| c.is_ascii_alphanumeric() || b"/._+-".contains(&c);
    let b = text.as_bytes();
    let mut out = String::with_capacity(text.len());
    let (mut copied, mut i) = (0, 0);
    while i + 34 <= b.len() {
        let hash = &b[i + 1..i + 33];
        if b[i] == b'/'
            && b[i + 33] == b'-'
            && hash
                .iter()
                .all(|c| c.is_ascii_digit() || c.is_ascii_lowercase())
        {
            // Back over the store directory to where the path starts.
            let mut start = i;
            while start > copied && is_path(b[start - 1]) {
                start -= 1;
            }
            // A store path is absolute; this skips the `1m` of a colour code glued to it.
            while b[start] != b'/' {
                start += 1;
            }
            out.push_str(&text[copied..start]);
            copied = i + 34;
            i = copied;
        } else {
            i += 1;
        }
    }
    out.push_str(&text[copied..]);
    out
}

/// The `.drv` and exit code of a builder failure, in Lix's and nix's wording.
fn builder_failure(plain: &str) -> Option<(&str, &str)> {
    let (drv, rest) = if let Some((_, rest)) = plain.split_once("builder for '") {
        rest.split_once('\'')?
    } else {
        plain.split_once("Cannot build '")?.1.split_once('\'')?
    };
    let code = rest.split_once("exit code ")?.1;
    let end = code
        .find(|c: char| !c.is_ascii_digit())
        .unwrap_or(code.len());
    Some((drv, &code[..end]))
}

/// Errors: a builder failure as its name, exit code and its own log tail;
/// anything else as nix printed it, with store paths shortened.
fn errors(state: &State) -> Vec<Node> {
    let mut out = Vec::new();
    for (i, note) in state.notes.iter().enumerate() {
        if note.severity != Severity::Error {
            continue;
        }
        let plain = strip_ansi(&note.text);
        let Some((drv, code)) = builder_failure(&plain) else {
            let text = shorten(&note.text).replace("\n       ", "\n  ");
            out.push(
                ui::ansi(text.replace('\n', "\r\n"))
                    .tone("error")
                    .key(format!("e{i}"))
                    .into(),
            );
            continue;
        };
        // nix quotes the last lines with `> `; the log kept here has more.
        let tail: Vec<&str> = match state.logs.get(drv) {
            Some(log) => log
                .iter()
                .skip(log.len().saturating_sub(LOG_TAIL))
                .map(String::as_str)
                .collect(),
            None => plain
                .lines()
                .filter_map(|l| l.trim_start().strip_prefix("> "))
                .collect(),
        };
        let mut body = nodes![
            ui::row()
                .gap("sm")
                .align("center")
                .key("h")
                .children(nodes![
                    ui::icon("x").tone("error").key("i"),
                    ui::text(vec![
                        ui::span(store_name(drv)).style("error strong"),
                        ui::span(format!(" failed with exit code {code}")).style("error"),
                    ])
                    .key("t"),
                ]),
        ];
        if !tail.is_empty() {
            let text = ansi_text(&tail);
            body.push(
                ui::ansi(text)
                    .follow(true)
                    .max(Bound::h(Extent::Lines(20.0)))
                    .key("log")
                    .into(),
            );
        }
        body.push(
            ui::text(vec![
                ui::span("nix log ").style("muted"),
                ui::span(drv).style("code"),
            ])
            .key("hint")
            .into(),
        );
        body.extend(full_log(state, drv, "full"));
        out.push(
            ui::col()
                .gap("xs")
                .tone("error")
                .role("nom.failure")
                .key(format!("e{i}"))
                .children(body)
                .into(),
        );
    }
    out
}

/// `text` with quoted code as code spans: Markdown's `` `x` `` and the
/// GNU-style `` `x' `` that NixOS option warnings use.
fn code_spans(text: &str) -> Vec<ui::Span> {
    let mut spans = Vec::new();
    let mut rest = text;
    while let Some(open) = rest.find('`') {
        let after = &rest[open + 1..];
        let Some(close) = after.find(['`', '\'']) else {
            break;
        };
        if open > 0 {
            spans.push(ui::span(&rest[..open]));
        }
        spans.push(ui::span(&after[..close]).style("code"));
        rest = &after[close + 1..];
    }
    if !rest.is_empty() {
        spans.push(ui::span(rest));
    }
    spans
}

/// Warnings, deduplicated, one line each, in a folded section.
fn warnings(state: &State) -> Option<Node> {
    let rows: Vec<Node> = state
        .notes
        .iter()
        .enumerate()
        .filter(|(_, n)| n.severity == Severity::Warning)
        .map(|(i, n)| {
            let plain = shorten(&strip_ansi(&n.text));
            let message = plain
                .trim_start()
                .trim_start_matches("evaluation warning:")
                .trim_start_matches("warning:")
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" ");
            let mut spans = code_spans(&message);
            if n.count > 1 {
                spans.push(ui::span(format!("  ×{}", n.count)).style("muted"));
            }
            ui::row()
                .gap("sm")
                .key(format!("w{i}"))
                .children(nodes![
                    ui::icon("warn").tone("warning").key("i"),
                    ui::text(spans).grow(1.0).key("t")
                ])
                .into()
        })
        .collect();
    if rows.is_empty() {
        return None;
    }
    Some(
        ui::section()
            .head(ui::span("Warnings").style("warning"))
            .collapsible(true)
            .collapsed(true)
            .key("warnings")
            .children(rows)
            .into(),
    )
}

/// What nix builds, as a tree from what was asked for down to its inputs.
/// Until a run succeeds, only what matters now shows under the roots:
/// running and failed builds and the derivations that need them.
fn deps(state: &State, open: Option<&str>) -> Option<Node> {
    let roots = state.dependency_tree();
    if roots.is_empty() {
        return None;
    }
    let succeeded = matches!(state.outcome, Some((Outcome::Done, _)));
    let keep = (!succeeded).then(|| {
        let mut keep = HashSet::new();
        for root in &roots {
            mark_active(state, root, &mut keep);
        }
        keep
    });
    let mut rows = Vec::new();
    for root in &roots {
        dep_rows(state, root, ("", ""), open, keep.as_ref(), &mut rows);
    }
    let built = state.built.iter().filter(|b| !b.failed).count();
    let head = format!("Derivations · {built}/{} built", state.planned.len());
    Some(
        ui::section()
            .head(head)
            .collapsible(true)
            .collapsed(succeeded)
            .key("deps")
            .child(ui::col().key("t").children(rows))
            .into(),
    )
}

/// Paths fetched from caches, folded.
fn fetched(state: &State) -> Option<Node> {
    if state.copied.is_empty() {
        return None;
    }
    let rows: Vec<Node> = state
        .copied
        .iter()
        .enumerate()
        .map(|(i, c)| {
            ui::row()
                .gap("sm")
                .key(format!("c{i}"))
                .children(nodes![
                    ui::icon("download").tone("info").key("i"),
                    ui::text(&c.name).key("n").grow(1.0),
                    ui::text(ui::span(size(c.bytes)).style("muted")).key("s"),
                ])
                .into()
        })
        .collect();
    let bytes: u64 = state.copied.iter().map(|c| c.bytes).sum();
    Some(
        ui::section()
            .head(format!(
                "Fetched · {} paths, {}",
                state.copied.len(),
                size(bytes)
            ))
            .collapsible(true)
            .collapsed(true)
            .key("fetched")
            .children(rows)
            .into(),
    )
}

/// Output nix printed outside any build, folded.
fn output(state: &State) -> Option<Node> {
    let lines = state.logs.get("").filter(|l| !l.is_empty())?;
    let skip = lines.len().saturating_sub(LOG_TAIL);
    let text = ansi_text(lines.iter().skip(skip));
    Some(
        ui::section()
            .head("Output")
            .collapsible(true)
            .collapsed(true)
            .key("log")
            .child(
                ui::ansi(text)
                    .follow(true)
                    .max(Bound::h(Extent::Lines(12.0)))
                    .key("out"),
            )
            .into(),
    )
}

/// The flake inputs nix fetched, one per row: done ones checked, the current
/// one with its download. While nix runs only the last few show, so the
/// card keeps its height; once it ends they fold into one line.
fn fetch_inputs(state: &State) -> Option<Node> {
    if state.inputs.is_empty() {
        return None;
    }
    let running = state.outcome.is_none();
    let skip = if running {
        state.inputs.len().saturating_sub(INPUT_ROWS)
    } else {
        0
    };
    let done = state.inputs.iter().filter(|i| i.done || !running).count();
    let rows: Vec<Node> = state
        .inputs
        .iter()
        .enumerate()
        .skip(skip)
        .map(|(i, input)| {
            let input_done = input.done || !running;
            let (kind, rest) = input
                .reference
                .split_once(':')
                .unwrap_or(("", &input.reference));
            let indicator: Node = if input_done {
                ui::icon("check").tone("success").key("s").into()
            } else {
                ui::spinner()
                    .style("braille")
                    .tone("accent")
                    .key("s")
                    .into()
            };
            let mut row = nodes![
                indicator,
                ui::text(vec![
                    ui::span(format!("{kind}:")).style("dim"),
                    ui::span(rest).style(if input_done { "muted" } else { "strong" }),
                ])
                .truncate("end")
                .wrap("none")
                .shrink(1.0)
                .key("n"),
            ];
            // nix unpacks and hashes a source after downloading it, silently;
            // a moment after the download ends, the row says so.
            let unpacking = input
                .downloaded
                .is_some_and(|t| t.elapsed() >= Duration::from_secs(1));
            if input_done {
            } else if unpacking {
                let text = if input.bytes > 0 {
                    format!("{} · unpacking", size(input.bytes))
                } else {
                    "unpacking".to_owned()
                };
                row.push(ui::spacer().grow(1.0).key("sp").into());
                row.push(ui::text(ui::span(text).style("muted")).key("b").into());
            } else if input.expected > 0 {
                row.push(
                    ui::progress()
                        .value(approx(input.bytes) / approx(input.expected))
                        .label(format!("{} / {}", size(input.bytes), size(input.expected)))
                        .tone("info")
                        .grow(1.0)
                        .key("b")
                        .into(),
                );
            } else if input.bytes > 0 {
                row.push(ui::spacer().grow(1.0).key("sp").into());
                row.push(
                    ui::text(ui::span(size(input.bytes)).style("num"))
                        .key("b")
                        .into(),
                );
            }
            ui::row()
                .gap("sm")
                .align("center")
                .key(i.to_string())
                .children(row)
                .into()
        })
        .collect();
    Some(
        ui::section()
            .head(format!("Inputs · {done} fetched"))
            .collapsible(!running)
            .collapsed(!running)
            .key("inputs")
            .children(rows)
            .into(),
    )
}

/// One side of a lock change: revision and date, with the source when it
/// differs from the row's.
fn lock_cell(lock: Option<&Lock>, source: &str, style: &str) -> Vec<ui::Span> {
    let Some(Lock::Pinned {
        source: own,
        rev,
        date,
    }) = lock
    else {
        return vec![ui::span("—").style("dim")];
    };
    let mut spans = Vec::new();
    if own != source {
        spans.push(ui::span(format!("{own} ")).style("muted"));
    }
    if let Some(rev) = rev {
        spans.push(ui::span(rev).style(style));
    }
    if let Some(date) = date {
        spans.push(ui::span(format!(" {date}")).style("dim"));
    }
    if spans.is_empty() {
        spans.push(ui::span("—").style("dim"));
    }
    spans
}

/// Inputs a lock file update added, updated or removed, as a table: the
/// source once, then the revision and its date before and after.
fn lock_changes(state: &State) -> Option<Node> {
    if state.lock_changes.is_empty() {
        return None;
    }
    let rank = |c: &&LockChange| match c.verb.as_str() {
        "Updated" => 0,
        "Added" => 1,
        _ => 2,
    };
    let mut changes: Vec<&LockChange> = state.lock_changes.iter().collect();
    changes.sort_by_key(rank);
    let mut table = ui::table()
        .key("t")
        .col(Column::new("s", "").priority(1.0))
        .col(Column::new("n", "Input").truncate("start").priority(5.0))
        .col(
            Column::new("src", "Source")
                .grow(1.0)
                .truncate("middle")
                .priority(2.0),
        )
        .col(Column::new("o", "Before").priority(3.0))
        .col(Column::new("w", "After").priority(4.0));
    let mut counts: Vec<(String, usize)> = Vec::new();
    for (i, c) in changes.iter().enumerate() {
        match counts.iter_mut().find(|(verb, _)| *verb == c.verb) {
            Some((_, n)) => *n += 1,
            None => counts.push((c.verb.clone(), 1)),
        }
        let tone = match c.verb.as_str() {
            "Added" => "success",
            "Removed" => "error",
            _ => "accent",
        };
        // A nested input reads by its own name; the path to it is context.
        let mut name = Vec::new();
        if let Some((parent, leaf)) = c.input.rsplit_once('/') {
            name.push(ui::span(format!("{parent}/")).style("dim"));
            name.push(ui::span(leaf).style("strong"));
        } else {
            name.push(ui::span(&c.input).style("strong"));
        }
        let lock = c.to.as_ref().or(c.from.as_ref());
        let (source, source_cell) = match lock {
            Some(Lock::Pinned { source, .. }) => {
                (source.as_str(), vec![ui::span(source).style("muted")])
            }
            Some(Lock::Follows(target)) => (
                "",
                vec![
                    ui::span("follows ").style("dim"),
                    ui::span(target).style("muted"),
                ],
            ),
            None => ("", vec![ui::span("—").style("dim")]),
        };
        table = table.row(
            TableRow::new(i.to_string())
                .cell("s", ui::span(c.verb.to_lowercase()).style(tone))
                .cell("n", name)
                .cell("src", source_cell)
                .cell("o", lock_cell(c.from.as_ref(), source, "muted"))
                .cell("w", lock_cell(c.to.as_ref(), source, "")),
        );
    }
    let summary: Vec<String> = counts
        .iter()
        .map(|(verb, n)| format!("{n} {}", verb.to_lowercase()))
        .collect();
    Some(
        ui::section()
            .head(format!("Lock file · {}", summary.join(", ")))
            .key("lock")
            .child(table)
            .into(),
    )
}

/// `bytes` with a sign, as dix prints size changes.
fn signed_size(bytes: i64) -> String {
    let sign = if bytes < 0 { '−' } else { '+' };
    format!("{sign}{}", size(bytes.unsigned_abs()))
}

/// How many leading bytes `a` and `b` share, ending after a `.`, `-` or `_`
/// so a version is split between its parts, never inside a number.
fn shared_prefix(a: &str, b: &str) -> usize {
    let mut last = 0;
    for ((i, x), y) in a.char_indices().zip(b.chars()) {
        if x != y {
            break;
        }
        if matches!(x, '.' | '-' | '_' | ' ') {
            last = i + 1;
        }
    }
    last
}

/// One side of a version change: the part both sides share dimmed, the
/// rest in `tone`.
fn version_cell(this: &str, other: &str, tone: &str) -> Vec<ui::Span> {
    if this.is_empty() {
        return vec![ui::span("—").style("dim")];
    }
    let cut = if this == other {
        this.len()
    } else {
        shared_prefix(this, other)
    };
    let (same, changed) = this.split_at(cut);
    let mut spans = Vec::new();
    if !same.is_empty() {
        spans.push(ui::span(same).style("muted"));
    }
    if !changed.is_empty() {
        spans.push(ui::span(changed).style(tone));
    }
    spans
}

/// Packages as a table: status, name, version before, version after, size.
fn package_table(packages: &[&Package], key: &str) -> Node {
    let mut table = ui::table()
        .key(key)
        .col(Column::new("s", "").priority(1.0))
        .col(Column::new("n", "Package").truncate("end").priority(5.0))
        .col(
            Column::new("o", "Before")
                .grow(1.0)
                .truncate("middle")
                .priority(2.0),
        )
        .col(
            Column::new("w", "After")
                .grow(1.0)
                .truncate("middle")
                .priority(4.0),
        )
        .col(Column::new("d", "Size").align("end").priority(3.0));
    for (i, p) in packages.iter().enumerate() {
        let (word, tone) = match p.status.as_str() {
            "Upgraded" => ("upgraded", "accent"),
            "Downgraded" => ("downgraded", "warning"),
            "Added" => ("added", "success"),
            "Removed" => ("removed", "error"),
            "Mixed" => ("mixed", "warning"),
            _ => ("changed", "muted"),
        };
        let (old, new) = p.versions();
        table = table.row(
            TableRow::new(i.to_string())
                .cell("s", ui::span(word).style(tone))
                .cell("n", ui::span(&p.name).style("strong"))
                .cell("o", version_cell(&old, &new, "del"))
                .cell("w", version_cell(&new, &old, "ins"))
                .cell("d", ui::span(signed_size(p.size_delta)).style("num")),
        );
    }
    table.into()
}

/// What the build changes against the current system, like nh's dix diff:
/// the system's own packages and everything added or removed, then the
/// dependencies that only changed version or size in their own section.
fn changes(state: &State) -> Option<Node> {
    let closure = state.closure.as_ref()?;
    // dix's order: changed, then added, then removed.
    let rank = |p: &&Package| match p.status.as_str() {
        "Added" => 1,
        "Removed" => 2,
        _ => 0,
    };
    let (mut picked, mut deps): (Vec<&Package>, Vec<&Package>) = closure
        .diffs
        .iter()
        .partition(|p| p.selected() || matches!(p.status.as_str(), "Added" | "Removed"));
    picked.sort_by_key(rank);
    deps.sort_by_key(rank);
    let p = &closure.paths;
    let delta = i64::try_from(closure.size_new).unwrap_or(i64::MAX)
        - i64::try_from(closure.size_old).unwrap_or(i64::MAX);
    let mut body = nodes![
        ui::text(vec![
            ui::span(format!("{} → {} paths", p.old, p.new)).style("muted"),
            ui::span(format!(" (+{}, −{})", p.added, p.removed)).style("dim"),
            ui::span(format!(
                " · {} → {} ",
                size(closure.size_old),
                size(closure.size_new)
            ))
            .style("muted"),
            ui::span(signed_size(delta)).style("num"),
        ])
        .key("sum"),
    ];
    if !picked.is_empty() {
        body.push(package_table(&picked, "pkgs"));
    }
    if !deps.is_empty() {
        body.push(
            ui::section()
                .head(format!("Updated dependencies · {}", deps.len()))
                .collapsible(true)
                .collapsed(false)
                .key("deps")
                .child(package_table(&deps, "t"))
                .into(),
        );
    }
    let head = match picked.len() {
        0 if deps.is_empty() => "Changes · none".to_owned(),
        1 => "Changes · 1 package".to_owned(),
        n => format!("Changes · {n} packages"),
    };
    Some(
        ui::section()
            .head(head)
            .key("changes")
            .children(body)
            .into(),
    )
}

/// The whole card; `open` is the `.drv` whose log the user opened.
pub fn view(state: &State, title: &str, open: Option<&str>) -> View {
    let status = match state.outcome {
        None => Status::Running,
        Some((Outcome::Done, _)) => Status::Done,
        Some((Outcome::Failed, _)) => Status::Error,
        Some((Outcome::Cancelled, _)) => Status::Cancelled,
    };
    let mut body: Vec<Node> = vec![summary(state)];
    body.extend(activity(state));
    body.extend(state.flake.iter().flat_map(crate::flake::Output::view));
    body.extend(fetch_inputs(state));
    body.extend(lock_changes(state));
    body.extend(changes(state));
    body.extend(deps(state, open));
    body.extend(running_copies(state));
    body.extend(errors(state));
    body.extend(warnings(state));
    body.extend(fetched(state));
    body.extend(output(state));
    body.extend(progress(state));
    body.extend(downloads(state));
    View::new().main([ui::card()
        .head(ui::span(title).style("mono"))
        .status(status)
        .role("nom.card")
        .key("nom")
        .children(body)])
}

#[cfg(test)]
mod tests {
    use super::*;

    const HASH: &str = "8nk26gfn51s07v339n5lycablfkl5lj7";

    #[test]
    fn versions_split_between_parts() {
        fn split<'a>(a: &'a str, b: &str) -> (&'a str, &'a str) {
            a.split_at(shared_prefix(a, b))
        }
        assert_eq!(split("8.12.36", "8.12.40"), ("8.12.", "36"));
        // `1.2` against `1.20` shares `1.2` but not a whole part.
        assert_eq!(split("1.2", "1.20"), ("1.", "2"));
        assert_eq!(
            split("3.14.8-env ×7", "3.14.8-env ×8"),
            ("3.14.8-env ", "×7")
        );
        assert_eq!(
            split("2026.10.22-27dcf5f", "2026.10.19-6bbf2c4"),
            ("2026.10.", "22-27dcf5f")
        );
        assert_eq!(split("1.0", "2.0"), ("", "1.0"));
    }

    #[test]
    fn shortens_store_paths_in_quotes_and_after_unicode() {
        let warning = format!(
            "warning: defined in `/nix/store/{HASH}-nix/systems/vm/default.nix' → `/x/{HASH}-a.drv`"
        );
        assert_eq!(
            shorten(&warning),
            "warning: defined in `nix/systems/vm/default.nix' → `a.drv`"
        );
        assert_eq!(shorten("no paths here"), "no paths here");
        // The colour code before the path stays whole.
        let coloured = format!("derivation '\x1b[35;1m/nix/store/{HASH}-linux.drv\x1b[0m'");
        assert_eq!(
            shorten(&coloured),
            "derivation '\x1b[35;1mlinux.drv\x1b[0m'"
        );
    }

    /// `node` in wire form, without its handlers.
    fn wire(node: &Node) -> serde_json::Value {
        let kids: Vec<_> = node.child_nodes().iter().map(wire).collect();
        serde_json::json!({ "k": node.kind(), "p": node.props(), "c": kids })
    }

    /// The derivations section's rows as plain text.
    fn tree_rows(state: &State) -> Vec<String> {
        let tree = deps(state, None).expect("a plan");
        let node = ui::Node::<()>::from_json(&wire(&tree)).expect("valid");
        let text = tern_sdk::plain(node, 100);
        text.lines()
            .skip(1)
            .map(|l| l.trim_end().to_owned())
            .collect()
    }

    #[test]
    fn tree_shows_what_matters_until_the_run_succeeds() {
        use crate::state::tests::{SUCCESS, feed};
        // `c` builds after its inputs `a` and `b` were built.
        let upto = SUCCESS
            .lines()
            .position(|l| l.contains(r#""text":"building '"#) && l.contains("-c-"))
            .expect("c builds");
        let running: Vec<&str> = SUCCESS.lines().take(upto + 1).collect();
        let rows = tree_rows(&feed(&running.join("\n")));
        assert_eq!(rows.len(), 2, "{rows:#?}");
        assert_eq!(rows[0], "  top-18439");
        assert!(rows[1].starts_with("  └─  ... c-18439"), "{rows:#?}");

        let mut state = feed(SUCCESS);
        state.finish(Outcome::Done);
        assert_eq!(
            tree_rows(&state)
                .iter()
                .map(|r| r.split(" 0.").next().unwrap_or_default())
                .collect::<Vec<_>>(),
            [
                "  top-18439",
                "  └─  c-18439",
                "     ├─  a-18439",
                "     └─  b-18439",
            ]
        );
    }

    #[test]
    fn quotes_code_in_markdown_and_gnu_style() {
        let spans = code_spans("The option `a.b' in `x.nix' is now `c.d`.");
        let code: Vec<_> = spans
            .iter()
            .filter(|s| s.s.as_deref() == Some("code"))
            .map(|s| s.t.as_str())
            .collect();
        assert_eq!(code, ["a.b", "x.nix", "c.d"]);
        assert_eq!(spans.last().map(|s| s.t.as_str()), Some("."));
        // An unclosed quote stays text.
        assert_eq!(code_spans("it`s").len(), 1);
    }

    #[test]
    fn finds_builder_failures_in_lix_and_nix_wording() {
        let lix = format!(
            "error: builder for '/nix/store/{HASH}-c.drv' failed with exit code 3;\n  last 5 log lines:"
        );
        assert_eq!(
            builder_failure(&lix),
            Some((format!("/nix/store/{HASH}-c.drv").as_str(), "3"))
        );
        let nix = format!(
            "error: Cannot build '/nix/store/{HASH}-c.drv'.\n       Reason: builder failed with exit code 12."
        );
        assert_eq!(
            builder_failure(&nix),
            Some((format!("/nix/store/{HASH}-c.drv").as_str(), "12"))
        );
        assert_eq!(
            builder_failure("error: 1 dependencies of derivation failed to build"),
            None
        );
    }
}
