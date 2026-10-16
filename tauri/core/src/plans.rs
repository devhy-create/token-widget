//! 订阅套餐检测：Claude 读 ~/.claude.json 的账号信息；Codex 优先用日志里的 plan_type，其次解码 auth.json 里 id_token 的套餐声明。
//! 只提取套餐字段，不保存也不输出任何令牌、邮箱。用户可用 plans.user.json 覆盖。
use crate::util::js_number_or_zero;
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use regex::Regex;
use serde::Serialize;
use serde_json::{json, Map, Value};
use std::collections::BTreeMap;
use std::path::Path;

const DEFAULT_JSON: &str = include_str!("../../../core/plans.default.json");
pub const AGENTS: [&str; 6] = ["claude", "codex", "antigravity", "workbuddy", "zcode", "opencode"];

#[derive(Clone, Debug, Serialize, PartialEq)]
pub struct Plan {
    pub key: String,
    pub name: String,
    /// 月费（美元）；价格表里没有的套餐为 None，只显示名字、不做价值换算
    pub usd: Option<f64>,
    pub est: bool,
    /// "detected" | "user"
    pub source: &'static str,
}

/// 默认套餐价格表（设置页的预设选项用）
pub fn defaults() -> Value {
    serde_json::from_str(DEFAULT_JSON).unwrap_or(Value::Null)
}

fn read_json(f: &Path) -> Option<Value> {
    std::fs::read_to_string(f).ok().and_then(|s| serde_json::from_str(&s).ok())
}

fn claude_key(home: &Path) -> Option<String> {
    let j = read_json(&home.join(".claude.json"))?;
    let a = j.get("oauthAccount").filter(|a| !a.is_null())?;
    let org = a.get("organizationType").and_then(Value::as_str).unwrap_or("");
    let tier = a.get("organizationRateLimitTier").and_then(Value::as_str).unwrap_or("");
    let re = Regex::new(r"max_(\d+)x").unwrap();
    let m = re.captures(tier);
    if org.contains("max") || m.is_some() {
        return Some(match m {
            Some(c) => format!("max_{}x", &c[1]),
            None => "max".into(),
        });
    }
    if org.contains("pro") {
        return Some("pro".into());
    }
    if org.contains("team") || org.contains("enterprise") {
        return Some("team".into());
    }
    if org.contains("free") {
        return Some("free".into());
    }
    None
}

fn codex_key(home: &Path, log_plan: Option<&str>) -> Option<String> {
    if let Some(p) = log_plan.filter(|p| !p.is_empty()) {
        return Some(p.to_lowercase());
    }
    let a = read_json(&home.join(".codex").join("auth.json"))?;
    let t = a.get("tokens")?.get("id_token")?.as_str()?;
    let payload = t.split('.').nth(1)?;
    let bytes = URL_SAFE_NO_PAD.decode(payload.trim_end_matches('=')).ok()?;
    let pl: Value = serde_json::from_slice(&bytes).ok()?;
    let p = pl.get("https://api.openai.com/auth")?.get("chatgpt_plan_type")?.as_str()?;
    if p.is_empty() { None } else { Some(p.to_lowercase()) }
}

/// 返回 {agent: Some(Plan)}；检测不到的 agent 为 None（界面不显示套餐，可用用户文件补）
pub fn detect(home: &Path, user_file: &Path, log_plans: &BTreeMap<String, String>) -> BTreeMap<String, Option<Plan>> {
    let defaults: Value = serde_json::from_str(DEFAULT_JSON).unwrap_or(Value::Null);
    let user = read_json(user_file).unwrap_or_else(|| json!({}));
    let det: [(&str, Option<String>); 6] = [
        ("claude", claude_key(home)),
        ("codex", codex_key(home, log_plans.get("codex").map(String::as_str))),
        ("antigravity", None),
        ("workbuddy", None),
        ("zcode", None),
        ("opencode", None),
    ];
    let mut out = BTreeMap::new();
    for (ag, key) in det {
        if let Some(u) = user.get(ag) {
            if let Some(name) = u.get("name").and_then(Value::as_str).filter(|n| !n.is_empty()) {
                out.insert(ag.to_string(), Some(Plan {
                    key: "user".into(), name: name.into(), usd: Some(js_number_or_zero(u.get("usd"))), est: false, source: "user",
                }));
                continue;
            }
        }
        let Some(key) = key else {
            out.insert(ag.to_string(), None);
            continue;
        };
        let plan = match defaults.get(ag).and_then(|d| d.get(&key)) {
            Some(p) => Plan {
                name: p.get("name").and_then(Value::as_str).unwrap_or(&key).to_string(),
                usd: p.get("usd").and_then(Value::as_f64),
                est: p.get("est").and_then(Value::as_bool).unwrap_or(false),
                key, source: "detected",
            },
            // 检测到了套餐但价格表里没有：只显示名字，不做价值换算
            None => Plan { name: key.clone(), key, usd: None, est: false, source: "detected" },
        };
        out.insert(ag.to_string(), Some(plan));
    }
    out
}

/// 读用户覆盖文件 plans.user.json；不存在或损坏返回空对象
pub fn read_user(file: &Path) -> Value {
    read_json(file).unwrap_or_else(|| json!({}))
}

/// 写用户覆盖文件（设置面板用）：entry 为 {name, usd} 表示覆盖，None 表示恢复自动识别。其余键原样保留。
pub fn write_user(file: &Path, agent: &str, entry: Option<&Value>) -> std::io::Result<()> {
    let mut u = match read_json(file) {
        Some(Value::Object(m)) => m,
        _ => Map::new(),
    };
    if u.get("_说明").and_then(Value::as_str).map_or(true, str::is_empty) {
        u.insert("_说明".into(), json!("键名为 agent（claude/codex/antigravity/workbuddy/zcode/opencode）的条目会覆盖自动识别的套餐；usd 为月费（美元）。可在小组件设置面板里改"));
    }
    match entry {
        Some(e) => {
            let name = e.get("name").and_then(Value::as_str).filter(|n| !n.is_empty()).unwrap_or("自定义");
            let name: String = name.chars().take(24).collect();
            let usd = js_number_or_zero(e.get("usd")).max(0.0);
            u.insert(agent.into(), json!({ "name": name, "usd": usd }));
        }
        None => {
            u.shift_remove(agent);
        }
    }
    std::fs::write(file, serde_json::to_string_pretty(&Value::Object(u))?)
}
