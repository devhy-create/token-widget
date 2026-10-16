//! Claude Code statusLine 钩子：把 stdin 里的 rate_limits（5 小时 / 每周额度）写到本地文件，供小组件读取。
//! 只落盘额度百分比和重置时间，不碰任何令牌；返回一行简短状态给 Claude Code 显示。
//! Tauri 版把它做成主程序的子命令（`agent-monitor --statusline`），用户机器上不需要装 Node。
use crate::util::now_ms;
use serde_json::{json, Value};
use std::path::Path;

/// JS 的真值判断（用于 `!!rl` 与 `if (rl)`）
fn truthy(v: &Value) -> bool {
    match v {
        Value::Null | Value::Bool(false) => false,
        Value::Number(n) => n.as_f64().map_or(true, |f| f != 0.0),
        Value::String(s) => !s.is_empty(),
        _ => true,
    }
}

/// 一个窗口的百分比文本：取 used_percentage ?? used_percent，缺失显示 —
fn pct(w: Option<&Value>) -> String {
    let used = w.filter(|w| w.is_object()).and_then(|w| {
        ["used_percentage", "used_percent"].iter().filter_map(|k| w.get(*k)).find(|v| !v.is_null())
    });
    match used.and_then(Value::as_f64) {
        Some(f) => format!("{}%", f.round() as i64),
        None => "—".into(),
    }
}

/// 处理一次钩子调用。input 是 stdin 全文，dir 是小组件数据目录；返回要打印到 stdout 的那一行（可能为空）。
/// 输入不是 JSON 时静默返回空串，不影响 Claude Code。
pub fn run(input: &str, dir: &Path) -> String {
    let Ok(j) = serde_json::from_str::<Value>(input) else { return String::new() };
    if j.is_null() {
        return String::new(); // JS 版对 null 取属性会抛错，等价于静默
    }
    let rl = j.get("rate_limits").filter(|v| truthy(v));
    // 诊断：只记录“被调用的时间 + 输入里有哪些字段名”，不记录任何内容；用来排查为什么没有额度数据
    let keys: Vec<&String> = j.as_object().map(|o| o.keys().collect()).unwrap_or_default();
    let _ = std::fs::create_dir_all(dir).and_then(|_| {
        std::fs::write(
            dir.join("claude-statusline-seen.json"),
            json!({ "ts": now_ms(), "keys": keys, "hasRateLimits": rl.is_some() }).to_string(),
        )
    }); // 诊断失败不影响主流程
    let Some(rl) = rl else { return String::new() };
    // 先写临时文件再改名，避免小组件读到写了一半的内容
    let f = dir.join("claude-limits.json");
    let tmp = dir.join("claude-limits.json.tmp");
    let wrote = std::fs::create_dir_all(dir)
        .and_then(|_| std::fs::write(&tmp, json!({ "ts": now_ms(), "rate_limits": rl }).to_string()))
        .and_then(|_| std::fs::rename(&tmp, &f));
    if wrote.is_err() {
        return String::new();
    }
    format!("5h {} · 周 {}", pct(rl.get("five_hour")), pct(rl.get("seven_day")))
}
