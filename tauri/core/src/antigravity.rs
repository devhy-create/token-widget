//! Antigravity 额度：本地日志里没有，只能问它自己的本地语言服务（127.0.0.1 上的 GetUserStatus）。
//! 接口令牌取自该进程的命令行参数，只放在内存里：不打印、不写盘、不发往任何非本机地址。
//! 这是未公开的内部接口，可能随版本变化；任何一步失败都返回 None，界面回退成“暂无数据”。
//! 本模块目前只移植了响应解析（纯函数，可测）；进程发现与 HTTP 请求属于平台相关的窗口壳阶段。
use chrono::DateTime;
use serde_json::{json, Value};

/// 解析 GetUserStatus 的响应，返回 {primary, models} 或 None。
/// primary 取各模型里剩余最少的那个（最紧张的）。
pub fn parse(j: &Value) -> Option<Value> {
    let cfgs = j.get("userStatus")?.get("cascadeModelConfigData")?.get("clientModelConfigs")?.as_array()?;
    let mut models: Vec<(String, f64, i64)> = Vec::new();
    for c in cfgs {
        let Some(q) = c.get("quotaInfo") else { continue };
        let Some(remaining) = q.get("remainingFraction").filter(|v| !v.is_null()) else { continue };
        let remaining = remaining.as_f64().or_else(|| remaining.as_str().and_then(|s| s.parse().ok())).unwrap_or(f64::NAN);
        let resets_at = q.get("resetTime").and_then(Value::as_str)
            .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
            .map(|d| (d.timestamp_millis() as f64 / 1000.0).round() as i64)
            .unwrap_or(0);
        let label = c.get("label").and_then(Value::as_str).unwrap_or("").to_string();
        models.push((label, remaining, resets_at));
    }
    if models.is_empty() {
        return None;
    }
    // 与 JS 的 reduce 一致：严格小于才替换，并列时取靠前的
    let worst = models.iter().skip(1).fold(&models[0], |a, b| if b.1 < a.1 { b } else { a });
    Some(json!({
        "primary": {
            "used_percent": ((1.0 - worst.1) * 100.0).clamp(0.0, 100.0),
            "window_minutes": 300,
            "resets_at": worst.2,
        },
        "models": models.iter().map(|(l, r, t)| json!({ "label": l, "remaining": r, "resetsAt": t })).collect::<Vec<_>>(),
    }))
}
