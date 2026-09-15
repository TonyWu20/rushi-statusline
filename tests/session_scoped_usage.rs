use serde_json::{json, Value};
use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};

fn usage_event(type_: &str, in_tok: u64, out_tok: u64, cached: u64) -> Value {
    json!({
        "v": 1,
        "type": type_,
        "content": "",
        "usage": {
            "input_tokens": in_tok,
            "output_tokens": out_tok,
            "cached_tokens": cached
        }
    })
}

fn event_op(id: u64, ev: Value) -> Value {
    json!({ "v": 1, "op": "event", "id": id, "event": ev })
}

fn tick_op(seq: u64, session: Option<&str>) -> Value {
    let mut t = json!({
        "v": 1,
        "op": "tick",
        "seq": seq,
        "width": 200,
        "model": "m",
        "loop_running": false,
        "thinking": 0,
        "statuses": {},
        "cwd": "/tmp"
    });
    if let Some(s) = session {
        t["session"] = json!(s);
    }
    t
}

fn run_protocol(lines: &[Value], config: &Path) -> Vec<Value> {
    let bin = env!("CARGO_BIN_EXE_statusline-ext");
    let mut child = Command::new(bin)
        .env("CONFIG", config)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn the statusline binary");
    {
        let mut stdin = child.stdin.take().expect("the stdin pipe");
        for line in lines {
            let mut payload = line.to_string();
            payload.push('\n');
            stdin.write_all(payload.as_bytes()).expect("write the op");
        }
    }
    let out = child.wait_with_output().expect("wait for the binary");
    let text = String::from_utf8(out.stdout).expect("utf-8 stdout");
    text.lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| serde_json::from_str(l).expect("the reply is json"))
        .collect()
}

fn row_text(reply: &Value) -> String {
    let mut out = String::new();
    for line in reply["lines"].as_array().expect("lines is an array") {
        if let Some(s) = line.as_str() {
            out.push_str(s);
            continue;
        }
        for span in line.as_array().expect("a line holds spans") {
            let text = match span.as_str() {
                Some(s) => s,
                None => span[0].as_str().expect("a span carries text"),
            };
            out.push_str(text);
        }
    }
    out
}

fn write_config(label: &str) -> std::path::PathBuf {
    let path = std::env::temp_dir().join(format!("statusline-test-{}-{label}", std::process::id()));
    std::fs::write(&path, "[model.m]\ncontext_tokens = 10000\n").expect("write the config");
    path
}

#[test]
fn startup_replay_builds_the_totals() {
    let cfg = write_config("startup");
    let lines = vec![
        event_op(0, usage_event("assistant_message", 1000, 500, 0)),
        event_op(1, usage_event("assistant_message", 2000, 500, 0)),
        tick_op(1, Some("A")),
    ];
    let replies = run_protocol(&lines, &cfg);
    assert_eq!(replies.len(), 1);
    let row = row_text(&replies[0]);
    assert!(row.contains("in:3k out:1k sum:4k"), "row: {row}");
    assert!(row.contains("ctx:"), "row: {row}");
    assert!(row.contains("(2k/10k)"), "row: {row}");
    let _ = std::fs::remove_file(&cfg);
}

#[test]
fn session_switch_replaces_the_totals() {
    let cfg = write_config("switch");
    let lines = vec![
        event_op(0, usage_event("assistant_message", 1000, 500, 0)),
        event_op(1, usage_event("assistant_message", 2000, 500, 0)),
        tick_op(1, Some("A")),
        event_op(2, usage_event("assistant_message", 1000, 1000, 0)),
        tick_op(2, Some("A")),
        event_op(3, usage_event("assistant_message", 500, 50, 0)),
        tick_op(3, Some("B")),
        event_op(4, usage_event("assistant_message", 1000, 500, 0)),
        event_op(5, usage_event("assistant_message", 2000, 500, 0)),
        event_op(6, usage_event("assistant_message", 1000, 1000, 0)),
        tick_op(4, Some("A")),
        tick_op(5, Some("A")),
    ];
    let replies = run_protocol(&lines, &cfg);
    assert_eq!(replies.len(), 5);
    let rows: Vec<String> = replies.iter().map(row_text).collect();
    assert!(
        rows[0].contains("in:3k out:1k sum:4k"),
        "A startup: {}",
        rows[0]
    );
    assert!(
        rows[1].contains("in:4k out:2k sum:6k"),
        "A live: {}",
        rows[1]
    );
    assert!(
        rows[2].contains("in:500 out:50 sum:550"),
        "B switch: {}",
        rows[2]
    );
    assert!(
        rows[3].contains("in:4k out:2k sum:6k"),
        "A switchback: {}",
        rows[3]
    );
    assert!(
        rows[4].contains("in:4k out:2k sum:6k"),
        "A stable: {}",
        rows[4]
    );
    assert!(rows[2].contains("(500/10k)"), "B ctx: {}", rows[2]);
    assert!(rows[3].contains("(1k/10k)"), "A ctx: {}", rows[3]);
    let _ = std::fs::remove_file(&cfg);
}

#[test]
fn empty_session_resets_the_state() {
    let cfg = write_config("empty");
    let lines = vec![
        event_op(0, usage_event("assistant_message", 9000, 900, 0)),
        tick_op(1, Some("A")),
        tick_op(2, Some("C")),
    ];
    let replies = run_protocol(&lines, &cfg);
    assert_eq!(replies.len(), 2);
    let row = row_text(&replies[1]);
    assert!(row.contains("in:0 out:0 sum:0"), "row: {row}");
    assert!(!row.contains("ctx"), "stale ctx leaked: {row}");
    let _ = std::fs::remove_file(&cfg);
}

#[test]
fn no_session_ticks_fall_back_to_additive() {
    let cfg = write_config("nosession");
    let lines = vec![
        event_op(0, usage_event("assistant_message", 1000, 500, 0)),
        tick_op(1, None),
        event_op(1, usage_event("assistant_message", 1000, 500, 0)),
        tick_op(2, None),
    ];
    let replies = run_protocol(&lines, &cfg);
    assert_eq!(replies.len(), 2);
    let row = row_text(&replies[1]);
    assert!(row.contains("in:2k out:1k sum:3k"), "row: {row}");
    let _ = std::fs::remove_file(&cfg);
}

#[test]
fn compaction_usage_joins_the_totals() {
    let cfg = write_config("compaction");
    let lines = vec![
        event_op(0, usage_event("assistant_message", 2000, 500, 0)),
        event_op(1, usage_event("compaction_summary", 1000, 500, 0)),
        tick_op(1, Some("A")),
    ];
    let replies = run_protocol(&lines, &cfg);
    assert_eq!(replies.len(), 1);
    let row = row_text(&replies[0]);
    assert!(row.contains("in:3k out:1k sum:4k"), "row: {row}");
    assert!(row.contains("(2k/10k)"), "row: {row}");
    let _ = std::fs::remove_file(&cfg);
}
