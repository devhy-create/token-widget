// 引擎：驱动采集器增量扫描、去重、按日/小时聚合、计算实时速度、持久化缓存。
const fs = require('fs');
const path = require('path');
const os = require('os');
const crypto = require('crypto');
const pricing = require('./pricing');
const plans = require('./plans');
const { collectors } = require('./collectors');

const AGENTS = ['claude', 'codex', 'antigravity', 'workbuddy'];
const LIVE_WINDOW = 60 * 1000;     // 实时速度统计窗口
const LIVE_HOLD = 20 * 1000;       // 事件后多久内仍算“正在生成”
const RING_KEEP = 10 * 60 * 1000;  // 实时事件环保留时长
const HOUR_KEEP_DAYS = 14;
const DISCOVER_EVERY = 20 * 1000;

const pad = (n) => String(n).padStart(2, '0');
const dayKey = (t) => { const d = new Date(t); return `${d.getFullYear()}-${pad(d.getMonth() + 1)}-${pad(d.getDate())}`; };
const hourKey = (t) => `${dayKey(t)} ${pad(new Date(t).getHours())}`;
const hash = (s) => crypto.createHash('md5').update(s).digest('base64').slice(0, 11);

// 桶：[请求数, in, cr, cw5, cw1, out, rs]
const zero = () => [0, 0, 0, 0, 0, 0, 0];
const toB = (a) => ({ n: a[0], in: a[1], cr: a[2], cw5: a[3], cw1: a[4], out: a[5], rs: a[6] });
const totalOf = (a) => a[1] + a[2] + a[3] + a[4] + a[5];

class Engine {
  constructor(opts = {}) {
    const dir = opts.dir || path.join(process.env.APPDATA || path.join(os.homedir(), '.config'), 'token-widget');
    fs.mkdirSync(dir, { recursive: true });
    this.dir = dir;
    this.cacheFile = path.join(dir, 'cache.json');
    this.pricingUser = path.join(dir, 'pricing.user.json');
    this.plansUser = path.join(dir, 'plans.user.json');
    this.plans = {};
    this.plansAt = 0;
    this.planMtime = 0;
    this.logPlans = {};
    pricing.load(this.pricingUser);
    this.files = {};            // path -> 采集状态
    this.seen = new Set();      // 已入账事件的哈希（跨文件去重：会话分叉/归档会复制历史）
    this.days = {};             // day -> agent -> model -> 桶
    this.hours = {};
    this.limits = {};           // agent -> 额度窗口（目前只有 Codex 在日志里带）
    this.ring = [];             // 最近事件，用于实时速度
    this.lastEv = {};           // agent -> 最近一次事件
    this.lists = {};            // collector -> 文件列表
    this.lastDiscover = {};
    this.dirty = false;
    this.ver = 0;               // 有新事件入账就 +1，用于缓存汇总
    this.cacheSnap = null;
    this.busy = false;
    this.status = { scanning: false, done: 0, total: 0 };
    this.pricingMtime = 0;
    if (!opts.fresh) this._load();
  }

  _load() {
    try {
      const j = JSON.parse(fs.readFileSync(this.cacheFile, 'utf8'));
      if (j.v !== 1) return;
      this.files = j.files || {};
      this.seen = new Set(j.seen || []);
      this.days = j.days || {};
      this.hours = j.hours || {};
      this.limits = j.limits || {};
      this.logPlans = j.logPlans || {};
      this.lastEv = j.lastEv || {};
    } catch { /* 首次运行或缓存损坏：从头扫描 */ }
  }

  save() {
    if (!this.dirty) return;
    const files = {};
    for (const [k, v] of Object.entries(this.files)) { const { pend, ...rest } = v; files[k] = rest; }
    const body = JSON.stringify({ v: 1, files, seen: [...this.seen], days: this.days, hours: this.hours, limits: this.limits, lastEv: this.lastEv, logPlans: this.logPlans });
    const tmp = this.cacheFile + '.tmp';
    fs.writeFileSync(tmp, body);
    fs.renameSync(tmp, this.cacheFile);
    this.dirty = false;
  }

  _emit(ev) {
    const k = hash(ev.agent + ':' + ev.key);
    if (this.seen.has(k)) return;
    this.seen.add(k);
    const add = (store, key) => {
      const a = ((store[key] ||= {})[ev.agent] ||= {});
      const b = (a[ev.model] ||= zero());
      b[0] += 1; b[1] += ev.in; b[2] += ev.cr; b[3] += ev.cw5; b[4] += ev.cw1; b[5] += ev.out; b[6] += ev.rs;
    };
    add(this.days, dayKey(ev.ts));
    add(this.hours, hourKey(ev.ts));
    this.dirty = true;
    this.ver++;
    const now = Date.now();
    if (ev.ts > now - RING_KEEP) {
      this.ring.push({ agent: ev.agent, ts: ev.ts, model: ev.model, out: ev.out, tot: ev.in + ev.cr + ev.cw5 + ev.cw1 + ev.out, dur: ev.dur });
    }
    const le = this.lastEv[ev.agent];
    if (!le || ev.ts >= le.ts) this.lastEv[ev.agent] = { ts: ev.ts, model: ev.model, out: ev.out, dur: ev.dur };
  }

  _setLimits(agent, ts, rl) {
    const cur = this.limits[agent];
    if (cur && cur.ts > ts) return;
    if (rl.plan_type) this.logPlans[agent] = rl.plan_type;
    this.limits[agent] = { ts, plan: rl.plan_type || null, primary: rl.primary || null, secondary: rl.secondary || null, models: rl.models || null };
    this.dirty = true;
  }

  // 外部来源（Antigravity 本地服务、Claude 钩子文件）写入额度窗口
  setExternalLimits(agent, rl) { this._setLimits(agent, Date.now(), rl); }

  // 扫描一轮。大文件首次扫描较慢，文件之间让出事件循环，避免界面卡住。
  async tick() {
    if (this.busy) return;
    this.busy = true;
    try {
      const now = Date.now();
      const jobs = [];
      for (const c of collectors) {
        if (!this.lists[c.name] || now - (this.lastDiscover[c.name] || 0) > DISCOVER_EVERY) {
          this.lists[c.name] = c.discover();
          this.lastDiscover[c.name] = now;
        }
        for (const f of this.lists[c.name]) jobs.push([c, f]);
      }
      // 先 stat 一遍，只处理有增长的文件
      const work = [];
      for (const [c, f] of jobs) {
        const st = (this.files[f] ||= { off: 0 });
        // 冷热分层：10 分钟内修改过的“热文件”每轮都查，其余文件每 30 秒才查一次，省掉上千次 stat
        if (st.chk && now - (st.mt || 0) > 600000 && now - st.chk < 30000 && !st.pend) continue;
        let stat;
        try { stat = fs.statSync(f); } catch { continue; }
        const size = stat.size;
        st.chk = now;
        st.mt = stat.mtimeMs;
        if (size < (st.seen || 0)) { st.off = 0; st.seen = 0; delete st.prevTs; delete st.mark; } // 文件被截断/重写：重读，靠去重避免重复入账
        // 用“上次见到的文件大小”判断是否有新内容；否则末尾没有换行的文件会每轮都被重新打开
        if (size > (st.seen || 0) || st.pend) work.push([c, f, st, size]);
      }
      this.status = { scanning: work.length > 3, done: 0, total: work.length };
      const emit = (ev) => this._emit(ev);
      for (const [c, f, st, size] of work) {
        try { c.scan(f, st, emit, Date.now(), (a, t, rl) => this._setLimits(a, t, rl)); st.seen = size; } catch { /* 单个文件出错不影响其它 */ }
        this.dirty = true;
        this.status.done++;
        if (work.length > 3) await new Promise((r) => setImmediate(r));
      }
      this.status.scanning = false;
      this._prune(now);
      this._checkPricing();
      this._refreshPlans(now);
      this._readClaudeLimits();
    } finally {
      this.busy = false;
    }
  }

  // Claude 的 5h / 周额度：日志里没有，由 tools/claude-statusline.js（Claude Code 的 statusLine 钩子）写到 claude-limits.json
  _readClaudeLimits() {
    const f = path.join(this.dir, 'claude-limits.json');
    let m;
    try { m = fs.statSync(f).mtimeMs; } catch { return; } // 没配置钩子就没有这个文件
    if (m === this.claudeLimMtime) return;
    this.claudeLimMtime = m;
    try {
      const j = JSON.parse(fs.readFileSync(f, 'utf8'));
      const rl = j.rate_limits || {};
      // resets_at 可能是秒级时间戳或 ISO 字符串，统一成秒
      const sec = (v) => (typeof v === 'number' ? (v > 1e12 ? Math.round(v / 1000) : v) : v ? Math.round(Date.parse(v) / 1000) : 0);
      const mk = (w, minutes) => {
        if (!w) return null;
        const used = w.used_percentage ?? w.used_percent ?? w.utilization;
        if (used == null) return null;
        return { used_percent: Number(used), window_minutes: minutes, resets_at: sec(w.resets_at) };
      };
      const primary = mk(rl.five_hour, 300);
      const secondary = mk(rl.seven_day, 10080);
      if (primary || secondary) this._setLimits('claude', j.ts || m, { primary, secondary });
    } catch { /* 文件损坏就等下一次写入 */ }
  }

  // 套餐 5 分钟重检一次；用户改了 plans.user.json 立即生效
  _refreshPlans(now) {
    let m = 0;
    try { m = fs.statSync(this.plansUser).mtimeMs; } catch { /* 没有用户文件 */ }
    if (now - this.plansAt < 300000 && m === this.planMtime) return;
    this.plansAt = now; this.planMtime = m;
    try { this.plans = plans.detect(this.plansUser, this.logPlans); } catch { /* 检测失败沿用旧值 */ }
  }

  _checkPricing() {
    try {
      const m = fs.statSync(this.pricingUser).mtimeMs;
      if (m !== this.pricingMtime) { this.pricingMtime = m; pricing.load(this.pricingUser); }
    } catch { this.pricingMtime = 0; }
  }

  _prune(now) {
    const cut = now - RING_KEEP;
    if (this.ring.length && this.ring[0].ts < cut) this.ring = this.ring.filter((e) => e.ts >= cut);
    const hc = hourKey(now - HOUR_KEEP_DAYS * 86400e3);
    for (const k of Object.keys(this.hours)) if (k < hc) { delete this.hours[k]; this.dirty = true; }
  }

  shutdown() {
    for (const [c] of collectors.map((c) => [c])) {
      if (!c.finish) continue;
      for (const [f, st] of Object.entries(this.files)) if (st.pend) c.finish(st, (ev) => this._emit(ev));
    }
    this.save();
  }

  // ---- 汇总 ----
  _sum(store, keys, agent) {
    const total = zero();
    const models = {};
    for (const k of keys) {
      const a = store[k] && store[k][agent];
      if (!a) continue;
      for (const [m, b] of Object.entries(a)) {
        const t = (models[m] ||= zero());
        for (let i = 0; i < 7; i++) { t[i] += b[i]; total[i] += b[i]; }
      }
    }
    return { total, models };
  }

  _periodView(sum) {
    let usd = 0, est = false, unknown = false;
    const models = Object.entries(sum.models).map(([m, a]) => {
      const c = pricing.cost(m, toB(a));
      usd += c.usd; est = est || (c.known && c.est); unknown = unknown || !c.known;
      return { model: m, tokens: totalOf(a), out: a[5], n: a[0], usd: c.usd, est: c.est, known: c.known };
    }).sort((x, y) => y.tokens - x.tokens);
    const t = sum.total;
    return { n: t[0], in: t[1], cr: t[2], cw: t[3] + t[4], out: t[5], rs: t[6], tokens: totalOf(t), usd, est, unknown, models };
  }

  // 静态汇总（今日/近7天/近30天/累计/序列）：只在有新事件或跨小时时重算
  _static(now) {
    const stamp = `${this.ver}|${hourKey(now)}`;
    if (this.cacheSnap && this.cacheSnap.stamp === stamp) return this.cacheSnap.agents;
    const today = dayKey(now);
    const lastN = (n) => Array.from({ length: n }, (_, i) => dayKey(now - (n - 1 - i) * 86400e3));
    const d7 = lastN(7), d30 = lastN(30), d14 = lastN(14);
    const allDays = Object.keys(this.days);
    const lastHours = Array.from({ length: 24 }, (_, i) => hourKey(now - (23 - i) * 3600e3));
    const agents = {};
    for (const ag of AGENTS) {
      const view = (keys) => this._periodView(this._sum(this.days, keys, ag));
      const series = d14.map((k) => { const v = this._periodView(this._sum(this.days, [k], ag)); return { day: k, tokens: v.tokens, usd: v.usd }; });
      agents[ag] = { today: view([today]), d7: view(d7), d30: view(d30), all: view(allDays), series };
    }
    this.cacheSnap = { stamp, agents };
    return agents;
  }

  // 推算：近 5 小时、本周期（有额度窗口按窗口，否则按自然周）、本月
  _proj(ag, now, limits, st7) {
    const hoursBetween = (from, to) => {
      const keys = [];
      for (let t = from; t <= to + 3600e3; t += 3600e3) { const k = hourKey(t); if (keys[keys.length - 1] !== k) keys.push(k); if (hourKey(to) === k) break; }
      return this._periodView(this._sum(this.hours, keys, ag));
    };
    const slim = (v) => ({ usd: v.usd, tokens: v.tokens, est: v.est });
    const five = slim(hoursBetween(now - 4 * 3600e3, now));

    // 本周期：Codex 用日志里的每周窗口；其它 agent 按周一 00:00 起的自然周
    const sec = limits && limits.secondary;
    let wStart, wEnd, kind = 'calendar';
    if (sec && sec.resets_at * 1000 > now) { kind = 'window'; wEnd = sec.resets_at * 1000; wStart = wEnd - sec.window_minutes * 60000; }
    else {
      const d = new Date(now); d.setHours(0, 0, 0, 0); d.setDate(d.getDate() - ((d.getDay() + 6) % 7));
      wStart = d.getTime(); wEnd = wStart + 7 * 86400e3;
    }
    const wUsed = hoursBetween(wStart, now);
    const wFrac = Math.min(1, Math.max(0, (now - wStart) / (wEnd - wStart)));
    const wOk = wFrac >= 0.04; // 周期刚开始时外推不可靠
    const week = { kind, start: wStart, end: wEnd, frac: wFrac, ok: wOk, used: slim(wUsed),
      projUsd: wOk ? wUsed.usd / wFrac : null, projTokens: wOk ? wUsed.tokens / wFrac : null,
      pct: sec && kind === 'window' ? sec.used_percent : null,
      pctProj: sec && kind === 'window' && wOk ? sec.used_percent / wFrac : null };

    // 本月：自然月至今 / 已过比例
    const d0 = new Date(now); d0.setDate(1); d0.setHours(0, 0, 0, 0);
    const mStart = d0.getTime();
    const d1 = new Date(d0); d1.setMonth(d1.getMonth() + 1);
    const mEnd = d1.getTime();
    const mFrac = (now - mStart) / (mEnd - mStart);
    const mKeys = []; for (let t = mStart; t <= now; t += 86400e3) mKeys.push(dayKey(t)); if (mKeys[mKeys.length - 1] !== dayKey(now)) mKeys.push(dayKey(now));
    const mUsed = this._periodView(this._sum(this.days, mKeys, ag));
    const mOk = mFrac >= 0.03;
    const d7 = st7 || { usd: 0, tokens: 0 };
    const restDays = (mEnd - now) / 86400e3;
    const month = { frac: mFrac, ok: mOk, used: slim(mUsed), projUsd: mOk ? mUsed.usd / mFrac : null, projTokens: mOk ? mUsed.tokens / mFrac : null,
      // 另一种口径：本月至今 + 近 7 天日均 × 剩余天数（更贴近当前节奏）
      projUsd7: mUsed.usd + (d7.usd / 7) * restDays, projTokens7: mUsed.tokens + (d7.tokens / 7) * restDays,
      daysLeft: Math.ceil((mEnd - now) / 86400e3) };
    return { five, week, month };
  }

  snapshot(now = Date.now()) {
    const st = this._static(now);
    const agents = {};
    for (const ag of AGENTS) {
      const limits = this.limits[ag] || null;
      agents[ag] = { ...st[ag], live: this._live(ag, now), limits, last: this.lastEv[ag] || null,
        // 本机是否有这个 agent 的日志（或历史数据）；没有就让界面隐藏该卡片
        found: (this.lists[ag] || []).length > 0 || !!this.lastEv[ag], proj: this._proj(ag, now, limits, st[ag].d7), plan: this.plans[ag] || null };
    }
    return { now, agents, status: this.status };
  }

  _live(agent, now) {
    const evs = this.ring.filter((e) => e.agent === agent && e.ts > now - LIVE_WINDOW * 5);
    const win = evs.filter((e) => e.ts > now - LIVE_WINDOW);
    const rate = (list) => {
      let out = 0, ms = 0;
      for (const e of list) if (e.dur > 0) { out += e.out; ms += e.dur; }
      return ms > 0 ? out / (ms / 1000) : null;
    };
    const last = evs[evs.length - 1];
    const byModel = {};
    for (const e of evs) (byModel[e.model] ||= []).push(e);
    const models = Object.entries(byModel).map(([m, list]) => {
      const lw = list.filter((e) => e.ts > now - LIVE_WINDOW);
      const l = list[list.length - 1];
      return { model: m, tps: rate(lw), lastTps: l.dur > 0 ? l.out / (l.dur / 1000) : null, lastTs: l.ts, active: now - l.ts < LIVE_HOLD, tokens: list.reduce((s, e) => s + e.tot, 0) };
    }).sort((a, b) => b.lastTs - a.lastTs);
    return {
      active: !!last && now - last.ts < LIVE_HOLD,
      tps: rate(win),                                   // 近 60 秒平均输出速度 tok/s
      lastTps: last && last.dur > 0 ? last.out / (last.dur / 1000) : null,
      perMin: win.reduce((s, e) => s + e.tot, 0),       // 近 60 秒总吞吐 token（含缓存）
      outMin: win.reduce((s, e) => s + e.out, 0),       // 近 60 秒输出 token
      reqMin: win.length,                               // 近 60 秒请求数
      lastTs: last ? last.ts : 0,
      model: last ? last.model : null,
      models,
    };
  }
}

module.exports = { Engine, AGENTS, dayKey };
