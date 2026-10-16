// 四个 agent 的日志采集器。每个采集器负责：发现文件、增量读取、归一化为统一事件。
// 统一事件：{agent, ts, model, sid, key, in, cr, cw5, cw1, out, rs, dur}
//   in  = 未命中缓存的输入 token；cr = 缓存读；cw5/cw1 = 5分钟/1小时缓存写
//   out = 输出 token（含思考）；rs = 其中的思考 token（仅展示）；dur = 本次生成耗时(ms)，未知为 0
const fs = require('fs');
const path = require('path');
const os = require('os');
const { readLines } = require('./lines');

const HOME = os.homedir();
const MIN_DUR = 300; // 小于 0.3 秒的耗时视为不可信
const MAX_DUR = 15 * 60 * 1000;

function clampDur(ms) {
  return ms >= MIN_DUR && ms <= MAX_DUR ? ms : 0;
}

// 递归列出 root 下满足 test 的文件（容错：目录不存在返回空）
function walk(root, test, out = [], depth = 0) {
  let ents;
  try { ents = fs.readdirSync(root, { withFileTypes: true }); } catch { return out; }
  for (const e of ents) {
    const p = path.join(root, e.name);
    if (e.isDirectory()) { if (depth < 6) walk(p, test, out, depth + 1); }
    else if (test(e.name, p)) out.push(p);
  }
  return out;
}

const num = (v) => (typeof v === 'number' && Number.isFinite(v) ? v : 0);

// ---------------------------------------------------------------- Claude
const claude = {
  name: 'claude',
  discover() {
    return walk(path.join(HOME, '.claude', 'projects'), (n) => n.endsWith('.jsonl'));
  },
  // st: 文件状态 {off, prevTs, pend}；emit(ev)
  scan(file, st, emit, now) {
    const flush = () => {
      const p = st.pend;
      if (!p) return;
      st.pend = null;
      emit({ agent: 'claude', ts: p.ts, model: p.model, sid: p.sid, key: p.id,
        in: p.in, cr: p.cr, cw5: p.cw5, cw1: p.cw1, out: p.out, rs: p.rs,
        dur: clampDur(p.ts - p.prevTs) });
    };
    const r = readLines(file, st.off || 0, ['"usage"'], (text, ts) => {
      let o;
      try { o = JSON.parse(text); } catch { st.prevTs = ts || st.prevTs; return; }
      const m = o.message;
      if (o.type !== 'assistant' || !m || !m.usage || !m.model || m.model === '<synthetic>') { st.prevTs = ts || st.prevTs; return; }
      const u = m.usage;
      const cc = u.cache_creation || {};
      const has = cc.ephemeral_5m_input_tokens != null || cc.ephemeral_1h_input_tokens != null;
      const total = num(u.cache_creation_input_tokens);
      const w1 = has ? num(cc.ephemeral_1h_input_tokens) : 0;
      const w5 = has ? num(cc.ephemeral_5m_input_tokens) : total; // 没有拆分时按 5 分钟算
      const id = m.id || o.requestId || o.uuid;
      const cur = { in: num(u.input_tokens), cr: num(u.cache_read_input_tokens), cw5: w5, cw1: w1,
        out: 0, rs: num(u.output_tokens_details && u.output_tokens_details.thinking_tokens) };
      // 实测（对照 Claude 自带的 cost-state 汇总）：日志里的 output_tokens 不含思考 token，
      // 计费输出 ≈ output_tokens + thinking_tokens，所以这里把两者相加
      cur.out = num(u.output_tokens) + cur.rs;
      const t = ts || now;
      if (st.pend && st.pend.id === id) {
        // 同一条消息被拆成多行写入（thinking / text / tool_use 各一行）：取各字段最大值，时间取最后一行
        const p = st.pend;
        for (const k of Object.keys(cur)) p[k] = Math.max(p[k], cur[k]);
        p.ts = t; p.wall = now;
      } else {
        flush();
        st.pend = { id, model: m.model, sid: o.sessionId || path.basename(file, '.jsonl'), ts: t,
          prevTs: st.prevTs || 0, wall: now, ...cur };
      }
      st.prevTs = t;
    }, (ts) => { if (ts) st.prevTs = ts; });
    st.off = r.offset;
    // 最后一条消息可能还在被追加：4 秒没有新行再入账
    if (st.pend && now - st.pend.wall > 4000) flush();
    return r;
  },
  finish(st, emit) { if (st.pend) { const p = st.pend; st.pend = null; emit({ agent: 'claude', ts: p.ts, model: p.model, sid: p.sid, key: p.id, in: p.in, cr: p.cr, cw5: p.cw5, cw1: p.cw1, out: p.out, rs: p.rs, dur: clampDur(p.ts - p.prevTs) }); } },
};

// ---------------------------------------------------------------- Codex
const codex = {
  name: 'codex',
  discover() {
    const t = (n) => n.endsWith('.jsonl');
    return [
      ...walk(path.join(HOME, '.codex', 'sessions'), t),
      ...walk(path.join(HOME, '.codex', 'archived_sessions'), t),
    ];
  },
  scan(file, st, emit, now, setLimits) {
    const needles = ['"token_usage_record"', '"turn_context"', '"token_count"'];
    // 一次模型请求的起点：用户输入、工具输出、回合开始
    const marks = ['"function_call_output"', '"custom_tool_call_output"', '"task_started"', '"role":"user"'];
    const r = readLines(file, st.off || 0, needles, (text, ts) => {
      let o;
      try { o = JSON.parse(text); } catch { return; }
      const p = o.payload || {};
      const t = ts || Date.parse(o.timestamp) || now;
      if (o.type === 'turn_context') { if (p.model) st.model = p.model; return; }
      if (o.type === 'event_msg' && p.type === 'token_count') {
        const rl = p.rate_limits;
        if (rl && setLimits) setLimits('codex', t, rl);
        return;
      }
      if (o.type !== 'token_usage_record' || !p.usage) return;
      const u = p.usage;
      const cached = num(u.cached_input_tokens);
      emit({ agent: 'codex', ts: t, model: st.model || 'codex', sid: p.session_id || path.basename(file, '.jsonl'),
        key: p.response_id || `${path.basename(file)}:${o.ordinal}`,
        in: Math.max(0, num(u.input_tokens) - cached), cr: cached, cw5: num(u.cache_write_input_tokens), cw1: 0,
        out: num(u.output_tokens), rs: num(u.reasoning_output_tokens),
        dur: clampDur(t - (st.mark || 0)) });
      st.mark = t;
    }, (ts, isMark) => { if (isMark && ts) st.mark = ts; }, marks);
    st.off = r.offset;
    return r;
  },
};

// ---------------------------------------------------------------- WorkBuddy
const workbuddy = {
  name: 'workbuddy',
  discover() {
    return walk(path.join(HOME, '.workbuddy-ai', 'projects'), (n) => n.endsWith('.jsonl'));
  },
  scan(file, st, emit, now) {
    const marks = ['"type":"function_call_result"', '"role":"user"'];
    const r = readLines(file, st.off || 0, ['"rawUsage"'], (text, ts) => {
      let o;
      try { o = JSON.parse(text); } catch { return; }
      const pd = o.providerData || {};
      const ru = pd.rawUsage;
      if (!ru) return;
      const t = ts || o.timestamp || now;
      const prompt = num(ru.prompt_tokens);
      const hit = num(ru.prompt_cache_hit_tokens) || num(ru.cached_tokens) ||
        num(ru.prompt_tokens_details && ru.prompt_tokens_details.cached_tokens) || num(ru.cache_read_input_tokens);
      const w = num(ru.cache_creation_input_tokens) || num(ru.prompt_cache_write_tokens);
      emit({ agent: 'workbuddy', ts: t, model: pd.model || pd.requestModelId || 'unknown',
        sid: o.sessionId || path.basename(file, '.jsonl'),
        key: pd.messageId || o.id,
        in: Math.max(0, prompt - hit - w), cr: hit, cw5: w, cw1: 0,
        out: num(ru.completion_tokens), rs: num(ru.completion_thinking_tokens) || num(ru.completion_tokens_details && ru.completion_tokens_details.reasoning_tokens),
        dur: clampDur(t - (st.mark || 0)) });
      st.mark = t;
    }, (ts, isMark) => { if (isMark && ts) st.mark = ts; }, marks);
    st.off = r.offset;
    return r;
  },
};

// ---------------------------------------------------------------- Antigravity
const antigravity = {
  name: 'antigravity',
  discover() {
    const root = path.join(HOME, '.gemini', 'antigravity', 'brain');
    let ids;
    try { ids = fs.readdirSync(root); } catch { return []; }
    const out = [];
    for (const id of ids) {
      const f = path.join(root, id, '.system_generated', 'logs', 'transcript_full.jsonl');
      if (fs.existsSync(f)) out.push(f);
    }
    return out;
  },
  scan(file, st, emit, now) {
    // 路径：brain/<会话ID>/.system_generated/logs/transcript_full.jsonl，向上三层才是会话目录
    // （多取一层会得到固定的 "brain"，不同会话的事件键撞在一起被去重吞掉）
    const convId = path.basename(path.dirname(path.dirname(path.dirname(file))));
    const r = readLines(file, st.off || 0, ['"input_tokens"', 'Model Selection'], (text, ts) => {
      // 模型切换提示出现在用户输入里：`Model Selection` from X to Y
      const sel = /Model Selection` from [^.]{0,60}? to ([^.\\"]{1,60})/.exec(text);
      if (sel && !text.includes('"input_tokens"')) { st.model = sel[1].trim(); return; }
      let o;
      try { o = JSON.parse(text); } catch { return; }
      if (o.input_tokens == null) { st.prevTs = ts || st.prevTs; return; }
      const t = ts || now;
      const cached = num(o.cache_read_tokens);
      emit({ agent: 'antigravity', ts: t, model: st.model || 'Gemini 3', sid: convId,
        key: `${convId}:${o.step_index}`,
        in: Math.max(0, num(o.input_tokens) - cached), cr: cached, cw5: 0, cw1: 0,
        out: num(o.output_tokens), rs: 0,
        // 时间戳只有秒级精度，耗时至少按 1 秒算
        dur: st.prevTs ? clampDur(Math.max(1000, t - st.prevTs)) : 0 });
      st.prevTs = t;
    }, (ts) => { if (ts) st.prevTs = ts; });
    st.off = r.offset;
    return r;
  },
};

module.exports = { collectors: [claude, codex, workbuddy, antigravity] };
