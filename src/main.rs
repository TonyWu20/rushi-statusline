//! The reference `statusline` extension, Rust port
//! (ui-extension-plan stage 4). The bash reference is
//! ui_extensions/statusline/.
//!
//! One long-lived process. It answers every `tick` op with a status
//! row. The row is a powerline footer: rounded pill segments with
//! Nerd Font glyphs (the left hard divider U+E0B6 caps each pill;
//! the right hard divider U+E0B4 joins the pills and closes the
//! line). The palette is Catppuccin Macchiato, the starship-statusline
//! reference's. Each pill is one or more styled spans on the wire
//! (docs/ui-extension.md section 4); the host draws the spans left
//! to right on one row.
//!
//! The row shows:
//! - live dir: the config dir, where the TUI and the loop operate
//!   ($CONFIG is exported by the host)
//! - git branch + dirty mark, TTL-cached at 3 s so a tick never
//!   spawns git more than once per 3 s (the design tick-cost note)
//! - model from the tick payload. The session name and the loop
//!   state stay in the host's top bar (frame title); the footer
//!   does not duplicate them
//! - cumulative usage summed over assistant_message.usage and
//!   compaction_summary.usage events. The summary call's usage
//!   joins the totals; the context fullness metric stays on the
//!   last assistant_message input (docs/auto-compact-plan.md
//!   section 4.6)
//! - the numbers shorten to k/M/B, the reference fmtNum rule, with
//!   a trailing .0 dropped (5500 -> 5.5k, 5000 -> 5k, 1.2M)
//!   the host re-sends every usage-bearing event of the listed
//!   kinds at start, so the totals survive a TUI restart from the
//!   log alone
//! - context fullness, the number to watch for compaction: the last
//!   measured request input tokens over the model window, in the
//!   starship-statusline style (ctx <pct>% (<tokens>/<window>)). The
//!   window is the active model's context_tokens from $CONFIG; the
//!   metric is the last usage event's input_tokens, not the
//!   cumulative totals. The section hides until both are known. Both
//!   numbers shorten with the same k/M/B rule.
//!
//! The footer does NOT dump the ext_status values the tick payload
//! carries: shared UI state (model_thinking, loop_phase, other
//! extensions' state) is host presentation (the input-area border,
//! the working row), not statusline content. A 2026-09-02 revision
//! dropped the generic k=v pill dump, which also carried a missing
//! separator between two pills and showed the thinking level as a
//! bare number. The `statuses` map stays on the wire for consumers
//! that want it.
//!
//! Layout: one line on wide terminals, two lines when the terminal
//! is narrow (width under 100). The host reserves one terminal row
//! per line. Line 1 carries dir, git, and model; line 2 carries the
//! stats pill alone. Overflow drops the model pill first, then the
//! git pill; the dir and stats pills never drop, so the token
//! indicator keeps its space.

use serde_json::{json, Value};
use std::io::{BufRead, Write};
use std::process::Command;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// The git cache TTL: a tick never spawns git more than once per
/// interval (the design's tick-cost note).
const GIT_TTL: Duration = Duration::from_secs(3);

/// Nerd Font glyphs: the left hard divider caps a pill; the right
/// hard divider joins the pills and closes the line.
const SEP_L: &str = "\u{E0B6}";
const SEP_R: &str = "\u{E0B4}";
// Catppuccin Macchiato, the starship-statusline reference palette.
// The backgrounds match the reference: dark base/surface pills, one
// light mauve pill for the model.
const DIR_BG: &str = "24273a"; // base
const GIT_BG: &str = "363a4f"; // surface0
const MODEL_BG: &str = "c6a0f6"; // mauve
const STATS_BG: &str = "494d64"; // surface1
const TXT: &str = "cad3f5"; // text, on dark backgrounds
const TXT_DARK: &str = "1e2030"; // mantle, on light backgrounds

/// The shared git state: branch, dirty count, last refresh time.
type GitCache = (String, u64, Option<Instant>);

/// The live dir: the config dir, where the TUI and the loop operate
/// ($CONFIG is exported by the host; unset means the current dir).
fn live_dir() -> String {
    match std::env::var("CONFIG") {
        Ok(p) => std::path::Path::new(&p)
            .parent()
            .map(|d| d.to_string_lossy().into_owned())
            .unwrap_or_else(|| ".".to_string()),
        Err(_) => ".".to_string(),
    }
}

/// Refresh the git branch + dirty count. The TTL check spawns
/// nothing; the refresh runs in a background thread, so a tick
/// reply never waits on a slow git (a cold cache can take seconds,
/// and the staleness bound is 3 x tick_ms). The thread writes the
/// shared cache; the next tick shows the last finished state.
fn git_refresh(dir: &str, cache: &Arc<Mutex<GitCache>>) {
    let now = Instant::now();
    let stale = {
        let c = cache.lock().unwrap();
        c.2.map(|last| now.duration_since(last) >= GIT_TTL)
            .unwrap_or(true)
    };
    if !stale {
        return;
    }
    {
        let mut c = cache.lock().unwrap();
        c.2 = Some(now);
    }
    let dir = dir.to_string();
    let cache = cache.clone();
    std::thread::spawn(move || {
        let branch = Command::new("git")
            .arg("-C")
            .arg(&dir)
            .arg("rev-parse")
            .arg("--abbrev-ref")
            .arg("HEAD")
            .output();
        let status = Command::new("git")
            .arg("-C")
            .arg(&dir)
            .arg("status")
            .arg("--porcelain")
            .output();
        let mut c = cache.lock().unwrap();
        if let Ok(b) = branch {
            if b.status.success() {
                c.0 = String::from_utf8_lossy(&b.stdout).trim().to_string();
            }
        }
        if let Ok(s) = status {
            if s.status.success() {
                c.1 = String::from_utf8_lossy(&s.stdout)
                    .lines()
                    .filter(|l| !l.trim().is_empty())
                    .count() as u64;
            }
        }
    });
}

/// Truncate the dir to the last 16 chars with a leading ellipsis.
fn short_dir(dir: &str) -> String {
    let chars: Vec<char> = dir.chars().collect();
    if chars.len() > 16 {
        format!(
            "...{}",
            chars[chars.len() - 16..].iter().collect::<String>()
        )
    } else {
        dir.to_string()
    }
}

/// One powerline segment: the pill body text and its colors.
#[derive(Debug, Clone)]
struct Seg {
    text: String,
    fg: &'static str,
    bg: &'static str,
}

/// One wire span: a `[text, style]` pair. An empty fg or bg is a
/// terminal default.
fn span(text: &str, fg: &str, bg: &str, bold: bool) -> Value {
    let mut style = serde_json::Map::new();
    if !fg.is_empty() {
        style.insert("fg".into(), json!(format!("#{fg}")));
    }
    if !bg.is_empty() {
        style.insert("bg".into(), json!(format!("#{bg}")));
    }
    if bold {
        style.insert("bold".into(), json!(true));
    }
    json!([text, style])
}

/// The span list of one pill row from the segments, in display
/// order. The row is [cap, body, arrow, cap, body, ..., endcap];
/// the cap and arrow colors follow the reference.
fn row_spans(segs: &[Seg]) -> Vec<Value> {
    let mut out: Vec<Value> = Vec::new();
    let mut prev_bg: &str = "";
    for s in segs {
        if !prev_bg.is_empty() {
            out.push(span(SEP_R, prev_bg, s.bg, false));
        }
        out.push(span(SEP_L, s.bg, s.bg, false));
        out.push(span(&format!(" {} ", s.text), s.fg, s.bg, true));
        prev_bg = s.bg;
    }
    out.push(span(SEP_R, prev_bg, "", false));
    out
}

/// The column count of one row: each segment is one cap (1) plus
/// its body (text length plus two spaces), plus one arrow per join
/// and the end cap (2 per segment).
fn row_cols(segs: &[Seg]) -> usize {
    segs.iter().map(|s| s.text.chars().count() + 4).sum()
}

/// Fit a row to `w` columns: drop the lowest-priority tail segments
/// until it fits. The head segment never drops.
fn fit(segs: &[Seg], w: usize) -> Vec<Seg> {
    let mut keep = segs.to_vec();
    while row_cols(&keep) > w && keep.len() > 1 {
        keep.pop();
    }
    keep
}

/// k/M/B abbreviation, the starship-statusline fmtNum rule, with a
/// trailing .0 dropped: 5500 -> 5.5k, 5000 -> 5k, 1200000 -> 1.2M,
/// 1500000000 -> 1.5B.
fn fmt_num(n: u64) -> String {
    if n < 1000 {
        return n.to_string();
    }
    let (scale, unit) = if n < 1_000_000 {
        (1000.0, "k")
    } else if n < 1_000_000_000 {
        (1_000_000.0, "M")
    } else {
        (1_000_000_000.0, "B")
    };
    let v = n as f64 / scale;
    let s = format!("{v:.1}");
    if s.ends_with(".0") {
        format!("{}{unit}", s.trim_end_matches(".0"))
    } else {
        format!("{s}{unit}")
    }
}

/// The context-fullness section, starship-statusline style:
/// `ctx <pct>% (<tokens>/<window>)`. Both numbers need to be
/// known, so the section hides until the last usage event and the
/// config window are both in hand.
fn ctx_text(last_in: u64, window: Option<u64>) -> Option<String> {
    let w = window?;
    if last_in == 0 || w == 0 {
        return None;
    }
    let pct10 = last_in.saturating_mul(1000) / w;
    let pct = format!("{}.{}", pct10 / 10, pct10 % 10);
    Some(format!("ctx:{pct}% ({}/{})", fmt_num(last_in), fmt_num(w)))
}

/// The active model's context window from $CONFIG (config.toml):
/// a `[model."<name>"]` section carrying a plain `context_tokens`
/// key. `None` when the model or the value is unknown: the ctx
/// section hides.
fn ctx_window_of(model: &str) -> Option<u64> {
    let config = std::env::var("CONFIG").ok()?;
    let raw = std::fs::read_to_string(config).ok()?;
    let mut in_section = false;
    let mut found: Option<u64> = None;
    for line in raw.lines() {
        let t = line.trim();
        if let Some(rest) = t.strip_prefix("[model.") {
            let name = rest.split(']').next().unwrap_or("");
            in_section = name.trim_matches('"') == model;
            found = None;
            continue;
        }
        if t.starts_with('[') {
            in_section = false;
            continue;
        }
        if in_section {
            if let Some(v) = t.strip_prefix("context_tokens") {
                let v = v
                    .trim_start()
                    .trim_start_matches('=')
                    .trim()
                    .trim_matches('"')
                    .trim();
                found = v.parse().ok();
            }
        }
    }
    found
}

fn main() {
    let dir = live_dir();
    let mut in_total: u64 = 0;
    let mut out_total: u64 = 0;
    let mut cached_total: u64 = 0;
    // The last request's measured input tokens. Context fullness
    // rides on this value, not the cumulative totals.
    let mut last_in: u64 = 0;
    // The context window cache, keyed on the model name. The model
    // name comes from the tick payload; the window is the model's
    // context_tokens in $CONFIG. A model change re-parses; the same
    // model reuses the value.
    let mut ctx_model = String::new();
    let mut ctx_window: Option<u64> = None;
    // The git cache starts "fresh": the first tick skips the git
    // spawn and the row shows git:none; the first TTL refresh lands
    // about 3 s in, on a background thread. A tick reply never
    // waits on a slow git (the staleness bound is 3 x tick_ms).
    let git: Arc<Mutex<GitCache>> = Arc::new(Mutex::new((String::new(), 0, Some(Instant::now()))));

    let stdin = std::io::stdin();
    let stdout = std::io::stdout();
    let mut out = std::io::LineWriter::new(stdout.lock());
    for line in stdin.lock().lines() {
        let Ok(line) = line else {
            break;
        };
        let Ok(v) = serde_json::from_str::<Value>(&line) else {
            // A malformed line: no state change, no reply.
            continue;
        };
        match v.get("op").and_then(|o| o.as_str()) {
            Some("event") => {
                // Cumulative usage: the host re-sends every
                // usage-bearing message of the listed kinds at
                // start, so the totals survive a TUI restart from
                // the log alone. The compaction_summary usage joins
                // the cumulative totals. It does not move the
                // context fullness metric: the ctx section reads
                // the last assistant_message input only
                // (docs/auto-compact-plan.md section 4.6).
                let ev = &v["event"];
                let ty = ev.get("type").and_then(|t| t.as_str()).unwrap_or("");
                if ty == "assistant_message" || ty == "compaction_summary" {
                    if let Some(usage) = ev.get("usage").and_then(|u| u.as_object()) {
                        in_total += usage
                            .get("input_tokens")
                            .and_then(|x| x.as_u64())
                            .unwrap_or(0);
                        out_total += usage
                            .get("output_tokens")
                            .and_then(|x| x.as_u64())
                            .unwrap_or(0);
                        cached_total += usage
                            .get("cached_tokens")
                            .and_then(|x| x.as_u64())
                            .unwrap_or(0);
                        if ty == "assistant_message" {
                            last_in = usage
                                .get("input_tokens")
                                .and_then(|x| x.as_u64())
                                .unwrap_or(0);
                        }
                    }
                }
            }
            Some("tick") => {
                git_refresh(&dir, &git);
                let (branch, dirty) = {
                    let c = git.lock().unwrap();
                    (c.0.clone(), c.1)
                };
                let width = v.get("width").and_then(|w| w.as_u64()).unwrap_or(80) as usize;
                let model = v
                    .get("model")
                    .and_then(|m| m.as_str())
                    .filter(|m| !m.is_empty())
                    .unwrap_or("no-model");
                // The tick also carries session, loop_running,
                // thinking, and the ext_status statuses map. The
                // footer does not consume them: the host top bar
                // and the input-area border show that state.

                // The context window is keyed on the model name from
                // the tick. The model rarely changes; the re-parse
                // only fires on a switch.
                if model != ctx_model {
                    ctx_window = ctx_window_of(model);
                    ctx_model = model.to_string();
                }
                let git_txt = if branch.is_empty() {
                    "git:none".to_string()
                } else {
                    let mut t = format!("git:{branch}");
                    if dirty > 0 {
                        t.push_str(&format!(" *{dirty}"));
                    }
                    t.chars().take(24).collect::<String>()
                };
                // The stats pill: the cumulative totals (k/M/B
                // shortened), the cached total when the session saw
                // any, the ctx section when both its inputs are
                // known.
                let mut stats = format!(
                    "in:{} out:{} sum:{}",
                    fmt_num(in_total),
                    fmt_num(out_total),
                    fmt_num(in_total + out_total)
                );
                if cached_total > 0 {
                    stats.push_str(&format!(" R:{}", fmt_num(cached_total)));
                }
                if let Some(c) = ctx_text(last_in, ctx_window) {
                    stats.push_str(&format!(" {c}"));
                }

                // One segment per pill. The head (dir) never drops;
                // the model and git pills drop in that order on
                // overflow; the stats pill keeps its space (the
                // token indicator wins the width fight).
                let dir_seg = Seg {
                    text: short_dir(&dir),
                    fg: TXT,
                    bg: DIR_BG,
                };
                let git_seg = Seg {
                    text: git_txt,
                    fg: TXT,
                    bg: GIT_BG,
                };
                let model_seg = Seg {
                    text: model.to_string(),
                    fg: TXT_DARK,
                    bg: MODEL_BG,
                };
                let stats_seg = Seg {
                    text: stats,
                    fg: TXT,
                    bg: STATS_BG,
                };
                let lines: Vec<Value> = if width >= 100 {
                    // One line: reserve the stats pill and its join
                    // arrow, then fit dir, git, and model into the
                    // rest. The tail-drop order is model first,
                    // then git.
                    let stats_cols = row_cols(&[stats_seg.clone()]);
                    let rest_w = width.saturating_sub(stats_cols.saturating_sub(1));
                    let l1 = fit(&[dir_seg.clone(), git_seg.clone(), model_seg], rest_w);
                    let mut row = l1;
                    row.push(stats_seg);
                    vec![Value::Array(row_spans(&row))]
                } else {
                    // Two-line layout: line 1 is dir, git, model;
                    // line 2 is the stats pill alone. Each row fits
                    // the width on its own.
                    let l1 = fit(&[dir_seg.clone(), git_seg.clone(), model_seg], width);
                    vec![
                        Value::Array(row_spans(&l1)),
                        Value::Array(row_spans(&[stats_seg])),
                    ]
                };
                let reply = json!({"v": 1, "op": "status", "lines": lines});
                let _ = writeln!(out, "{reply}");
                let _ = out.flush();
            }
            _ => {}
        }
    }
}
