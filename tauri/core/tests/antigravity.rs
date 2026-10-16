use agent_core::antigravity::parse;
use chrono::DateTime;
use serde_json::{json, Value};

fn cfg(label: &str, remaining: Option<f64>, reset: Option<&str>) -> Value {
    match remaining {
        None => json!({ "label": label, "quotaInfo": {} }),
        Some(r) => json!({ "label": label, "quotaInfo": { "remainingFraction": r, "resetTime": reset } }),
    }
}
fn wrap(list: Vec<Value>) -> Value {
    json!({ "userStatus": { "cascadeModelConfigData": { "clientModelConfigs": list } } })
}
fn secs(s: &str) -> i64 {
    (DateTime::parse_from_rfc3339(s).unwrap().timestamp_millis() as f64 / 1000.0).round() as i64
}

#[test]
fn parse_primary取剩余最少的模型_百分比等于已用() {
    let r = parse(&wrap(vec![
        cfg("Gemini 3 Pro", Some(0.8), Some("2026-03-01T12:00:00Z")),
        cfg("Claude Sonnet", Some(0.25), Some("2026-03-01T13:00:00Z")),
        cfg("无额度信息", None, None),
    ])).unwrap();
    assert_eq!(r["models"].as_array().unwrap().len(), 2);
    assert_eq!(r["primary"]["used_percent"], 75.0);
    assert_eq!(r["primary"]["window_minutes"], 300);
    assert_eq!(r["primary"]["resets_at"], secs("2026-03-01T13:00:00Z"));
}

#[test]
fn parse_剩余比例越界时百分比钳制在0到100() {
    assert_eq!(parse(&wrap(vec![cfg("a", Some(1.5), Some("2026-03-01T12:00:00Z"))])).unwrap()["primary"]["used_percent"], 0.0);
    assert_eq!(parse(&wrap(vec![cfg("a", Some(-0.5), Some("2026-03-01T12:00:00Z"))])).unwrap()["primary"]["used_percent"], 100.0);
}

#[test]
fn parse_缺少reset_time时resets_at为0() {
    assert_eq!(parse(&wrap(vec![cfg("a", Some(0.5), None)])).unwrap()["primary"]["resets_at"], 0);
}

#[test]
fn parse_没有任何带额度的模型或结构缺失返回none() {
    assert!(parse(&wrap(vec![])).is_none());
    assert!(parse(&wrap(vec![cfg("a", None, None)])).is_none());
    assert!(parse(&json!({})).is_none());
}
