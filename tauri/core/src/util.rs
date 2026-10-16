//! 与 JS 语义对齐的小工具（Date.now / Date.parse / Number() / 字符串化）。
use chrono::{DateTime, Local, NaiveDate, NaiveDateTime, TimeZone};
use serde_json::Value;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

/// 当前时间（毫秒），等价 Date.now()
pub fn now_ms() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as i64).unwrap_or(0)
}

/// 等价 Date.parse：RFC3339 / 无时区日期时间（按本地时区）/ 纯日期（按 UTC）；失败返回 None
pub fn parse_date_ms(s: &str) -> Option<i64> {
    let s = s.trim();
    if let Ok(d) = DateTime::parse_from_rfc3339(s) {
        return Some(d.timestamp_millis());
    }
    for fmt in ["%Y-%m-%dT%H:%M:%S%.f", "%Y-%m-%d %H:%M:%S%.f", "%Y-%m-%dT%H:%M"] {
        if let Ok(n) = NaiveDateTime::parse_from_str(s, fmt) {
            return Local.from_local_datetime(&n).earliest().map(|d| d.timestamp_millis());
        }
    }
    if let Ok(d) = NaiveDate::parse_from_str(s, "%Y-%m-%d") {
        return Some(d.and_hms_opt(0, 0, 0)?.and_utc().timestamp_millis());
    }
    None
}

/// 等价 collectors.js 的 num()：有限数字取原值，其余（缺失 / 字符串 / null）为 0；负数按 0 处理（计数不可能为负）
pub fn num(v: Option<&Value>) -> u64 {
    match v.and_then(Value::as_f64) {
        Some(f) if f.is_finite() && f > 0.0 => f as u64,
        _ => 0,
    }
}

/// 等价 JS 的 Number(x) || 0：数字原样，可解析的字符串转数字，其余为 0
pub fn js_number_or_zero(v: Option<&Value>) -> f64 {
    let f = match v {
        Some(Value::Number(n)) => n.as_f64().unwrap_or(0.0),
        Some(Value::String(s)) => s.trim().parse::<f64>().unwrap_or(0.0),
        Some(Value::Bool(true)) => 1.0,
        _ => 0.0,
    };
    if f.is_finite() { f } else { 0.0 }
}

/// 等价 JS 模板字符串里 `${v}` 的表现：缺失 / null → "undefined" / "null"，整数不带小数点
pub fn js_str(v: Option<&Value>) -> String {
    match v {
        None => "undefined".into(),
        Some(Value::Null) => "null".into(),
        Some(Value::String(s)) => s.clone(),
        Some(Value::Number(n)) => match n.as_f64() {
            Some(f) if f.fract() == 0.0 && f.abs() < 1e15 => format!("{}", f as i64),
            _ => n.to_string(),
        },
        Some(other) => other.to_string(),
    }
}

/// JS 的真值判断用于字符串：非空才算有值
pub fn non_empty(v: Option<&Value>) -> Option<&str> {
    v.and_then(Value::as_str).filter(|s| !s.is_empty())
}

/// 小组件数据目录：%APPDATA%\token-widget（Windows）、~/Library/Application Support/token-widget（macOS）、~/.config/token-widget（其它）
/// 与旧版（v0.1）相同，升级后已有缓存可以直接沿用。
pub fn default_data_dir() -> PathBuf {
    // TOKEN_WIDGET_DIR 与 Claude 钩子（--statusline）认的是同一个变量：测试 / 便携使用时可整体改目录
    if let Some(d) = std::env::var_os("TOKEN_WIDGET_DIR").filter(|d| !d.is_empty()) {
        return PathBuf::from(d);
    }
    if let Some(appdata) = std::env::var_os("APPDATA") {
        return PathBuf::from(appdata).join("token-widget");
    }
    let home = home_dir();
    if cfg!(target_os = "macos") {
        home.join("Library").join("Application Support").join("token-widget")
    } else {
        home.join(".config").join("token-widget")
    }
}

/// 用户主目录（USERPROFILE 优先，其次 HOME）
pub fn home_dir() -> PathBuf {
    std::env::var_os("USERPROFILE")
        .or_else(|| std::env::var_os("HOME"))
        .map(PathBuf::from)
        .unwrap_or_default()
}
