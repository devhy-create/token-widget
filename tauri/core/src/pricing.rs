//! 模型定价：默认表 + 用户覆盖表，按模型名前缀（最长匹配）取价。
//! 默认表在 data/pricing.default.json，编译时嵌入。
use serde_json::Value;
use std::path::Path;

const DEFAULT_JSON: &str = include_str!("../data/pricing.default.json");

#[derive(Clone, Debug, PartialEq)]
pub struct PriceEntry {
    pub matcher: String,
    pub input: f64,
    pub out: f64,
    pub cr: f64,
    pub cw5: f64,
    pub cw1: f64,
    pub est: bool,
}

/// 一个汇总桶（token 数）
#[derive(Clone, Copy, Debug, Default)]
pub struct Bucket {
    pub input: f64,
    pub cr: f64,
    pub cw5: f64,
    pub cw1: f64,
    pub out: f64,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Cost {
    pub usd: f64,
    pub est: bool,
    pub known: bool,
}

#[derive(Default)]
pub struct PriceTable {
    table: Vec<PriceEntry>,
}

fn entry_of(v: &Value) -> PriceEntry {
    let f = |k: &str| v.get(k).and_then(Value::as_f64).unwrap_or(0.0);
    PriceEntry {
        matcher: crate::util::js_str(v.get("match")).to_lowercase(),
        input: f("in"),
        out: f("out"),
        cr: f("cr"),
        cw5: f("cw5"),
        cw1: f("cw1"),
        est: v.get("est").map(|e| !matches!(e, Value::Null | Value::Bool(false))).unwrap_or(false),
    }
}

impl PriceTable {
    /// 载入默认表；user_file 存在且可解析时，其条目排在默认表前面
    pub fn load(user_file: Option<&Path>) -> Self {
        let def: Vec<PriceEntry> = serde_json::from_str::<Value>(DEFAULT_JSON)
            .ok()
            .and_then(|j| j.get("models").and_then(Value::as_array).map(|a| a.iter().map(entry_of).collect()))
            .unwrap_or_default();
        let mut user: Vec<PriceEntry> = Vec::new();
        if let Some(f) = user_file {
            // 用户文件写坏了就忽略，沿用默认表
            if let Some(j) = std::fs::read_to_string(f).ok().and_then(|s| serde_json::from_str::<Value>(&s).ok()) {
                let arr = if j.is_array() { j.as_array() } else { j.get("models").and_then(Value::as_array) };
                if let Some(a) = arr {
                    user = a.iter().map(entry_of).collect();
                }
            }
        }
        // 用户条目放前面；同样长度的前缀优先取用户条目（下面用严格大于比较）
        let mut table = user;
        table.extend(def);
        PriceTable { table }
    }

    pub fn find(&self, model: &str) -> Option<&PriceEntry> {
        let m = model.to_lowercase();
        let mut best: Option<&PriceEntry> = None;
        for e in &self.table {
            if m.starts_with(&e.matcher) && best.map_or(true, |b| e.matcher.len() > b.matcher.len()) {
                best = Some(e);
            }
        }
        best
    }

    /// b 为一个汇总桶；缓存写价格为 0 时回退成输入价（与 JS 版 `p.cw5 || p.in` 一致）
    pub fn cost(&self, model: &str, b: &Bucket) -> Cost {
        let Some(p) = self.find(model) else {
            return Cost { usd: 0.0, est: false, known: false };
        };
        let or_in = |v: f64| if v != 0.0 { v } else { p.input };
        let usd = (b.input * p.input + b.out * p.out + b.cr * p.cr + b.cw5 * or_in(p.cw5) + b.cw1 * or_in(p.cw1)) / 1e6;
        Cost { usd, est: p.est, known: true }
    }
}
