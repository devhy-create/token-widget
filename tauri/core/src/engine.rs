//! 引擎：驱动采集器增量扫描、去重、按日/小时聚合、计算实时速度、持久化缓存。
//! 缓存格式（cache.json，v:1）与 Electron 版完全一致，两版可以共用同一份缓存。
use crate::collectors::{all_collectors, Collector, Event, FileState, Sink};
use crate::plans::{self, Plan, AGENTS};
use crate::pricing::{Bucket, PriceTable};
use crate::util::{default_data_dir, home_dir, now_ms};
use base64::{engine::general_purpose::STANDARD, Engine as _};
use chrono::{Datelike, Local, TimeZone};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::io;
use std::path::{Path, PathBuf};

const LIVE_WINDOW: i64 = 60 * 1000; // 实时速度统计窗口
const LIVE_HOLD: i64 = 20 * 1000; // 事件后多久内仍算“正在生成”
const RING_KEEP: i64 = 10 * 60 * 1000; // 实时事件环保留时长
const HOUR_KEEP_DAYS: i64 = 14;
const DISCOVER_EVERY: i64 = 20 * 1000;
const HOUR_MS: i64 = 3_600_000;
const DAY_MS: f64 = 86_400_000.0;

// ---------------------------------------------------------------- 时间键（本地时区）
fn local(ms: i64) -> chrono::DateTime<Local> {
    Local.timestamp_millis_opt(ms).single().unwrap_or_else(|| Local.timestamp_millis_opt(0).unwrap())
}
pub fn day_key(ms: i64) -> String {
    local(ms).format("%Y-%m-%d").to_string()
}
fn hour_key(ms: i64) -> String {
    local(ms).format("%Y-%m-%d %H").to_string()
}
/// 本地当天 0 点的毫秒时间戳
fn midnight_of(date: chrono::NaiveDate) -> i64 {
    let n = date.and_hms_opt(0, 0, 0).unwrap();
    Local.from_local_datetime(&n).earliest().map(|d| d.timestamp_millis()).unwrap_or(0)
}

/// 事件哈希（跨文件去重）：md5 的 base64 前 11 位，与 JS 版一致，旧缓存里的 seen 可直接沿用
fn hash(s: &str) -> String {
    let d = md5::compute(s.as_bytes());
    STANDARD.encode(d.0)[..11].to_string()
}

// ---------------------------------------------------------------- 桶与汇总
/// 桶：[请求数, in, cr, cw5, cw1, out, rs]
type B = [u64; 7];
type Store = BTreeMap<String, BTreeMap<String, BTreeMap<String, B>>>;

fn total_of(a: &B) -> u64 {
    a[1] + a[2] + a[3] + a[4] + a[5]
}
fn to_bucket(a: &B) -> Bucket {
    Bucket { input: a[1] as f64, cr: a[2] as f64, cw5: a[3] as f64, cw1: a[4] as f64, out: a[5] as f64 }
}

#[derive(Clone, Debug, Serialize)]
pub struct ModelView {
    pub model: String,
    pub tokens: u64,
    pub out: u64,
    pub n: u64,
    pub usd: f64,
    pub est: bool,
    pub known: bool,
}

#[derive(Clone, Debug, Serialize)]
pub struct PeriodView {
    pub n: u64,
    #[serde(rename = "in")]
    pub input: u64,
    pub cr: u64,
    pub cw: u64,
    pub out: u64,
    pub rs: u64,
    pub tokens: u64,
    pub usd: f64,
    pub est: bool,
    pub unknown: bool,
    pub models: Vec<ModelView>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct LastEv {
    ts: i64,
    model: String,
    out: u64,
    dur: i64,
}

#[derive(Clone, Debug)]
struct RingEv {
    agent: String,
    ts: i64,
    model: String,
    out: u64,
    tot: u64,
    dur: i64,
}

// ---------------------------------------------------------------- 账本（事件入账的落点）
/// 去重集合 + 日/小时聚合 + 实时事件环 + 额度窗口。实现 Sink，供采集器直接写入。
#[derive(Default)]
struct Ledger {
    seen: HashSet<String>,
    days: Store,
    hours: Store,
    limits: BTreeMap<String, Value>,
    ring: Vec<RingEv>,
    last_ev: BTreeMap<String, LastEv>,
    log_plans: BTreeMap<String, String>,
    dirty: bool,
    ver: u64,
}

impl Ledger {
    fn add(store: &mut Store, key: String, ev: &Event) {
        let b = store.entry(key).or_default().entry(ev.agent.to_string()).or_default().entry(ev.model.clone()).or_insert([0; 7]);
        b[0] += 1;
        b[1] += ev.input;
        b[2] += ev.cr;
        b[3] += ev.cw5;
        b[4] += ev.cw1;
        b[5] += ev.out;
        b[6] += ev.rs;
    }

    fn put_limits(&mut self, agent: &str, ts: i64, rl: &Value) {
        if let Some(cur) = self.limits.get(agent) {
            if cur.get("ts").and_then(Value::as_i64).map_or(false, |c| c > ts) {
                return;
            }
        }
        let plan = rl.get("plan_type").and_then(Value::as_str).filter(|s| !s.is_empty());
        if let Some(p) = plan {
            self.log_plans.insert(agent.to_string(), p.to_string());
        }
        let pick = |k: &str| rl.get(k).filter(|v| !v.is_null()).cloned().unwrap_or(Value::Null);
        self.limits.insert(agent.to_string(), json!({
            "ts": ts, "plan": plan, "primary": pick("primary"), "secondary": pick("secondary"), "models": pick("models"),
        }));
        self.dirty = true;
    }
}

impl Sink for Ledger {
    fn emit(&mut self, ev: Event) {
        let k = hash(&format!("{}:{}", ev.agent, ev.key));
        if !self.seen.insert(k) {
            return;
        }
        Self::add(&mut self.days, day_key(ev.ts), &ev);
        Self::add(&mut self.hours, hour_key(ev.ts), &ev);
        self.dirty = true;
        self.ver += 1;
        if ev.ts > now_ms() - RING_KEEP {
            self.ring.push(RingEv {
                agent: ev.agent.to_string(), ts: ev.ts, model: ev.model.clone(), out: ev.out,
                tot: ev.input + ev.cr + ev.cw5 + ev.cw1 + ev.out, dur: ev.dur,
            });
        }
        let newer = self.last_ev.get(ev.agent).map_or(true, |le| ev.ts >= le.ts);
        if newer {
            self.last_ev.insert(ev.agent.to_string(), LastEv { ts: ev.ts, model: ev.model, out: ev.out, dur: ev.dur });
        }
    }

    fn set_limits(&mut self, agent: &str, ts: i64, rl: &Value) {
        self.put_limits(agent, ts, rl);
    }
}

// ---------------------------------------------------------------- 缓存文件
#[derive(Serialize, Deserialize, Default)]
#[serde(default)]
struct Cache {
    v: u32,
    files: BTreeMap<String, FileState>,
    seen: Vec<String>,
    days: Store,
    hours: Store,
    limits: BTreeMap<String, Value>,
    #[serde(rename = "lastEv")]
    last_ev: BTreeMap<String, LastEv>,
    #[serde(rename = "logPlans")]
    log_plans: BTreeMap<String, String>,
}

#[derive(Clone, Debug, Serialize, Default)]
pub struct Status {
    pub scanning: bool,
    pub done: usize,
    pub total: usize,
}

/// 一个待扫描的日志文件
pub struct ScanJob {
    ci: usize,
    path: PathBuf,
    key: String,
    size: u64,
}

pub struct EngineOptions {
    /// 数据目录；None 用默认位置（与 Electron 版共用）
    pub dir: Option<PathBuf>,
    /// 用户主目录（各 agent 日志所在）；None 取系统主目录。测试里指向临时目录
    pub home: Option<PathBuf>,
    /// true 则忽略已有缓存，从头扫描
    pub fresh: bool,
}

pub struct Engine {
    dir: PathBuf,
    home: PathBuf,
    cache_file: PathBuf,
    pricing_user: PathBuf,
    plans_user: PathBuf,
    plans: BTreeMap<String, Option<Plan>>,
    plans_at: i64,
    plan_mtime: f64,
    pricing: PriceTable,
    pricing_mtime: f64,
    files: BTreeMap<String, FileState>,
    ledger: Ledger,
    collectors: Vec<Box<dyn Collector>>,
    lists: HashMap<String, Vec<PathBuf>>,
    last_discover: HashMap<String, i64>,
    cache_snap: Option<(String, Value)>,
    claude_lim_mtime: Option<f64>,
    pub status: Status,
}

fn mtime_ms(p: &Path) -> Option<f64> {
    let m = std::fs::metadata(p).ok()?.modified().ok()?;
    let d = m.duration_since(std::time::UNIX_EPOCH).ok()?;
    Some(d.as_secs_f64() * 1000.0)
}

impl Engine {
    pub fn new(opts: EngineOptions) -> io::Result<Self> {
        let dir = opts.dir.unwrap_or_else(default_data_dir);
        let home = opts.home.unwrap_or_else(home_dir);
        std::fs::create_dir_all(&dir)?;
        let pricing_user = dir.join("pricing.user.json");
        let mut e = Engine {
            cache_file: dir.join("cache.json"),
            plans_user: dir.join("plans.user.json"),
            pricing: PriceTable::load(Some(&pricing_user)),
            pricing_user,
            collectors: all_collectors(&home),
            dir, home,
            plans: BTreeMap::new(), plans_at: 0, plan_mtime: 0.0, pricing_mtime: 0.0,
            files: BTreeMap::new(), ledger: Ledger::default(),
            lists: HashMap::new(), last_discover: HashMap::new(),
            cache_snap: None, claude_lim_mtime: None, status: Status::default(),
        };
        if !opts.fresh {
            e.load();
        }
        Ok(e)
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    pub fn home(&self) -> &Path {
        &self.home
    }

    pub fn cache_file(&self) -> &Path {
        &self.cache_file
    }

    pub fn plans_user_file(&self) -> &Path {
        &self.plans_user
    }

    pub fn pricing_user_file(&self) -> &Path {
        &self.pricing_user
    }

    /// 日志里带出的套餐类型（Codex 的 plan_type），设置页展示“自动识别”结果时用
    pub fn log_plans(&self) -> &BTreeMap<String, String> {
        &self.ledger.log_plans
    }

    /// 用户改了套餐覆盖后立即重新检测，不等 5 分钟
    pub fn refresh_plans_now(&mut self) {
        self.plans_at = 0;
        self.refresh_plans(now_ms());
    }

    fn load(&mut self) {
        // 首次运行或缓存损坏：从头扫描
        let Some(c) = std::fs::read_to_string(&self.cache_file).ok().and_then(|s| serde_json::from_str::<Cache>(&s).ok()) else { return };
        if c.v != 1 {
            return;
        }
        self.files = c.files;
        self.ledger.seen = c.seen.into_iter().collect();
        self.ledger.days = c.days;
        self.ledger.hours = c.hours;
        self.ledger.limits = c.limits;
        self.ledger.log_plans = c.log_plans;
        self.ledger.last_ev = c.last_ev;
    }

    pub fn save(&mut self) -> io::Result<()> {
        if !self.ledger.dirty {
            return Ok(());
        }
        let l = &self.ledger;
        let body = serde_json::to_string(&json!({
            "v": 1, "files": self.files, "seen": l.seen, "days": l.days, "hours": l.hours,
            "limits": l.limits, "lastEv": l.last_ev, "logPlans": l.log_plans,
        }))?;
        // 先写临时文件再改名，避免写到一半崩溃留下半截缓存
        let tmp = self.cache_file.with_extension("json.tmp");
        std::fs::write(&tmp, body)?;
        std::fs::rename(&tmp, &self.cache_file)?;
        self.ledger.dirty = false;
        Ok(())
    }

    /// 外部来源（Antigravity 本地服务、Claude 钩子文件）写入额度窗口
    pub fn set_external_limits(&mut self, agent: &str, rl: &Value) {
        self.ledger.put_limits(agent, now_ms(), rl);
    }

    /// 以指定时间戳写入额度窗口；较旧的时间戳不会覆盖较新的
    pub fn set_limits_at(&mut self, agent: &str, ts: i64, rl: &Value) {
        self.ledger.put_limits(agent, ts, rl);
    }

    /// 扫描一轮（阻塞到完成）。窗口壳想在扫描途中响应界面请求时，改用
    /// begin_tick → 逐个 scan_job → end_tick，每一步之间可以释放对引擎的锁。
    pub fn tick(&mut self) {
        let now = now_ms();
        let jobs = self.begin_tick(now);
        for j in &jobs {
            self.scan_job(j);
        }
        self.end_tick(now);
    }

    /// 第一步：发现文件并 stat，返回需要扫描的任务（只含有增长的文件），很快
    pub fn begin_tick(&mut self, now: i64) -> Vec<ScanJob> {
        // 发现文件（每个采集器 20 秒重新发现一次）
        for c in &self.collectors {
            let name = c.name().to_string();
            if !self.lists.contains_key(&name) || now - self.last_discover.get(&name).copied().unwrap_or(0) > DISCOVER_EVERY {
                self.lists.insert(name.clone(), c.discover());
                self.last_discover.insert(name, now);
            }
        }
        // 先 stat 一遍，只处理有增长的文件
        let mut work: Vec<ScanJob> = Vec::new();
        for (ci, c) in self.collectors.iter().enumerate() {
            for f in self.lists.get(c.name()).into_iter().flatten() {
                let key = f.to_string_lossy().into_owned();
                let st = self.files.entry(key.clone()).or_default();
                // 冷热分层：10 分钟内修改过的“热文件”每轮都查，其余文件每 30 秒才查一次，省掉上千次 stat
                if st.chk != 0 && (now as f64 - st.mt) > 600_000.0 && now - st.chk < 30_000 && st.pend.is_none() {
                    continue;
                }
                let (size, mt) = match c.probe(f) {
                    Some(p) => p,
                    None => {
                        let Ok(meta) = std::fs::metadata(f) else { continue };
                        (meta.len(), mtime_ms(f).unwrap_or(0.0))
                    }
                };
                st.chk = now;
                st.mt = mt;
                if size < st.seen {
                    // 文件被截断/重写：重读，靠去重避免重复入账
                    st.off = 0;
                    st.seen = 0;
                    st.prev_ts = 0;
                    st.mark = 0;
                }
                // 用“上次见到的文件大小”判断是否有新内容；否则末尾没有换行的文件会每轮都被重新打开
                if size > st.seen || st.pend.is_some() {
                    work.push(ScanJob { ci, path: f.clone(), key, size });
                }
            }
        }
        self.status = Status { scanning: work.len() > 3, done: 0, total: work.len() };
        work
    }

    /// 第二步：扫描一个文件。单个文件出错不影响其它
    pub fn scan_job(&mut self, job: &ScanJob) {
        if let Some(st) = self.files.get_mut(&job.key) {
            if self.collectors[job.ci].scan(&job.path, st, &mut self.ledger, now_ms()).is_ok() {
                st.seen = job.size;
            }
        }
        self.ledger.dirty = true;
        self.status.done += 1;
    }

    /// 第三步：收尾（清理过期数据、热加载价格表 / 套餐 / Claude 额度文件）
    pub fn end_tick(&mut self, now: i64) {
        self.status.scanning = false;
        self.prune(now);
        self.check_pricing();
        self.refresh_plans(now);
        self.read_claude_limits();
    }

    /// Claude 的 5h / 周额度：日志里没有，由 Claude Code 的 statusLine 钩子写到 claude-limits.json
    pub fn read_claude_limits(&mut self) {
        let f = self.dir.join("claude-limits.json");
        let Some(m) = mtime_ms(&f) else { return }; // 没配置钩子就没有这个文件
        if self.claude_lim_mtime == Some(m) {
            return;
        }
        self.claude_lim_mtime = Some(m);
        // 文件损坏就等下一次写入
        let Some(j) = std::fs::read_to_string(&f).ok().and_then(|s| serde_json::from_str::<Value>(&s).ok()) else { return };
        let rl = j.get("rate_limits").cloned().unwrap_or(Value::Null);
        // resets_at 可能是秒级时间戳或 ISO 字符串，统一成秒
        let sec = |v: Option<&Value>| -> i64 {
            match v {
                Some(Value::Number(n)) => {
                    let f = n.as_f64().unwrap_or(0.0);
                    (if f > 1e12 { f / 1000.0 } else { f }).round() as i64
                }
                Some(Value::String(s)) if !s.is_empty() => {
                    crate::util::parse_date_ms(s).map(|ms| (ms as f64 / 1000.0).round() as i64).unwrap_or(0)
                }
                _ => 0,
            }
        };
        let mk = |w: Option<&Value>, minutes: i64| -> Option<Value> {
            let w = w.filter(|w| w.is_object())?;
            let used = ["used_percentage", "used_percent", "utilization"].iter().filter_map(|k| w.get(*k)).find(|v| !v.is_null())?;
            let used = used.as_f64().or_else(|| used.as_str().and_then(|s| s.parse().ok()))?;
            Some(json!({ "used_percent": used, "window_minutes": minutes, "resets_at": sec(w.get("resets_at")) }))
        };
        let primary = mk(rl.get("five_hour"), 300);
        let secondary = mk(rl.get("seven_day"), 10080);
        if primary.is_some() || secondary.is_some() {
            let ts = j.get("ts").and_then(Value::as_f64).map(|f| f as i64).unwrap_or(m as i64);
            self.ledger.put_limits("claude", ts, &json!({ "primary": primary, "secondary": secondary }));
        }
    }

    /// 套餐 5 分钟重检一次；用户改了 plans.user.json 立即生效
    fn refresh_plans(&mut self, now: i64) {
        let m = mtime_ms(&self.plans_user).unwrap_or(0.0);
        if now - self.plans_at < 300_000 && m == self.plan_mtime {
            return;
        }
        self.plans_at = now;
        self.plan_mtime = m;
        self.plans = plans::detect(&self.home, &self.plans_user, &self.ledger.log_plans);
    }

    fn check_pricing(&mut self) {
        match mtime_ms(&self.pricing_user) {
            Some(m) if m != self.pricing_mtime => {
                self.pricing_mtime = m;
                self.pricing = PriceTable::load(Some(&self.pricing_user));
            }
            Some(_) => {}
            None => self.pricing_mtime = 0.0,
        }
    }

    fn prune(&mut self, now: i64) {
        let cut = now - RING_KEEP;
        self.ledger.ring.retain(|e| e.ts >= cut);
        let hc = hour_key(now - HOUR_KEEP_DAYS * 86_400_000);
        let before = self.ledger.hours.len();
        self.ledger.hours.retain(|k, _| *k >= hc);
        if self.ledger.hours.len() != before {
            self.ledger.dirty = true;
        }
    }

    /// 进程退出：把还在合并中的消息入账，并落盘
    pub fn shutdown(&mut self) {
        for c in &self.collectors {
            for st in self.files.values_mut() {
                if st.pend.is_some() {
                    c.finish(st, &mut self.ledger);
                }
            }
        }
        let _ = self.save();
    }

    // ---- 汇总 ----
    fn sum(&self, store: &Store, keys: &[String], agent: &str) -> (B, BTreeMap<String, B>) {
        let mut total = [0u64; 7];
        let mut models: BTreeMap<String, B> = BTreeMap::new();
        for k in keys {
            let Some(a) = store.get(k).and_then(|d| d.get(agent)) else { continue };
            for (m, b) in a {
                let t = models.entry(m.clone()).or_insert([0; 7]);
                for i in 0..7 {
                    t[i] += b[i];
                    total[i] += b[i];
                }
            }
        }
        (total, models)
    }

    fn period_view(&self, sum: (B, BTreeMap<String, B>)) -> PeriodView {
        let (t, models) = sum;
        let (mut usd, mut est, mut unknown) = (0.0, false, false);
        let mut ms: Vec<ModelView> = models.iter().map(|(m, a)| {
            let c = self.pricing.cost(m, &to_bucket(a));
            usd += c.usd;
            est = est || (c.known && c.est);
            unknown = unknown || !c.known;
            ModelView { model: m.clone(), tokens: total_of(a), out: a[5], n: a[0], usd: c.usd, est: c.est, known: c.known }
        }).collect();
        ms.sort_by(|x, y| y.tokens.cmp(&x.tokens)); // 稳定排序，并列时保持模型名顺序
        PeriodView { n: t[0], input: t[1], cr: t[2], cw: t[3] + t[4], out: t[5], rs: t[6], tokens: total_of(&t), usd, est, unknown, models: ms }
    }

    fn view(&self, store: &Store, keys: &[String], agent: &str) -> PeriodView {
        self.period_view(self.sum(store, keys, agent))
    }

    /// 静态汇总（今日/近7天/近30天/累计/序列）：只在有新事件或跨小时时重算
    fn static_part(&mut self, now: i64) -> Value {
        let stamp = format!("{}|{}", self.ledger.ver, hour_key(now));
        if let Some((s, v)) = &self.cache_snap {
            if *s == stamp {
                return v.clone();
            }
        }
        let last_n = |n: i64| -> Vec<String> { (0..n).map(|i| day_key(now - (n - 1 - i) * 86_400_000)).collect() };
        let (d7, d30, d14) = (last_n(7), last_n(30), last_n(14));
        let today = vec![day_key(now)];
        let all_days: Vec<String> = self.ledger.days.keys().cloned().collect();
        let mut agents = serde_json::Map::new();
        for ag in AGENTS {
            let days = &self.ledger.days;
            let series: Vec<Value> = d14.iter().map(|k| {
                let v = self.view(days, std::slice::from_ref(k), ag);
                json!({ "day": k, "tokens": v.tokens, "usd": v.usd })
            }).collect();
            agents.insert(ag.to_string(), json!({
                "today": self.view(days, &today, ag), "d7": self.view(days, &d7, ag),
                "d30": self.view(days, &d30, ag), "all": self.view(days, &all_days, ag), "series": series,
            }));
        }
        let v = Value::Object(agents);
        self.cache_snap = Some((stamp, v.clone()));
        v
    }

    /// 近 N 小时窗口内（含端点所在小时）的汇总，复刻 JS 版 hoursBetween 的取键方式
    fn hours_between(&self, from: i64, to: i64, ag: &str) -> PeriodView {
        let mut keys: Vec<String> = Vec::new();
        let mut t = from;
        while t <= to + HOUR_MS {
            let k = hour_key(t);
            if keys.last() != Some(&k) {
                keys.push(k.clone());
            }
            if hour_key(to) == k {
                break;
            }
            t += HOUR_MS;
        }
        self.view(&self.ledger.hours, &keys, ag)
    }

    /// 推算：近 5 小时、本周期（有额度窗口按窗口，否则按自然周）、本月
    fn proj(&self, ag: &str, now: i64, limits: Option<&Value>, d7: &Value) -> Value {
        let slim = |v: &PeriodView| json!({ "usd": v.usd, "tokens": v.tokens, "est": v.est });
        let five = slim(&self.hours_between(now - 4 * HOUR_MS, now, ag));

        // 本周期：Codex / Claude 用额度窗口；其它 agent 按周一 00:00 起的自然周
        let sec = limits.and_then(|l| l.get("secondary")).filter(|s| s.is_object());
        let sec_resets = sec.and_then(|s| s.get("resets_at")).and_then(Value::as_f64).unwrap_or(0.0);
        let (kind, w_start, w_end);
        if sec.is_some() && sec_resets * 1000.0 > now as f64 {
            kind = "window";
            w_end = (sec_resets * 1000.0) as i64;
            let wm = sec.and_then(|s| s.get("window_minutes")).and_then(Value::as_f64).unwrap_or(0.0);
            w_start = w_end - (wm * 60000.0) as i64;
        } else {
            kind = "calendar";
            let d = local(now).date_naive();
            let monday = d - chrono::Duration::days(d.weekday().num_days_from_monday() as i64);
            w_start = midnight_of(monday);
            w_end = w_start + 7 * 86_400_000;
        }
        let w_used = self.hours_between(w_start, now, ag);
        let w_frac = ((now - w_start) as f64 / (w_end - w_start) as f64).clamp(0.0, 1.0);
        let w_ok = w_frac >= 0.04; // 周期刚开始时外推不可靠
        let used_pct = sec.and_then(|s| s.get("used_percent")).and_then(Value::as_f64);
        let window = kind == "window";
        let week = json!({
            "kind": kind, "start": w_start, "end": w_end, "frac": w_frac, "ok": w_ok, "used": slim(&w_used),
            "projUsd": if w_ok { json!(w_used.usd / w_frac) } else { Value::Null },
            "projTokens": if w_ok { json!(w_used.tokens as f64 / w_frac) } else { Value::Null },
            "pct": if window { json!(used_pct) } else { Value::Null },
            "pctProj": match used_pct { Some(p) if window && w_ok => json!(p / w_frac), _ => Value::Null },
        });

        // 本月：自然月至今 / 已过比例
        let d0 = local(now).date_naive().with_day(1).unwrap();
        let m_start = midnight_of(d0);
        let d1 = if d0.month() == 12 { chrono::NaiveDate::from_ymd_opt(d0.year() + 1, 1, 1) } else { chrono::NaiveDate::from_ymd_opt(d0.year(), d0.month() + 1, 1) }.unwrap();
        let m_end = midnight_of(d1);
        let m_frac = (now - m_start) as f64 / (m_end - m_start) as f64;
        let mut m_keys: Vec<String> = Vec::new();
        let mut t = m_start;
        while t <= now {
            m_keys.push(day_key(t));
            t += 86_400_000;
        }
        if m_keys.last() != Some(&day_key(now)) {
            m_keys.push(day_key(now));
        }
        let m_used = self.view(&self.ledger.days, &m_keys, ag);
        let m_ok = m_frac >= 0.03;
        let d7_usd = d7.get("usd").and_then(Value::as_f64).unwrap_or(0.0);
        let d7_tokens = d7.get("tokens").and_then(Value::as_f64).unwrap_or(0.0);
        let rest_days = (m_end - now) as f64 / DAY_MS;
        let month = json!({
            "frac": m_frac, "ok": m_ok, "used": slim(&m_used),
            "projUsd": if m_ok { json!(m_used.usd / m_frac) } else { Value::Null },
            "projTokens": if m_ok { json!(m_used.tokens as f64 / m_frac) } else { Value::Null },
            // 另一种口径：本月至今 + 近 7 天日均 × 剩余天数（更贴近当前节奏）
            "projUsd7": m_used.usd + (d7_usd / 7.0) * rest_days,
            "projTokens7": m_used.tokens as f64 + (d7_tokens / 7.0) * rest_days,
            "daysLeft": ((m_end - now) as f64 / DAY_MS).ceil(),
        });
        json!({ "five": five, "week": week, "month": month })
    }

    /// 本机是否有这个 agent 的日志（或历史数据）；没有就不该在界面（卡片 / 设置 / 菜单）里出现
    pub fn found(&self, ag: &str) -> bool {
        self.lists.get(ag).map_or(false, |l| !l.is_empty()) || self.ledger.last_ev.contains_key(ag)
    }

    /// 第一轮文件发现是否已完成
    pub fn booted(&self) -> bool {
        !self.lists.is_empty()
    }

    pub fn snapshot_now(&mut self) -> Value {
        self.snapshot(now_ms())
    }

    pub fn snapshot(&mut self, now: i64) -> Value {
        let st = self.static_part(now);
        let mut agents = serde_json::Map::new();
        for ag in AGENTS {
            let limits = self.ledger.limits.get(ag).cloned();
            let base = st.get(ag).cloned().unwrap_or(Value::Null);
            let d7 = base.get("d7").cloned().unwrap_or(Value::Null);
            let mut obj = base.as_object().cloned().unwrap_or_default();
            obj.insert("live".into(), self.live(ag, now));
            obj.insert("proj".into(), self.proj(ag, now, limits.as_ref(), &d7));
            obj.insert("limits".into(), limits.unwrap_or(Value::Null));
            obj.insert("last".into(), self.ledger.last_ev.get(ag).map_or(Value::Null, |l| json!(l)));
            // 本机是否有这个 agent 的日志（或历史数据）；没有就让界面隐藏该卡片
            obj.insert("found".into(), json!(self.found(ag)));
            obj.insert("plan".into(), self.plans.get(ag).and_then(|p| p.as_ref()).map_or(Value::Null, |p| json!(p)));
            agents.insert(ag.to_string(), Value::Object(obj));
        }
        // booted：第一轮文件发现是否已完成；没完成前所有 agent 的 found 都是 false，界面据此先不下结论
        json!({ "now": now, "agents": agents, "status": self.status, "booted": self.booted() })
    }

    fn live(&self, agent: &str, now: i64) -> Value {
        let evs: Vec<&RingEv> = self.ledger.ring.iter().filter(|e| e.agent == agent && e.ts > now - LIVE_WINDOW * 5).collect();
        let win: Vec<&&RingEv> = evs.iter().filter(|e| e.ts > now - LIVE_WINDOW).collect();
        // 输出 token / 生成耗时（只算耗时可信的事件）
        let rate = |list: &[&RingEv]| -> Option<f64> {
            let (mut out, mut ms) = (0u64, 0i64);
            for e in list {
                if e.dur > 0 {
                    out += e.out;
                    ms += e.dur;
                }
            }
            if ms > 0 { Some(out as f64 / (ms as f64 / 1000.0)) } else { None }
        };
        let tps_of = |e: &RingEv| if e.dur > 0 { Some(e.out as f64 / (e.dur as f64 / 1000.0)) } else { None };
        let last = evs.last();
        let mut by_model: Vec<(String, Vec<&RingEv>)> = Vec::new();
        for e in &evs {
            match by_model.iter_mut().find(|(m, _)| *m == e.model) {
                Some((_, v)) => v.push(e),
                None => by_model.push((e.model.clone(), vec![e])),
            }
        }
        let mut models: Vec<(i64, Value)> = by_model.iter().map(|(m, list)| {
            let lw: Vec<&RingEv> = list.iter().filter(|e| e.ts > now - LIVE_WINDOW).copied().collect();
            let l = list.last().unwrap();
            (l.ts, json!({
                "model": m, "tps": rate(&lw), "lastTps": tps_of(l), "lastTs": l.ts,
                "active": now - l.ts < LIVE_HOLD, "tokens": list.iter().map(|e| e.tot).sum::<u64>(),
            }))
        }).collect();
        models.sort_by(|a, b| b.0.cmp(&a.0));
        let win_refs: Vec<&RingEv> = win.iter().map(|e| **e).collect();
        json!({
            "active": last.map_or(false, |l| now - l.ts < LIVE_HOLD),
            "tps": rate(&win_refs), // 近 60 秒平均输出速度 tok/s
            "lastTps": last.and_then(|l| tps_of(l)),
            "perMin": win_refs.iter().map(|e| e.tot).sum::<u64>(), // 近 60 秒总吞吐 token（含缓存）
            "outMin": win_refs.iter().map(|e| e.out).sum::<u64>(), // 近 60 秒输出 token
            "reqMin": win_refs.len(), // 近 60 秒请求数
            "lastTs": last.map_or(0, |l| l.ts),
            "model": last.map(|l| l.model.clone()),
            "models": models.into_iter().map(|(_, v)| v).collect::<Vec<_>>(),
        })
    }
}
