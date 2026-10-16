//! 各 agent 的日志 / 数据库采集器。每个采集器负责：发现文件、增量读取、归一化为统一事件。
//! 统一事件：Event {agent, ts, model, sid, key, input, cr, cw5, cw1, out, rs, dur}
//!   input = 未命中缓存的输入 token；cr = 缓存读；cw5/cw1 = 5分钟/1小时缓存写
//!   out = 输出 token（含思考）；rs = 其中的思考 token（仅展示）；dur = 本次生成耗时(ms)，未知为 0
use crate::lines::{read_lines, Line, ReadResult};
use crate::util::{js_number_or_zero, js_str, non_empty, num, parse_date_ms};
use chrono::{Local, NaiveDate, TimeZone};
use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

const MIN_DUR: i64 = 300; // 小于 0.3 秒的耗时视为不可信
const MAX_DUR: i64 = 15 * 60 * 1000;

fn clamp_dur(ms: i64) -> i64 {
    if (MIN_DUR..=MAX_DUR).contains(&ms) { ms } else { 0 }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Event {
    pub agent: &'static str,
    pub ts: i64,
    pub model: String,
    pub sid: String,
    pub key: String,
    pub input: u64,
    pub cr: u64,
    pub cw5: u64,
    pub cw1: u64,
    pub out: u64,
    pub rs: u64,
    pub dur: i64,
}

/// 采集器的输出口：入账事件 + 额度窗口（目前只有 Codex 在日志里带）
pub trait Sink {
    fn emit(&mut self, ev: Event);
    fn set_limits(&mut self, agent: &str, ts: i64, rl: &Value);
    /// 请求已发出、还没有用量（目前只有 WorkBuddy 主日志能给出）：model 为 Some 表示开始，None 表示结束
    fn busy(&mut self, _agent: &'static str, _key: &str, _ts: i64, _model: Option<&str>) {}
}

/// Claude 同一条消息被拆成多行写入时，暂存合并中的消息
#[derive(Clone, Debug, Default)]
pub struct Pend {
    pub id: Option<String>,
    pub model: String,
    pub sid: String,
    pub ts: i64,
    pub prev_ts: i64,
    pub wall: i64,
    pub input: u64,
    pub cr: u64,
    pub cw5: u64,
    pub cw1: u64,
    pub out: u64,
    pub rs: u64,
}

/// 单个日志文件的采集状态；会随引擎缓存落盘（pend 不落盘，与 JS 版一致）
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct FileState {
    pub off: u64,
    /// 上次见到的文件大小，用来判断是否有新内容
    pub seen: u64,
    /// 上次检查时间 / 文件修改时间（冷热分层用）
    pub chk: i64,
    pub mt: f64,
    #[serde(rename = "prevTs")]
    pub prev_ts: i64,
    pub mark: i64,
    pub model: Option<String>,
    #[serde(skip)]
    pub pend: Option<Pend>,
}

pub trait Collector: Send {
    fn name(&self) -> &'static str;
    fn discover(&self) -> Vec<PathBuf>;
    fn scan(&self, file: &Path, st: &mut FileState, sink: &mut dyn Sink, now: i64) -> io::Result<ReadResult>;
    /// 进程退出时把还在合并中的消息入账
    fn finish(&self, _st: &mut FileState, _sink: &mut dyn Sink) {}
    /// 判断文件有没有新内容的探针：返回 (大小, 修改时间 ms)；None 表示用文件本身的元数据。
    /// SQLite 库要覆盖 -wal 文件（见 sqlite::probe），所以由采集器自己决定。
    fn probe(&self, _file: &Path) -> Option<(u64, f64)> { None }
}

/// 按固定顺序返回全部采集器
pub fn all_collectors(home: &Path) -> Vec<Box<dyn Collector>> {
    vec![
        Box::new(Claude { home: home.to_path_buf() }),
        Box::new(Codex { home: home.to_path_buf() }),
        Box::new(WorkBuddy::new(home)),
        Box::new(Antigravity { home: home.to_path_buf() }),
        Box::new(ZCode { home: home.to_path_buf() }),
        Box::new(OpenCode { home: home.to_path_buf() }),
    ]
}

/// 递归列出 root 下满足 test 的文件（容错：目录不存在返回空；最多 6 层）
fn walk(root: &Path, test: &dyn Fn(&str) -> bool, out: &mut Vec<PathBuf>, depth: u32) {
    let Ok(rd) = std::fs::read_dir(root) else { return };
    for e in rd.flatten() {
        let p = e.path();
        let Ok(ft) = e.file_type() else { continue };
        if ft.is_dir() {
            if depth < 6 {
                walk(&p, test, out, depth + 1);
            }
        } else if test(&e.file_name().to_string_lossy()) {
            out.push(p);
        }
    }
}

fn walk_jsonl(root: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    walk(root, &|n| n.ends_with(".jsonl"), &mut out, 0);
    out
}

fn stem(file: &Path) -> String {
    file.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default()
}

fn file_name(file: &Path) -> String {
    file.file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default()
}

/// 时间戳为 0 时保留旧值（JS 的 `ts || old`）
fn or_prev(ts: i64, old: i64) -> i64 {
    if ts != 0 { ts } else { old }
}

/// 取第一个非 0 的数（JS 的 `a || b || c`）
fn first_nonzero(vals: &[u64]) -> u64 {
    vals.iter().copied().find(|v| *v != 0).unwrap_or(0)
}

// ---------------------------------------------------------------- Claude
pub struct Claude {
    home: PathBuf,
}

fn claude_flush(st: &mut FileState, sink: &mut dyn Sink) {
    let Some(p) = st.pend.take() else { return };
    sink.emit(Event {
        agent: "claude", ts: p.ts, model: p.model, sid: p.sid,
        key: p.id.unwrap_or_else(|| "undefined".into()),
        input: p.input, cr: p.cr, cw5: p.cw5, cw1: p.cw1, out: p.out, rs: p.rs,
        dur: clamp_dur(p.ts - p.prev_ts),
    });
}

impl Collector for Claude {
    fn name(&self) -> &'static str { "claude" }

    fn discover(&self) -> Vec<PathBuf> {
        walk_jsonl(&self.home.join(".claude").join("projects"))
    }

    fn scan(&self, file: &Path, st: &mut FileState, sink: &mut dyn Sink, now: i64) -> io::Result<ReadResult> {
        let off = st.off;
        let r = read_lines(file, off, &["\"usage\""], &[], &mut |line| match line {
            Line::Skip(ts, _) => {
                if ts != 0 { st.prev_ts = ts; }
            }
            Line::Hit(text, ts) => {
                let Ok(o) = serde_json::from_str::<Value>(text) else {
                    st.prev_ts = or_prev(ts, st.prev_ts);
                    return;
                };
                let m = o.get("message");
                let usage = m.and_then(|m| m.get("usage")).filter(|u| !u.is_null());
                let model = non_empty(m.and_then(|m| m.get("model")));
                let (Some(m), Some(u), Some(model)) = (m, usage, model) else {
                    st.prev_ts = or_prev(ts, st.prev_ts);
                    return;
                };
                if o.get("type").and_then(Value::as_str) != Some("assistant") || model == "<synthetic>" {
                    st.prev_ts = or_prev(ts, st.prev_ts);
                    return;
                }
                let cc = u.get("cache_creation").filter(|c| c.is_object());
                let c5 = cc.and_then(|c| c.get("ephemeral_5m_input_tokens")).filter(|v| !v.is_null());
                let c1 = cc.and_then(|c| c.get("ephemeral_1h_input_tokens")).filter(|v| !v.is_null());
                let has = c5.is_some() || c1.is_some();
                let total = num(u.get("cache_creation_input_tokens"));
                let w1 = if has { num(c1) } else { 0 };
                let w5 = if has { num(c5) } else { total }; // 没有拆分时按 5 分钟算
                let id = non_empty(m.get("id")).or_else(|| non_empty(o.get("requestId"))).or_else(|| non_empty(o.get("uuid"))).map(str::to_string);
                let rs = num(u.get("output_tokens_details").and_then(|d| d.get("thinking_tokens")));
                // 实测（对照 Claude 自带的 cost-state 汇总）：日志里的 output_tokens 不含思考 token，
                // 计费输出 ≈ output_tokens + thinking_tokens，所以这里把两者相加
                let out = num(u.get("output_tokens")) + rs;
                let (input, cr) = (num(u.get("input_tokens")), num(u.get("cache_read_input_tokens")));
                let t = if ts != 0 { ts } else { now };
                if st.pend.as_ref().map_or(false, |p| p.id == id) {
                    // 同一条消息被拆成多行写入（thinking / text / tool_use 各一行）：取各字段最大值，时间取最后一行
                    let p = st.pend.as_mut().unwrap();
                    p.input = p.input.max(input);
                    p.cr = p.cr.max(cr);
                    p.cw5 = p.cw5.max(w5);
                    p.cw1 = p.cw1.max(w1);
                    p.out = p.out.max(out);
                    p.rs = p.rs.max(rs);
                    p.ts = t;
                    p.wall = now;
                } else {
                    claude_flush(st, sink);
                    let sid = non_empty(o.get("sessionId")).map(str::to_string).unwrap_or_else(|| stem(file));
                    st.pend = Some(Pend {
                        id, model: model.to_string(), sid, ts: t, prev_ts: st.prev_ts, wall: now,
                        input, cr, cw5: w5, cw1: w1, out, rs,
                    });
                }
                st.prev_ts = t;
            }
        })?;
        st.off = r.offset;
        // 最后一条消息可能还在被追加：4 秒没有新行再入账
        if st.pend.as_ref().map_or(false, |p| now - p.wall > 4000) {
            claude_flush(st, sink);
        }
        Ok(r)
    }

    fn finish(&self, st: &mut FileState, sink: &mut dyn Sink) {
        claude_flush(st, sink);
    }
}

// ---------------------------------------------------------------- Codex
pub struct Codex {
    home: PathBuf,
}

impl Collector for Codex {
    fn name(&self) -> &'static str { "codex" }

    fn discover(&self) -> Vec<PathBuf> {
        let mut v = walk_jsonl(&self.home.join(".codex").join("sessions"));
        v.extend(walk_jsonl(&self.home.join(".codex").join("archived_sessions")));
        v
    }

    fn scan(&self, file: &Path, st: &mut FileState, sink: &mut dyn Sink, now: i64) -> io::Result<ReadResult> {
        let needles = ["\"token_usage_record\"", "\"turn_context\"", "\"token_count\""];
        // 一次模型请求的起点：用户输入、工具输出、回合开始
        let marks = ["\"function_call_output\"", "\"custom_tool_call_output\"", "\"task_started\"", "\"role\":\"user\""];
        let off = st.off;
        let r = read_lines(file, off, &needles, &marks, &mut |line| match line {
            Line::Skip(ts, is_mark) => {
                if is_mark && ts != 0 { st.mark = ts; }
            }
            Line::Hit(text, ts) => {
                let Ok(o) = serde_json::from_str::<Value>(text) else { return };
                let null = Value::Null;
                let p = o.get("payload").filter(|p| p.is_object()).unwrap_or(&null);
                let t = if ts != 0 { ts } else {
                    o.get("timestamp").and_then(Value::as_str).and_then(parse_date_ms).filter(|t| *t != 0).unwrap_or(now)
                };
                let ty = o.get("type").and_then(Value::as_str);
                if ty == Some("turn_context") {
                    if let Some(m) = non_empty(p.get("model")) { st.model = Some(m.to_string()); }
                    return;
                }
                if ty == Some("event_msg") && p.get("type").and_then(Value::as_str) == Some("token_count") {
                    if let Some(rl) = p.get("rate_limits").filter(|v| !v.is_null()) {
                        sink.set_limits("codex", t, rl);
                    }
                    return;
                }
                let Some(u) = p.get("usage").filter(|u| !u.is_null()) else { return };
                if ty != Some("token_usage_record") { return; }
                let cached = num(u.get("cached_input_tokens"));
                let key = non_empty(p.get("response_id")).map(str::to_string)
                    .unwrap_or_else(|| format!("{}:{}", file_name(file), js_str(o.get("ordinal"))));
                sink.emit(Event {
                    agent: "codex", ts: t,
                    model: st.model.clone().unwrap_or_else(|| "codex".into()),
                    sid: non_empty(p.get("session_id")).map(str::to_string).unwrap_or_else(|| stem(file)),
                    key,
                    input: num(u.get("input_tokens")).saturating_sub(cached), cr: cached,
                    cw5: num(u.get("cache_write_input_tokens")), cw1: 0,
                    out: num(u.get("output_tokens")), rs: num(u.get("reasoning_output_tokens")),
                    dur: clamp_dur(t - st.mark),
                });
                st.mark = t;
            }
        })?;
        st.off = r.offset;
        Ok(r)
    }
}

// ---------------------------------------------------------------- WorkBuddy
/// 主日志里记下的 chat 请求精确耗时只留 15 分钟：只有实时速度用得上，历史日志里的不必留
const WB_DUR_KEEP: i64 = 15 * 60 * 1000;

pub struct WorkBuddy {
    home: PathBuf,
    /// 主日志 qua.end 给出的 chat 请求耗时：message_id → (耗时 ms, 结束时间)，jsonl 用量行入账时取用
    durs: Mutex<HashMap<String, (i64, i64)>>,
}

impl WorkBuddy {
    pub fn new(home: &Path) -> Self {
        WorkBuddy { home: home.to_path_buf(), durs: Mutex::new(HashMap::new()) }
    }

    /// 主日志（logs/<日期>/<启动时间>__<哈希>.log）：每次模型请求有 qua.enqueue / qua.start / qua.end 三行。
    /// jsonl 要等请求结束才写用量，主日志在请求发出时就有记录，所以“生成中”和模型名靠它；
    /// qua.end 带 token 数与总耗时。实测：正常结束的 chat 请求在 jsonl 里都有同 messageId 的记录（带缓存命中拆分，计费更准），
    /// 只取耗时；标题生成（conversation_topic）、上下文压缩（compress）、webfetch 等附带请求 jsonl 里没有，从这里入账。
    fn scan_log(&self, file: &Path, st: &mut FileState, sink: &mut dyn Sink, now: i64) -> io::Result<ReadResult> {
        let r = read_lines(file, st.off, &["] qua.enqueue: ", "] qua.start: ", "] qua.end: "], &[], &mut |line| {
            let Line::Hit(text, _) = line else { return };
            let Some(i) = text.find("] qua.") else { return };
            let Some((kind, body)) = text[i + 6..].split_once(": ") else { return };
            let Ok(o) = serde_json::from_str::<Value>(body.trim()) else { return };
            let Some(id) = non_empty(o.get("message_id")) else { return };
            let ts = wb_log_ts(text).unwrap_or(now);
            let model = non_empty(o.get("model")).unwrap_or("unknown");
            if kind != "end" {
                sink.busy("workbuddy", id, ts, Some(model));
                return;
            }
            sink.busy("workbuddy", id, ts, None);
            let cost = snum(o.get("total_cost_ms")) as i64;
            let done = o.get("finish_flag").and_then(Value::as_str) == Some("done");
            if done && o.get("request_type").and_then(Value::as_str) == Some("chat") {
                if now - ts < WB_DUR_KEEP {
                    if let Ok(mut d) = self.durs.lock() {
                        d.retain(|_, v| now - v.1 < WB_DUR_KEEP);
                        d.insert(id.to_string(), (cost, ts));
                    }
                }
                return;
            }
            // 被中断的请求 token 为 0，不入账；事件键与 jsonl 的 messageId 相同，万一两边都有也会被去重
            let (pt, ct) = (snum(o.get("prompt_tokens")), snum(o.get("completion_tokens")));
            if pt + ct == 0 { return; }
            sink.emit(Event {
                agent: "workbuddy", ts, model: model.to_string(),
                sid: non_empty(o.get("conversation_id")).map(str::to_string).unwrap_or_else(|| stem(file)),
                key: id.to_string(),
                // 日志里没有缓存命中拆分，输入全按未命中算；think_total_tokens 是客户端估算值，不当思考 token 用
                input: pt, cr: 0, cw5: 0, cw1: 0, out: ct, rs: 0,
                dur: clamp_dur(cost),
            });
        })?;
        st.off = r.offset;
        Ok(r)
    }
}

/// qua 行里的数字都写成字符串（"completion_tokens":"1517"）
fn snum(v: Option<&Value>) -> u64 {
    let f = js_number_or_zero(v);
    if f.is_finite() && f > 0.0 { f as u64 } else { 0 }
}

/// 形如 YYYY-MM-DD 的日期目录名；file 为 true 时匹配 YYYY-MM-DD-HH-MM-SS__*.log 的主日志文件名
fn wb_log_name(n: &str, file: bool) -> bool {
    let b = n.as_bytes();
    let len = if file { 19 } else { 10 };
    if b.len() < len || (!file && b.len() != len) {
        return false;
    }
    let shape = (0..len).all(|i| if i % 3 == 1 && i >= 4 { b[i] == b'-' } else { b[i].is_ascii_digit() });
    shape && (!file || (n[len..].starts_with("__") && n.ends_with(".log")))
}

/// WorkBuddy 主日志行首的本地时间，如 “[2026年10月16日 下午1:43:39.584]”。
/// 写法随系统区域设置变化：兼容“年月日 + 上午/下午”、ISO 写法和“月/日/年, 时:分:秒 AM/PM”；解析不了返回 None
fn wb_log_ts(line: &str) -> Option<i64> {
    let s = line.strip_prefix('[')?;
    let head = &s[..s.find(']')?];
    let runs: Vec<&str> = head.split(|c: char| !c.is_ascii_digit()).filter(|r| !r.is_empty()).collect();
    if runs.len() < 6 {
        return None;
    }
    let n: Vec<u32> = runs.iter().map(|r| r.parse().unwrap_or(0)).collect();
    let (y, m, d) = if runs[0].len() == 4 {
        (n[0], n[1], n[2])
    } else if runs[2].len() == 4 {
        // 年份在最后：第一个数大于 12 只能是“日/月/年”，否则按美式“月/日/年”
        if n[0] > 12 { (n[2], n[1], n[0]) } else { (n[2], n[0], n[1]) }
    } else {
        return None;
    };
    let mut h = n[3];
    let pm = head.contains("下午") || head.contains("PM") || head.contains("pm");
    let am = head.contains("上午") || head.contains("AM") || head.contains("am");
    if pm && h < 12 { h += 12; }
    if am && h == 12 { h = 0; }
    // 毫秒按小数位补齐：“.5” 是 500ms
    let ms = runs.get(6).map_or(0, |r| format!("{:0<3}", &r[..r.len().min(3)]).parse().unwrap_or(0));
    let dt = NaiveDate::from_ymd_opt(y as i32, m, d)?.and_hms_milli_opt(h, n[4], n[5], ms)?;
    Some(Local.from_local_datetime(&dt).earliest()?.timestamp_millis())
}

impl Collector for WorkBuddy {
    fn name(&self) -> &'static str { "workbuddy" }

    fn discover(&self) -> Vec<PathBuf> {
        let root = self.home.join(".workbuddy-ai");
        // 主日志排在 jsonl 前面：同一轮扫描里先读到 qua.end 的耗时，jsonl 用量行入账时才取得到
        let mut v = Vec::new();
        if let Ok(rd) = std::fs::read_dir(root.join("logs")) {
            for e in rd.flatten() {
                if !wb_log_name(&e.file_name().to_string_lossy(), false) { continue; }
                let Ok(files) = std::fs::read_dir(e.path()) else { continue };
                v.extend(files.flatten().filter(|f| wb_log_name(&f.file_name().to_string_lossy(), true)).map(|f| f.path()));
            }
        }
        v.extend(walk_jsonl(&root.join("projects")));
        v
    }

    fn scan(&self, file: &Path, st: &mut FileState, sink: &mut dyn Sink, now: i64) -> io::Result<ReadResult> {
        if file.extension().map_or(false, |e| e == "log") {
            return self.scan_log(file, st, sink, now);
        }
        let marks = ["\"type\":\"function_call_result\"", "\"role\":\"user\""];
        let off = st.off;
        let r = read_lines(file, off, &["\"rawUsage\""], &marks, &mut |line| match line {
            Line::Skip(ts, is_mark) => {
                if is_mark && ts != 0 { st.mark = ts; }
            }
            Line::Hit(text, ts) => {
                let Ok(o) = serde_json::from_str::<Value>(text) else { return };
                let null = Value::Null;
                let pd = o.get("providerData").filter(|p| p.is_object()).unwrap_or(&null);
                let Some(ru) = pd.get("rawUsage").filter(|r| !r.is_null()) else { return };
                let t = if ts != 0 { ts } else {
                    o.get("timestamp").and_then(Value::as_f64).map(|f| f as i64).filter(|t| *t != 0).unwrap_or(now)
                };
                let prompt = num(ru.get("prompt_tokens"));
                let details = ru.get("prompt_tokens_details");
                let hit = first_nonzero(&[
                    num(ru.get("prompt_cache_hit_tokens")),
                    num(ru.get("cached_tokens")),
                    num(details.and_then(|d| d.get("cached_tokens"))),
                    num(ru.get("cache_read_input_tokens")),
                ]);
                let w = first_nonzero(&[num(ru.get("cache_creation_input_tokens")), num(ru.get("prompt_cache_write_tokens"))]);
                let rs = first_nonzero(&[
                    num(ru.get("completion_thinking_tokens")),
                    num(ru.get("completion_tokens_details").and_then(|d| d.get("reasoning_tokens"))),
                ]);
                // 主日志 qua.end 里有这次请求的精确总耗时就用它；没有（日志被清理 / 还没写到）才用“距上次请求起点”估算
                let exact = non_empty(pd.get("messageId"))
                    .and_then(|m| self.durs.lock().ok().and_then(|mut d| d.remove(m)))
                    .map(|(d, _)| d);
                sink.emit(Event {
                    agent: "workbuddy", ts: t,
                    model: non_empty(pd.get("model")).or_else(|| non_empty(pd.get("requestModelId"))).unwrap_or("unknown").to_string(),
                    sid: non_empty(o.get("sessionId")).map(str::to_string).unwrap_or_else(|| stem(file)),
                    key: match non_empty(pd.get("messageId")) {
                        Some(s) => s.to_string(),
                        None => js_str(o.get("id")),
                    },
                    input: prompt.saturating_sub(hit).saturating_sub(w), cr: hit, cw5: w, cw1: 0,
                    out: num(ru.get("completion_tokens")), rs,
                    dur: clamp_dur(exact.unwrap_or(t - st.mark)),
                });
                st.mark = t;
            }
        })?;
        st.off = r.offset;
        Ok(r)
    }
}

// ---------------------------------------------------------------- Antigravity
pub struct Antigravity {
    home: PathBuf,
}

/// Antigravity 的一次模型调用：日志里的 created_at 是该步骤“开始”的时间，且整行要等步骤结束才写入。
/// 所以先暂存，等下一行（后续步骤开始 = 本步结束）到来时，用两者之差当生成耗时，用结束时间当事件时间，
/// 这样实时速度才不会因为事件时间过旧（长步骤落盘时已超出统计窗口）而显示不出来，也不会出现“1 秒生成几千 token”的假数据。
fn ag_flush(st: &mut FileState, sink: &mut dyn Sink, end: i64) {
    let Some(p) = st.pend.take() else { return };
    // 时间戳只有秒级精度，耗时至少按 1 秒算；end 不晚于开始（无后续行可参考）时耗时记 0
    let dur = if end > p.ts { clamp_dur((end - p.ts).max(1000)) } else { 0 };
    sink.emit(Event {
        agent: "antigravity", ts: end.max(p.ts), model: p.model, sid: p.sid.clone(),
        key: p.id.unwrap_or_default(),
        input: p.input, cr: p.cr, cw5: 0, cw1: 0, out: p.out, rs: 0, dur,
    });
}

impl Collector for Antigravity {
    fn name(&self) -> &'static str { "antigravity" }

    fn discover(&self) -> Vec<PathBuf> {
        let root = self.home.join(".gemini").join("antigravity").join("brain");
        let Ok(rd) = std::fs::read_dir(&root) else { return Vec::new() };
        rd.flatten()
            .map(|e| e.path().join(".system_generated").join("logs").join("transcript_full.jsonl"))
            .filter(|f| f.exists())
            .collect()
    }

    fn scan(&self, file: &Path, st: &mut FileState, sink: &mut dyn Sink, now: i64) -> io::Result<ReadResult> {
        // 路径：brain/<会话ID>/.system_generated/logs/transcript_full.jsonl，向上三层才是会话目录
        // （多取一层会得到固定的 "brain"，不同会话的事件键撞在一起被去重吞掉）
        let conv_id = file.parent().and_then(Path::parent).and_then(Path::parent)
            .and_then(Path::file_name).map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
        // 模型切换提示出现在用户输入里：`Model Selection` from X to Y
        let sel_re = Regex::new(r#"Model Selection` from [^.]{0,60}? to ([^.\\"]{1,60})"#).unwrap();
        let off = st.off;
        let r = read_lines(file, off, &["\"input_tokens\"", "Model Selection"], &[], &mut |line| match line {
            Line::Skip(ts, _) => {
                // 任何后续行的时间戳都标志着上一次模型调用已经结束
                if ts != 0 {
                    ag_flush(st, sink, ts);
                    st.prev_ts = ts;
                }
            }
            Line::Hit(text, ts) => {
                if ts != 0 {
                    ag_flush(st, sink, ts);
                }
                if let Some(c) = sel_re.captures(text) {
                    if !text.contains("\"input_tokens\"") {
                        st.model = Some(c[1].trim().to_string());
                        return;
                    }
                }
                let Ok(o) = serde_json::from_str::<Value>(text) else { return };
                if o.get("input_tokens").map_or(true, Value::is_null) {
                    st.prev_ts = or_prev(ts, st.prev_ts);
                    return;
                }
                let t = if ts != 0 { ts } else { now };
                let cached = num(o.get("cache_read_tokens"));
                st.pend = Some(Pend {
                    id: Some(format!("{}:{}", conv_id, js_str(o.get("step_index")))),
                    model: st.model.clone().unwrap_or_else(|| "Gemini 3".into()),
                    sid: conv_id.clone(), ts: t, prev_ts: st.prev_ts, wall: now,
                    input: num(o.get("input_tokens")).saturating_sub(cached), cr: cached,
                    out: num(o.get("output_tokens")), ..Default::default()
                });
                st.prev_ts = t;
            }
        })?;
        st.off = r.offset;
        // 最后一步之后没有后续行：4 秒仍无新行就入账。日志是步骤结束时写的，文件修改时间即结束时间；
        // 文件不是刚写的（历史数据）就按开始时间入账、耗时记 0
        if st.pend.as_ref().map_or(false, |p| now - p.wall > 4000) {
            let mt = st.mt as i64;
            let end = if mt != 0 && now - mt < 60_000 { mt } else { 0 };
            ag_flush(st, sink, end);
        }
        Ok(r)
    }

    fn finish(&self, st: &mut FileState, sink: &mut dyn Sink) {
        ag_flush(st, sink, 0);
    }
}

// ---------------------------------------------------------------- ZCode
/// ZCode（智谱）CLI：用量在全局 SQLite 库 `~/.zcode/cli/db/db.sqlite` 的 model_usage 表里，一行 = 一次模型请求。
/// 格式依据 CodeBurn 项目的 docs/providers/zcode.md（库版本 0.14.8），非官方文档：
///   - input_tokens 已包含缓存读 / 写（OpenAI 口径），要减掉，否则输入会重复算约 8 倍；
///   - 时间戳是毫秒；没有花费字段，花费按价格表估算；
///   - 同目录的 JSONL 日志里 token 被打码，不可用。
pub struct ZCode {
    home: PathBuf,
}

impl ZCode {
    fn db(&self) -> PathBuf {
        self.home.join(".zcode").join("cli").join("db").join("db.sqlite")
    }
}

/// 把 SQLite 的任意数值 / 文本列读成整数（缺失、NULL、类型不对都当 0）
fn sql_int(r: &rusqlite::Row, i: usize) -> i64 {
    use rusqlite::types::ValueRef;
    match r.get_ref(i) {
        Ok(ValueRef::Integer(n)) => n,
        Ok(ValueRef::Real(f)) => f as i64,
        Ok(ValueRef::Text(t)) => std::str::from_utf8(t).ok().and_then(|s| s.trim().parse().ok()).unwrap_or(0),
        _ => 0,
    }
}

fn sql_text(r: &rusqlite::Row, i: usize) -> String {
    use rusqlite::types::ValueRef;
    match r.get_ref(i) {
        Ok(ValueRef::Text(t)) => String::from_utf8_lossy(t).into_owned(),
        Ok(ValueRef::Integer(n)) => n.to_string(),
        _ => String::new(),
    }
}

impl Collector for ZCode {
    fn name(&self) -> &'static str { "zcode" }

    fn discover(&self) -> Vec<PathBuf> {
        let f = self.db();
        if f.exists() { vec![f] } else { Vec::new() }
    }

    fn probe(&self, file: &Path) -> Option<(u64, f64)> {
        crate::sqlite::probe(file)
    }

    fn scan(&self, file: &Path, st: &mut FileState, sink: &mut dyn Sink, now: i64) -> io::Result<ReadResult> {
        let size = std::fs::metadata(file).map(|m| m.len()).unwrap_or(0);
        let done = Ok(ReadResult { offset: size, size });
        // 库被 agent 占着 / 暂时打不开：当作没有新数据，下一轮再试（不报错，免得每秒重试刷屏）
        let Ok(conn) = crate::sqlite::open_ro(file) else { return done };
        const COLS: [&str; 10] = [
            "id", "session_id", "model_id", "input_tokens", "output_tokens", "reasoning_tokens",
            "cache_creation_input_tokens", "cache_read_input_tokens", "started_at", "completed_at",
        ];
        // agent 升级后表结构变了：认不出就当没数据
        if !crate::sqlite::has_columns(&conn, "model_usage", &COLS) {
            return done;
        }
        // 水位 = 已处理到的最大时间；回看 15 分钟：还没完成的请求（见下）会晚些才入账，靠去重避免重复
        const SLACK: i64 = 15 * 60 * 1000;
        let from = (st.mark - SLACK).max(0);
        let Ok(mut q) = conn.prepare(
            "SELECT id, session_id, model_id, input_tokens, output_tokens, reasoning_tokens,              cache_creation_input_tokens, cache_read_input_tokens, started_at, completed_at              FROM model_usage WHERE COALESCE(completed_at, started_at) > ?1 ORDER BY COALESCE(completed_at, started_at), id",
        ) else { return done };
        let Ok(mut rows) = q.query([from]) else { return done };
        let mut newest = st.mark;
        while let Ok(Some(r)) = rows.next() {
            let (started, completed) = (sql_int(r, 8), sql_int(r, 9));
            let ts = if completed != 0 { completed } else { started };
            if ts == 0 {
                continue;
            }
            // 没有完成时间的请求可能还在生成（token 数还会变），10 分钟后仍无完成时间才当作已结束入账
            if completed == 0 && now - started < 10 * 60 * 1000 {
                continue;
            }
            newest = newest.max(ts);
            let (inp, out, rs) = (sql_int(r, 3).max(0) as u64, sql_int(r, 4).max(0) as u64, sql_int(r, 5).max(0) as u64);
            let (cw, cr) = (sql_int(r, 6).max(0) as u64, sql_int(r, 7).max(0) as u64);
            if inp + out + rs + cw + cr == 0 {
                continue;
            }
            let model = sql_text(r, 2);
            sink.emit(Event {
                agent: "zcode", ts,
                model: if model.is_empty() { "zcode".into() } else { model },
                sid: sql_text(r, 1),
                key: sql_text(r, 0),
                input: inp.saturating_sub(cr).saturating_sub(cw), cr, cw5: cw, cw1: 0,
                out, rs,
                dur: if completed != 0 && started != 0 { clamp_dur(completed - started) } else { 0 },
            });
        }
        st.mark = newest;
        done
    }
}

// ---------------------------------------------------------------- OpenCode
/// OpenCode：用量在 SQLite 库 `<数据目录>/opencode*.db` 里（1.14+ 起；更早的 storage/ JSON 文件格式不支持）。
/// 数据目录优先级：OPENCODE_DATA_DIR → $XDG_DATA_HOME/opencode → ~/.local/share/opencode（Windows 上也是这个）。
/// 两代表结构（依据 CodeBurn 文档与几个第三方项目的 issue，非官方文档，未在真机上实测）：
///   - 1.x：`message(id, session_id, time_created, data)`，data 里是 `role` / `modelID` / `tokens` / `time.completed`；
///   - 2.x：`session_message(id, session_id, type, seq, time_created, time_updated, data)`，角色在 type 列，
///     模型在 `$.model.id`，tokens 结构不变。
///   tokens.input 不含缓存（Anthropic 口径），推理 token 单列：out = output + reasoning，rs = reasoning。
pub struct OpenCode {
    home: PathBuf,
}

impl OpenCode {
    fn data_dir(&self) -> PathBuf {
        // 环境变量只在“真实 home”下生效，避免测试用的临时 home 被本机的环境变量带偏
        if self.home == crate::util::home_dir() {
            if let Some(d) = std::env::var_os("OPENCODE_DATA_DIR").filter(|d| !d.is_empty()) {
                return PathBuf::from(d);
            }
            if let Some(d) = std::env::var_os("XDG_DATA_HOME").filter(|d| !d.is_empty()) {
                return PathBuf::from(d).join("opencode");
            }
        }
        self.home.join(".local").join("share").join("opencode")
    }
}

/// 一个表的读取方式：1.x 与 2.x 只差表名和“角色 / 完成时间”的来源
struct OcTable {
    table: &'static str,
    cols: &'static [&'static str],
    /// 角色表达式（SQL）
    role: &'static str,
    /// 模型 id 的 JSON 路径
    model: &'static str,
    /// 完成时间表达式（SQL），取不到时用 time_created
    done: &'static str,
}

const OC_V1: OcTable = OcTable {
    table: "message",
    cols: &["id", "session_id", "time_created", "data"],
    role: "json_extract(data,'$.role')",
    model: "$.modelID",
    done: "json_extract(data,'$.time.completed')",
};

const OC_V2: OcTable = OcTable {
    table: "session_message",
    cols: &["id", "session_id", "type", "time_created", "time_updated", "data"],
    role: "type",
    model: "$.model.id",
    done: "time_updated",
};

/// 读一个表里 from 之后的助手消息；返回读到的最大时间戳（用作下次的水位）
fn oc_scan_table(conn: &rusqlite::Connection, t: &OcTable, from: i64, now: i64, sink: &mut dyn Sink) -> i64 {
    if !crate::sqlite::has_columns(conn, t.table, t.cols) {
        return from;
    }
    // json_valid 过滤掉坏行：否则一行坏 JSON 会让整条查询报错、后面的数据都读不到
    let sql = format!(
        "SELECT id, session_id, time_created, {role}, json_extract(data,'{model}'),          json_extract(data,'$.tokens.input'), json_extract(data,'$.tokens.output'), json_extract(data,'$.tokens.reasoning'),          json_extract(data,'$.tokens.cache.read'), json_extract(data,'$.tokens.cache.write'), {done}          FROM {table} WHERE time_created > ?1 AND json_valid(data) ORDER BY time_created, id",
        role = t.role, model = t.model, done = t.done, table = t.table,
    );
    let Ok(mut q) = conn.prepare(&sql) else { return from };
    let Ok(mut rows) = q.query([from]) else { return from };
    let mut newest = from;
    while let Ok(Some(r)) = rows.next() {
        if sql_text(r, 3) != "assistant" {
            continue;
        }
        let created = sql_int(r, 2);
        let done = sql_int(r, 10);
        // 消息还没写完（token 会继续累加）：等它完成；超过 10 分钟仍无完成时间就当已结束
        if done == 0 && now - created < 10 * 60 * 1000 {
            continue;
        }
        let ts = if done != 0 { done.max(created) } else { created };
        newest = newest.max(created);
        let (inp, out, rs) = (sql_int(r, 5).max(0) as u64, sql_int(r, 6).max(0) as u64, sql_int(r, 7).max(0) as u64);
        let (cr, cw) = (sql_int(r, 8).max(0) as u64, sql_int(r, 9).max(0) as u64);
        if inp + out + rs + cr + cw == 0 {
            continue;
        }
        let model = sql_text(r, 4);
        sink.emit(Event {
            agent: "opencode", ts,
            model: if model.is_empty() { "opencode".into() } else { model },
            sid: sql_text(r, 1),
            key: sql_text(r, 0),
            input: inp, cr, cw5: cw, cw1: 0, out: out + rs, rs,
            // 一条消息可能包含工具执行时间，速度会被低估，仅作参考
            dur: if done != 0 { clamp_dur(done - created) } else { 0 },
        });
    }
    newest
}

impl Collector for OpenCode {
    fn name(&self) -> &'static str { "opencode" }

    fn discover(&self) -> Vec<PathBuf> {
        let Ok(rd) = std::fs::read_dir(self.data_dir()) else { return Vec::new() };
        let mut v: Vec<PathBuf> = rd.flatten().map(|e| e.path()).filter(|p| {
            let n = p.file_name().map(|n| n.to_string_lossy().to_lowercase()).unwrap_or_default();
            n.starts_with("opencode") && n.ends_with(".db")
        }).collect();
        v.sort();
        v
    }

    fn probe(&self, file: &Path) -> Option<(u64, f64)> {
        crate::sqlite::probe(file)
    }

    fn scan(&self, file: &Path, st: &mut FileState, sink: &mut dyn Sink, now: i64) -> io::Result<ReadResult> {
        let size = std::fs::metadata(file).map(|m| m.len()).unwrap_or(0);
        let done = Ok(ReadResult { offset: size, size });
        let Ok(conn) = crate::sqlite::open_ro(file) else { return done };
        // 回看 15 分钟：没写完的消息晚些才入账，靠去重避免重复。1.x 水位放 mark，2.x 放 prev_ts
        const SLACK: i64 = 15 * 60 * 1000;
        st.mark = oc_scan_table(&conn, &OC_V1, (st.mark - SLACK).max(0), now, sink).max(st.mark);
        st.prev_ts = oc_scan_table(&conn, &OC_V2, (st.prev_ts - SLACK).max(0), now, sink).max(st.prev_ts);
        done
    }
}
