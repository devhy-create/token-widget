mod common;
use agent_core::statusline::run;
use serde_json::{json, Value};
use std::fs;

fn read(p: std::path::PathBuf) -> Value {
    serde_json::from_str(&fs::read_to_string(p).unwrap()).unwrap()
}

#[test]
fn 钩子_把rate_limits写到claude_limits_json_并输出一行状态() {
    let dir = common::temp_home();
    let rl = json!({ "five_hour": { "used_percentage": 6.4, "resets_at": 1700000000 }, "seven_day": { "used_percentage": 28, "resets_at": 1700500000 } });
    let out = run(&json!({ "model": "x", "rate_limits": rl }).to_string(), dir.path());
    assert_eq!(out, "5h 6% · 周 28%");
    let w = read(dir.path().join("claude-limits.json"));
    assert_eq!(w["rate_limits"], rl);
    assert!(w["ts"].is_number());
    assert!(!dir.path().join("claude-limits.json.tmp").exists()); // 临时文件已改名
}

#[test]
fn 钩子_只记录字段名到诊断文件_不记录内容() {
    let dir = common::temp_home();
    run(&json!({ "secret_field_value": "TOPSECRET", "rate_limits": { "five_hour": { "used_percentage": 1 } } }).to_string(), dir.path());
    let seen = fs::read_to_string(dir.path().join("claude-statusline-seen.json")).unwrap();
    assert!(seen.contains("secret_field_value"));
    assert!(!seen.contains("TOPSECRET"));
    assert_eq!(serde_json::from_str::<Value>(&seen).unwrap()["hasRateLimits"], true);
}

#[test]
fn 钩子_输入里没有rate_limits时不写额度文件() {
    let dir = common::temp_home();
    assert_eq!(run(&json!({ "model": "x" }).to_string(), dir.path()), "");
    assert!(!dir.path().join("claude-limits.json").exists());
    assert_eq!(read(dir.path().join("claude-statusline-seen.json"))["hasRateLimits"], false);
}

#[test]
fn 钩子_输入不是json时静默退出() {
    let dir = common::temp_home();
    assert_eq!(run("这不是 json", dir.path()), "");
}

#[test]
fn 钩子_只有一个窗口时另一个显示破折号() {
    let dir = common::temp_home();
    assert_eq!(run(&json!({ "rate_limits": { "five_hour": { "used_percent": 50 } } }).to_string(), dir.path()), "5h 50% · 周 —");
}
