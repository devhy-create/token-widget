// 渲染端：被动接收主进程每秒推送的快照，只在内容变化时才改 DOM，尽量不占 CPU。
// 默认只显示精简行（实时速度 + 5h / 周占比），点击某个 agent 才展开详情（手风琴，一次只开一个）。
const AGENTS = [
  { id: 'claude', name: 'Claude' },
  { id: 'codex', name: 'Codex', dark: true },
  { id: 'antigravity', name: 'Antigravity' },
  { id: 'workbuddy', name: 'WorkBuddy' },
  { id: 'zcode', name: 'ZCode' },
  { id: 'opencode', name: 'OpenCode' },
];

const esc = (s) => String(s).replace(/[&<>"]/g, (c) => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;' }[c]));
const $ = (sel, root = document) => root.querySelector(sel);

// ---------- 格式化 ----------
const fmtTok = (n) => (n >= 1e9 ? (n / 1e9).toFixed(2) + 'B' : n >= 1e6 ? (n / 1e6).toFixed(n >= 1e8 ? 0 : 1) + 'M' : n >= 1e3 ? (n / 1e3).toFixed(n >= 1e5 ? 0 : 1) + 'K' : String(Math.round(n)));
const usdFull = (n) => '$' + n.toLocaleString('en-US', { minimumFractionDigits: 2, maximumFractionDigits: 2 });
// 紧凑金额：≥1000 不显示小数；估价加 ≈
const usd = (n, est) => (est && n > 0 ? '≈' : '') + (n >= 1000 ? '$' + Math.round(n).toLocaleString('en-US') : usdFull(n));
const tps = (v) => (v == null ? '—' : v >= 100 ? v.toFixed(0) : v.toFixed(1));
function fmtDur(ms) {
  if (ms <= 0) return '0m';
  const m = Math.floor(ms / 60000), h = Math.floor(m / 60), d = Math.floor(h / 24);
  if (d > 0) return `${d}d ${h % 24}h`;
  if (h > 0) return `${h}h ${m % 60}m`;
  return `${Math.max(1, m)}m`;
}
function ago(ms) {
  const s = Math.floor(ms / 1000);
  if (s < 60) return `${s} 秒前`;
  if (s < 3600) return `${Math.floor(s / 60)} 分钟前`;
  if (s < 86400) return `${Math.floor(s / 3600)} 小时前`;
  return `${Math.floor(s / 86400)} 天前`;
}
// 请求已进行多久（“生成中”时显示）
function elapsed(ms) {
  const s = Math.max(0, Math.floor(ms / 1000));
  return s < 60 ? `${s} 秒` : `${Math.floor(s / 60)} 分 ${s % 60} 秒`;
}
// 请求已发出但还没有任何速度数据（WorkBuddy 要等请求结束才写用量）：只能显示“生成中”
const noSpeed = (m) => m.busy != null && (m.tps ?? m.lastTps) == null;
const shortModel = (m) => String(m).replace(/^claude-/, '').replace(/-\d{8}$/, '');
const mult = (v, price) => (price > 0 ? (v / price >= 10 ? (v / price).toFixed(0) : (v / price).toFixed(1)) + '×' : '');
const dayLabel = (day) => { const [y, m, d] = day.split('-').map(Number); return new Date(y, m - 1, d).toLocaleDateString('zh-CN', { month: 'long', day: 'numeric', weekday: 'short' }); };

// ---------- 只在值变化时写入 ----------
function setText(el, v) { if (el._v !== v) { el._v = v; el.textContent = v; } }
function setHtml(el, v) { if (el._h !== v) { el._h = v; el.innerHTML = v; } }
function setCls(el, name, on) { if (el.classList.contains(name) !== on) el.classList.toggle(name, on); }

const refs = {};
let icons = {};      // { agentId: dataURL | null }，由主进程从本机应用提取；没有就用首字母徽标
let hidden = {};     // 用户在托盘菜单里手动隐藏的 agent
let mode = 'expanded';
let dock = null;      // 收缩时贴靠的边（主进程告知）
let lastPillH = 0;
let openId = null;
let lastSnap = null;

// ---------- 额度窗口：已用%、剩余时间、节奏 ----------
function win(w, now) {
  if (!w) return null;
  const winMs = w.window_minutes * 60000;
  const resetMs = w.resets_at * 1000;
  const expired = resetMs <= now; // 窗口已过期：日志里的百分比是旧的，按 0 算
  const used = expired ? 0 : Math.min(100, Math.max(0, w.used_percent));
  const left = Math.max(0, resetMs - now);
  const frac = expired ? 0 : Math.min(1, Math.max(0, 1 - left / winMs));
  const label = w.window_minutes === 300 ? '5h' : w.window_minutes === 10080 ? '周' : `${Math.round(w.window_minutes / 60)}h`;
  return { label, used, left, frac, expired, winMs };
}
const lvl = (p) => (p >= 90 ? 'hot' : p >= 70 ? 'mid' : '');
// 圆环：半径 r，圆心 (c, c)；周长 2πr，已用部分用 dashoffset 表示
const ring = (p, r, c) => { const len = 2 * Math.PI * r; return `<circle class="t" cx="${c}" cy="${c}" r="${r}"/><circle class="p" cx="${c}" cy="${c}" r="${r}" stroke-dasharray="${len.toFixed(1)}" stroke-dashoffset="${(len * (1 - p / 100)).toFixed(1)}"/>`; };
const miniRing = (w) => `<span class="rg ${lvl(w.used)}"><svg viewBox="0 0 18 18">${ring(w.used, 6, 9)}</svg><span>${w.label} <b>${Math.round(w.used)}%</b></span></span>`;
const bigRing = (w) => `<div class="big ${lvl(w.used)}"><svg viewBox="0 0 48 48">${ring(w.used, 18, 24)}</svg><div><div class="n">${Math.round(w.used)}%</div><div class="l">${w.label === '5h' ? '5 小时' : w.label === '周' ? '每周' : w.label}</div><div class="l">${w.expired ? '已重置' : fmtDur(w.left).replace(' ', '') + ' 后重置'}</div></div></div>`;

// ---------- 图标 ----------
// 有图标用 <img>，没有就显示名称首字母（保证开源仓库不带官方图标也能正常显示）
const iconHtml = (a, cls = '') => (icons[a.id] ? `<img class="${cls}" src="${icons[a.id]}" alt="">` : `<span class="ltr ${cls}">${esc(a.name[0])}</span>`);
function paintIcons() {
  for (const a of AGENTS) {
    const r = refs[a.id];
    if (!r) continue;
    setHtml(r.badge, iconHtml(a));
    const dark = !!(a.dark && icons[a.id]); // Codex 图标是白底透明，需要深色底
    setHtml(r.picSlot, iconHtml(a, 'ic' + (dark ? ' ic-dark' : '')));
    setCls(r.badge, 'dark', dark);
  }
}

// ---------- 构建 ----------
function build() {
  const box = $('#agents');
  for (const a of AGENTS) {
    const pi = document.createElement('span');
    pi.className = 'pi';
    pi.dataset.pi = a.id;
    pi.innerHTML = '<i class="d"></i><span class="pic"></span><em>—</em>';
    pi.hidden = true; // 先藏起来，收到第一份快照、确认本机有这个 agent 才显示
    $('#pill').insertBefore(pi, $('#pill-total'));
    const el = document.createElement('section');
    el.className = 'ag';
    el.hidden = true;
    el.dataset.ag = a.id;
    el.innerHTML = `
      <div class="row">
        <span class="badge"></span>
        <span class="nm">${a.name}<span class="plan" hidden></span></span>
        <span class="right"></span>
        <div class="sub"></div>
      </div>
      <div class="detail"><div class="inner"><div class="dp">
        <div class="tiles"></div>
        <div class="rings"></div>
        <div class="speeds"></div>
        <div class="speeds quotas"></div>
        <div class="chartbox">
          <div class="cap"><span></span><span></span></div>
          <div class="chart"></div>
        </div>
        <div class="stats"></div>
        <div class="mlist"></div>
      </div></div></div>`;
    box.appendChild(el);
    const r = refs[a.id] = {
      el, pi, badge: $('.badge', el), picSlot: $('.pic', pi), plan: $('.plan', el), right: $('.right', el), sub: $('.sub', el),
      tiles: $('.tiles', el), speeds: $('.speeds', el), quotas: $('.quotas', el), rings: $('.rings', el), chart: $('.chart', el), capL: $('.cap span:first-child', el), capR: $('.cap span:last-child', el),
      stats: $('.stats', el), mlist: $('.mlist', el), inner: $('.inner', el), detail: $('.detail', el), hover: null, series: [],
    };
    $('.row', el).addEventListener('click', () => {
      openId = openId === a.id ? null : a.id;
      for (const x of AGENTS) setCls(refs[x.id].el, 'open', x.id === openId);
      if (lastSnap) render(lastSnap);
      reportSize();
    });
    // 柱状图悬停：显示该天的日期、费用、token（自绘提示，不用浏览器 title，避免延迟和不显示）
    r.chart.addEventListener('mousemove', (e) => {
      const bars = r.chart.children;
      if (!bars.length) return;
      const rect = r.chart.getBoundingClientRect();
      const i = Math.max(0, Math.min(bars.length - 1, Math.floor(((e.clientX - rect.left) / rect.width) * bars.length)));
      if (r.hover !== i) { r.hover = i; paintCap(r); }
    });
    r.chart.addEventListener('mouseleave', () => { r.hover = null; paintCap(r); });
  }
}

function paintCap(r) {
  const s = r.series;
  if (!s.length) return;
  const i = r.hover == null ? s.length - 1 : r.hover;
  const x = s[i];
  setText(r.capL, (r.hover == null ? '今天 · ' : '') + dayLabel(x.day));
  setText(r.capR, `${usdFull(x.usd)} · ${fmtTok(x.tokens)}`);
  [...r.chart.children].forEach((b, k) => setCls(b, 'hov', r.hover === k));
}

// ---------- 详情：卡片化，少文字 ----------
function tilesHtml(d) {
  const p = d.proj;
  const plan = d.plan;
  const price = plan && plan.usd != null ? plan.usd : null;
  const tile = (k, v, s, cls = '') => `<div class="tile"><div class="k">${k}</div><div class="v ${cls}">${v}</div><div class="s">${s}</div></div>`;
  const wkTitle = p.week.kind === 'window' ? '每周' : '本周';
  const pctp = p.week.pctProj != null ? ` · <span class="${p.week.pctProj >= 100 ? 'hot' : ''}">${Math.round(p.week.pctProj)}%</span>` : '';
  const t1 = tile(wkTitle, usd(p.week.used.usd, p.week.used.est), p.week.ok ? `预计 ${usd(p.week.projUsd, p.week.used.est)}${pctp}` : '刚开始');
  // 第二格：有额度窗口的 Agent 显示折算月度额度（订阅周期与自然月不对应，按周额度百分比反推）；
  // 没有额度窗口的退回自然月的预计用量
  let t2;
  if (p.week.kind === 'window') {
    const pct = p.week.pct != null ? `${Math.round(p.week.pct)}%` : '';
    t2 = p.week.quotaMonthUsd != null
      ? tile('月度额度', usd(p.week.quotaMonthUsd, p.week.used.est), `周 ${usd(p.week.quotaWeekUsd, p.week.used.est)} · 已用 ${pct}`)
      : tile('月度额度', '—', pct ? `已用 ${pct}，用量太少暂不估算` : '暂无额度数据');
  } else {
    t2 = p.month.ok
      ? tile('本月预计', usd(p.month.projUsd, p.month.used.est), `已用 ${usd(p.month.used.usd, p.month.used.est)}`)
      : tile('本月', usd(p.month.used.usd, p.month.used.est), '月初');
  }
  // 订阅回本按近 30 天滚动用量算（订阅周期不是自然月），只占一行小字，不再单独占一格；
  // 此时上面两格各占一半宽
  const d30 = d.d30;
  if (price > 0) {
    const good = d30.usd >= price ? ' good' : '';
    const note = `<div class="tnote">订阅回本 <b class="${good}">${mult(d30.usd, price)}</b> · 近 30 天 ${usd(d30.usd, d30.est)} / 月费 $${price}</div>`;
    return `<div class="tiles2">${t1 + t2}</div>${note}`;
  }
  const t3 = price === 0 ? tile('套餐', 'Free', '无订阅费') : tile('近 5 小时', usd(p.five.usd, p.five.est), fmtTok(p.five.tokens));
  return t1 + t2 + t3;
}

// 没有额度数据的 Agent：第二行改显示套餐月费 + 近 30 天折算（订阅回本倍数）
function planLine(d) {
  const pl = d.plan;
  const used = d.d30;
  const month = `近 30 天折算 <b>${usd(used.usd, used.est)}</b>`;
  if (!pl) return `<span class="kd">${month}</span>`;
  if (pl.usd > 0) return `<span class="kd">${esc(pl.name)} $${pl.usd}/月 · ${month} <b>(${mult(used.usd, pl.usd)})</b></span>`;
  return `<span class="kd">${esc(pl.name)} · ${month}</span>`;
}

// ---------- 渲染 ----------
// 是否显示：本机检测到 + 用户没手动隐藏
const shown = (s, id) => !!(s.agents[id] && s.agents[id].found) && !hidden[id];

// 竖向胶囊（贴左/右边）的卡片高度 = 上下内边距 + 每个 agent 一格 + 合计一格；与 style.css 里的 .dock-v 尺寸对应
function reportPill(s) {
  if (!document.body.classList.contains('dock-v')) return;
  const n = AGENTS.filter((a) => shown(s, a.id)).length;
  const h = 34 + 44 * n;
  if (h !== lastPillH) { lastPillH = h; window.widget.pillSize(h); }
}

function renderPill(s) {
  let tot = 0;
  for (const a of AGENTS) {
    const d = s.agents[a.id];
    const el = $(`[data-pi="${a.id}"]`);
    el.hidden = !shown(s, a.id);
    if (el.hidden) continue;
    const on = d.live.active;
    setCls(el, 'on', on);
    let txt = '—';
    if (on) txt = noSpeed(d.live) ? '…' : tps(d.live.tps ?? d.live.lastTps);
    else if (d.limits && d.limits.primary) { const w = win(d.limits.primary, s.now); txt = Math.round(w.used) + '%'; }
    setText($('em', el), txt);
    tot += d.today.usd;
  }
  // 竖向胶囊很窄：金额不带小数，≥ $1000 用 k 作单位（四舍五入），保证 4 个字符内放得下
  const vert = document.body.classList.contains('dock-v');
  const totTxt = !vert ? usd(tot) : tot >= 1000 ? '$' + Math.round(tot / 1000) + 'k' : '$' + Math.round(tot);
  setHtml($('#pill-total'), `<span class="lbl">今日 </span>${totTxt}`);
  reportPill(s);
}

function render(s) {
  lastSnap = s;
  if (mode === 'collapsed') { renderPill(s); return; }
  const now = s.now;
  let tU = 0, tT = 0, estT = false;

  for (const a of AGENTS) {
    const d = s.agents[a.id];
    const r = refs[a.id];
    r.el.hidden = !shown(s, a.id);
    if (r.el.hidden) {
      // 被隐藏的卡片如果正开着，要收起，否则窗口高度会算错
      if (openId === a.id) { openId = null; setCls(r.el, 'open', false); }
      continue;
    }
    const lv = d.live;

    // 套餐标签：识别不到就不显示
    const pl = d.plan;
    if (pl) { setText(r.plan, pl.name); r.plan.hidden = false; } else r.plan.hidden = true;

    // 右侧：速度按模型分开显示。只有 1 个模型在生成 → 大号数字；≥2 个 → 每个模型一行；都没在生成 → 空闲
    const act = lv.models.filter((m) => m.active);
    const mtps = (m) => (noSpeed(m) ? '…' : tps(m.tps ?? m.lastTps));
    let rh;
    if (act.length >= 2) rh = act.slice(0, 3).map((m) => `<div class="mv"><span title="${esc(m.model)}">${esc(shortModel(m.model))}</span><b>${mtps(m)}</b></div>`).join('') + '<div class="b">tok/s</div>';
    else if (act.length === 1 && noSpeed(act[0])) rh = `<div class="a">生成中</div><div class="b">已 ${elapsed(now - act[0].busy)}</div>`;
    else if (act.length === 1) rh = `<div class="a">${mtps(act[0])}</div><div class="b">tok/s</div>`;
    else rh = `<div class="a">${d.last ? '空闲' : '—'}</div><div class="b">${d.last ? ago(now - d.last.ts) : '无数据'}</div>`;
    setHtml(r.right, rh);
    setCls(r.right, 'on', act.length > 0);
    setCls(r.right, 'idle', act.length === 0);

    // 第二行：5h / 周额度已用；没有额度数据就明说“暂无数据”（Claude 需配置钩子，Antigravity / WorkBuddy 本地没有）
    // 第三行：今日花费 + token；只有 0~1 个模型在生成时，把模型名也放这里（≥2 个时右侧已按模型分行）
    const model = lv.model || (d.last && d.last.model);
    const w5 = d.limits && win(d.limits.primary, now);
    const wk = d.limits && win(d.limits.secondary, now);
    const l2 = w5 || wk ? (w5 ? miniRing(w5) : '') + (wk ? miniRing(wk) : '') : planLine(d);
    const l3 = `<span>今日 <b>${usd(d.today.usd, d.today.est)}</b> · ${fmtTok(d.today.tokens)}</span>` +
      (model && act.length < 2 ? `<span class="mdl" title="${esc(model)}">${esc(shortModel(model))}</span>` : '');
    setHtml(r.sub, `<div class="l2">${l2}</div><div class="l3">${l3}</div>`);

    if (openId === a.id) {
      setHtml(r.tiles, tilesHtml(d));
      // 额度数据不是实时的（Claude 靠命令行版钩子刷新，Codex 靠日志）：超过 10 分钟没更新就标出来
      const age = d.limits && d.limits.ts ? now - d.limits.ts : 0;
      const stale = age > 600000 ? `<div class="rstale">额度数据更新于 ${ago(age)}</div>` : '';
      setHtml(r.rings, (w5 ? bigRing(w5) : '') + (wk ? bigRing(wk) : '') + stale);
      r.rings.style.display = w5 || wk ? '' : 'none';

      // 分模型速度：近 5 分钟内出现过的模型，生成中的用实时速度，其余显示最近一次速度
      const sp = lv.models.slice(0, 5);
      r.speeds.style.display = sp.length ? '' : 'none';
      setHtml(r.speeds, sp.map((m) => `<div class="sr${m.active ? ' on' : ''}"><i></i><span class="n">${esc(shortModel(m.model))}</span><span class="v">${m.active ? (noSpeed(m) ? `生成中 · 已 ${elapsed(now - m.busy)}` : mtps(m) + ' tok/s') : (m.lastTps != null ? Math.round(m.lastTps) + ' tok/s · ' : '') + ago(now - m.lastTs)}</span></div>`).join(''));

      // 分模型额度（目前只有 Antigravity 有）：已用百分比 + 重置倒计时
      const qm = (d.limits && d.limits.models) || [];
      r.quotas.style.display = qm.length ? '' : 'none';
      const qrow = (q, label) => {
        const left = q.resetsAt * 1000 - now;
        const used = left <= 0 ? 0 : Math.round((1 - q.remaining) * 100);
        return { key: used + '|' + q.resetsAt, html: `<div class="sr"><i></i><span class="n">${esc(label)}</span><span class="v">已用 ${used}%${left > 0 ? ' · ' + fmtDur(left).replace(' ', '') + ' 后重置' : ''}</span></div>` };
      };
      // 所有模型用量和重置时间都一样时合并成一行，避免刷屏
      const same = qm.length > 1 && qm.every((q) => qrow(q, '').key === qrow(qm[0], '').key);
      setHtml(r.quotas, same ? qrow(qm[0], `全部模型（${qm.length}）`).html : qm.slice(0, 8).map((q) => qrow(q, q.label).html).join(''));

      const byUsd = d.series.some((x) => x.usd > 0);
      const vals = d.series.map((x) => (byUsd ? x.usd : x.tokens));
      const max = Math.max(...vals, 1e-9);
      r.series = d.series;
      setHtml(r.chart, d.series.map((x, i) => `<i class="${i === d.series.length - 1 ? 'last' : ''}" style="height:${Math.max(6, (vals[i] / max) * 100)}%"></i>`).join(''));
      paintCap(r);

      const st = (k, v) => `<div><div class="k">${k}</div><div class="v">${usd(v.usd, v.est)}</div><div class="s">${fmtTok(v.tokens)}</div></div>`;
      setHtml(r.stats, st('今日', d.today) + st('7 天', d.d7) + st('30 天', d.d30) + st('累计', d.all));
      const ms = d.today.models.slice(0, 3);
      setHtml(r.mlist, ms.map((m) => `<div class="mr"><span>今日 ${esc(shortModel(m.model))}</span><span>${m.known ? usd(m.usd, m.est) : '未定价'} · ${fmtTok(m.tokens)}</span></div>`).join(''));
    }

    tU += d.today.usd; tT += d.today.tokens; estT = estT || d.today.est;
  }

  // 第一轮文件发现还没完成时先不提示“没检测到”
  $('#empty').hidden = s.booted === false || AGENTS.some((a) => shown(s, a.id));
  // 底部一行：今日合计（金额是 API 公开价折算值，不是实际扣款）
  setHtml($('#foot'), `<span>今日合计 <b>${usd(tU, estT)}</b> · ${fmtTok(tT)} token</span><span title="金额按 API 公开价折算（≈ 为估价），不是实际扣款">折算值</span>`);

  const sc = $('#scan');
  sc.hidden = !s.status.scanning;
  if (s.status.scanning) setText(sc, `首次扫描 ${s.status.done}/${s.status.total}`);
  reportSize();
}

// ---------- 设置页 ----------
// 打开期间主进程锁定自动收缩；每次改动立即写回（plans.user.json / widget.json），不需要“保存”按钮。
let setOpen = false;
let setData = null;
// 预设：默认价格表里的套餐；没有 Free 的 agent（如 WorkBuddy）补一个 Free
const planPresets = (a) => (a.presets.some((p) => p.key === 'free') ? a.presets : [{ key: 'free', name: 'Free', usd: 0 }, ...a.presets]);
// 当前选中项：没有用户覆盖 → 自动；覆盖值与某个预设一致 → 该预设；否则自定义
const planValue = (a) => {
  if (!a.user) return 'auto';
  const p = planPresets(a).find((x) => x.name === a.user.name && x.usd === a.user.usd);
  return p ? 'p:' + p.key : 'custom';
};
const sw = (attr, on) => `<label class="sw"><input type="checkbox" ${attr} ${on ? 'checked' : ''}><i></i></label>`;

// Claude 额度钩子区块：根据状态给出说明和按钮
function hookHtml(h) {
  const txt = {
    none: '本地日志里没有 Claude 的 5h / 周额度，需要给 Claude Code 配一个 statusLine 钩子。启用前会自动备份 settings.json。注意：只有命令行版 Claude Code 会触发钩子，桌面 App 不会。',
    ours: '已启用。命令行版 Claude Code 每次刷新状态栏就会更新额度；桌面 App 不会触发，只用桌面版时数字会停在上次。',
    other: '检测到你已有自己的 statusLine，为避免覆盖不会自动修改。可在其脚本里把 Claude Code 传入的 JSON 转给 agent-monitor.exe --statusline 合并。',
    unreadable: '~/.claude/settings.json 读取失败，为安全起见不修改。',
  }[h.state];
  let btn = '';
  if (h.state === 'none') btn = '<button class="sbtn" data-hook="1">启用</button>';
  else if (h.state === 'ours') btn = '<button class="sbtn" data-hook="0">停用</button>';
  return `<div class="sh">Claude 额度（5h / 周）</div><div class="sg"><div class="srow hookrow"><span class="snote">${txt}</span>${btn}</div></div>`;
}

function renderSettings(d) {
  setData = d;
  const rows = d.agents.filter((a) => a.found !== false).map((a) => {
    const v = planValue(a);
    const opts = [`<option value="auto">自动识别${a.detected ? '（' + esc(a.detected.name) + '）' : '（未识别）'}</option>`]
      .concat(planPresets(a).map((p) => `<option value="p:${p.key}">${esc(p.name)} · $${p.usd}${p.est ? '（估）' : ''}</option>`))
      .concat('<option value="custom">自定义…</option>');
    const cu = a.user || { name: '', usd: '' };
    return `<div class="sa" data-id="${a.id}">
      <div class="sline"><span class="sn">${esc(a.name)}</span>
        <select data-plan="${a.id}">${opts.join('').replace(`value="${v}"`, `value="${v}" selected`)}</select>
        ${sw(`data-show="${a.id}"`, !d.hidden[a.id])}</div>
      <div class="scustom" ${v === 'custom' ? '' : 'hidden'}>
        <input type="text" data-cn="${a.id}" placeholder="套餐名" maxlength="24" value="${esc(cu.name)}">
        <input type="number" data-cu="${a.id}" placeholder="月费 $" min="0" step="0.01" value="${cu.usd}">
      </div></div>`;
  }).join('');
  const row = (label, k, on) => `<div class="srow"><span class="sn">${label}</span>${sw(`data-k="${k}"`, on)}</div>`;
  $('#settings').innerHTML = `<div class="sh">Agent · 套餐与显示</div><div class="sg">${rows}</div>
    <div class="sh">通用</div><div class="sg"><div class="srow"><span class="sn">外观</span><div class="seg">${[['system', '跟随系统'], ['light', '浅色'], ['dark', '深色']].map(([v, t]) => `<button type="button" data-theme="${v}" class="${d.cfg.theme === v ? 'on' : ''}">${t}</button>`).join('')}</div></div>${d.cfg.canAutostart ? row('开机自启', 'openAtLogin', d.cfg.openAtLogin) : `<div class="srow" style="opacity:.45" title="开发模式下不可用，请使用安装版或便携版"><span class="sn">开机自启 · 仅安装版/便携版可用</span>${sw('disabled', false)}</div>`}${row('鼠标移开自动收缩到顶部', 'autoHide', d.cfg.autoHide)}${row('始终置顶', 'onTop', d.cfg.onTop)}${d.cfg.canBlur ? row('毛玻璃（真模糊）', 'blur', d.cfg.blur) : ''}${row('读取 Antigravity 额度', 'agQuota', d.cfg.agQuota)}<div class="snote">开启后会读取 Antigravity 本地服务的接口令牌，仅在内存中使用，只向本机 127.0.0.1 发请求，不保存也不外传。</div></div>
    ${hookHtml(d.claudeHook)}
    <button class="sdone" id="set-done">完成</button>`;
  $('#set-done').addEventListener('click', closeSettings);
}

async function openSettings() {
  try { renderSettings(await window.widget.getSettings()); } catch { return; }
  setOpen = true;
  window.widget.lock(true);
  setCls(document.body, 'is-set', true);
  setCls($('#btn-set'), 'on', true);
  setText($('#brand-t'), '设置');
  $('#settings').hidden = false;
  reportSize();
}
function closeSettings() {
  setOpen = false;
  window.widget.lock(false);
  setCls(document.body, 'is-set', false);
  setCls($('#btn-set'), 'on', false);
  setText($('#brand-t'), 'Agent 监控');
  $('#settings').hidden = true;
  reportSize();
}
const send = (patch) => window.widget.setSettings(patch).then((d) => { setData = d; }).catch(() => {});

// 自定义套餐：名称或月费改动时写回
function saveCustom(id) {
  const box = $(`.sa[data-id="${id}"]`);
  send({ plans: { [id]: { name: $('[data-cn]', box).value.trim() || '自定义', usd: Number($('[data-cu]', box).value) || 0 } } });
}
$('#settings').addEventListener('click', async (e) => {
  const tb = e.target.closest('[data-theme]');
  if (tb) {
    for (const x of $('#settings').querySelectorAll('[data-theme]')) setCls(x, 'on', x === tb);
    send({ cfg: { theme: tb.dataset.theme } });
    return;
  }
  const b = e.target.closest('[data-hook]');
  if (!b) return;
  b.disabled = true;
  try { renderSettings(await window.widget.setClaudeHook(b.dataset.hook === '1')); } catch { /* 失败时保持当前界面 */ }
  reportSize();
});
$('#settings').addEventListener('change', (e) => {
  const t = e.target;
  if (t.dataset.k) send({ cfg: { [t.dataset.k]: t.checked } });
  else if (t.dataset.show) send({ hidden: { [t.dataset.show]: !t.checked } });
  else if (t.dataset.plan) {
    const id = t.dataset.plan;
    const a = setData.agents.find((x) => x.id === id);
    $('.scustom', t.closest('.sa')).hidden = t.value !== 'custom';
    if (t.value === 'auto') send({ plans: { [id]: null } });
    else if (t.value === 'custom') saveCustom(id);
    else { const p = planPresets(a).find((x) => 'p:' + x.key === t.value); send({ plans: { [id]: { name: p.name, usd: p.usd } } }); }
    reportSize();
  } else if (t.dataset.cn || t.dataset.cu) saveCustom(t.dataset.cn || t.dataset.cu);
});
document.addEventListener('keydown', (e) => { if (e.key === 'Escape' && setOpen) closeSettings(); });

// ---------- 窗口高度跟随内容 ----------
// 手风琴展开时 CSS 高度在动画中，所以按“动画结束后”的最终高度上报：当前高度 + (各详情完整高度 - 当前高度)
let sizeT = 0;
let lastH = 0;
function targetHeight() {
  let h = $('#content').offsetHeight;
  for (const a of AGENTS) {
    const r = refs[a.id];
    const cur = r.detail.offsetHeight;
    const fin = r.el.classList.contains('open') ? r.inner.scrollHeight : 0;
    h += fin - cur;
  }
  return Math.ceil(h);
}
function reportSize() {
  clearTimeout(sizeT);
  sizeT = setTimeout(() => {
    if (mode !== 'expanded') return;
    const h = targetHeight();
    if (Math.abs(h - lastH) > 1) { lastH = h; window.widget.resize(h); }
  }, 30);
}

// ---------- 模式切换（主进程控制窗口大小与缓动，这里只切显示） ----------
window.widget.onMode((m) => {
  mode = m.mode;
  dock = m.dock || null;
  setCls(document.body, 'dock-v', mode === 'collapsed' && (dock === 'left' || dock === 'right'));
  lastPillH = 0;
  setCls(document.body, 'is-pill', mode === 'collapsed');
  setCls($('#btn-pin'), 'on', !!m.pinned);
  hidden = m.hidden || {};
  if (lastSnap) render(lastSnap);
  if (mode === 'expanded') {
    lastH = 0; reportSize();
    // 从很窄的竖向胶囊展开时，开头窗口还没长宽，内容会被挤得换行、量出偏高的高度；动画结束后再量一次纠正
    setTimeout(() => { if (mode === 'expanded') { lastH = 0; reportSize(); } }, 360);
  }
});

build();
paintIcons();
window.widget.icons().then((m) => { icons = m || {}; paintIcons(); reportSize(); }).catch(() => {});
window.widget.onSnap(render);
$('#btn-pin').addEventListener('click', () => window.widget.pin());
$('#btn-set').addEventListener('click', () => (setOpen ? closeSettings() : openSettings()));
$('#btn-menu').addEventListener('click', () => window.widget.menu());
$('#btn-close').addEventListener('click', () => window.widget.close());
document.addEventListener('contextmenu', (e) => { e.preventDefault(); window.widget.menu(); });
document.addEventListener('mouseenter', () => window.widget.hover(true));
document.addEventListener('mouseleave', () => window.widget.hover(false));
document.addEventListener('mousemove', () => { if (mode === 'collapsed') window.widget.hover(true); }, { passive: true });
