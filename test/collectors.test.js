const test = require('node:test');
const assert = require('node:assert/strict');
const path = require('path');
const { useTempHome, writeJsonl, collect } = require('./helpers');

const home = useTempHome(); // 必须先于 require('../core/collectors')
const { collectors } = require('../core/collectors');
const by = Object.fromEntries(collectors.map((c) => [c.name, c]));

const T0 = Date.parse('2026-03-01T10:00:00.000Z');
const iso = (ms) => new Date(T0 + ms).toISOString();

test('Claude：同一消息多行合并取最大值，out = output_tokens + thinking，缓存写按 5m/1h 拆分', () => {
  const f = path.join(home, '.claude', 'projects', 'p1', 's1.jsonl');
  const usage = (out, think) => ({
    input_tokens: 10, cache_read_input_tokens: 1000, cache_creation_input_tokens: 140,
    cache_creation: { ephemeral_5m_input_tokens: 100, ephemeral_1h_input_tokens: 40 },
    output_tokens: out, output_tokens_details: { thinking_tokens: think },
  });
  writeJsonl(f, [
    { timestamp: iso(0), type: 'user', sessionId: 's1', message: { role: 'user', content: 'hi' } },
    // 同一条消息被拆成两行写入：第一行 output 还没写全
    { timestamp: iso(1000), type: 'assistant', sessionId: 's1', message: { id: 'm1', model: 'claude-sonnet-5-5', usage: usage(5, 0) } },
    { timestamp: iso(2000), type: 'assistant', sessionId: 's1', message: { id: 'm1', model: 'claude-sonnet-5-5', usage: usage(50, 7) } },
    { timestamp: iso(5000), type: 'assistant', sessionId: 's1', message: { id: 'm2', model: 'claude-sonnet-5-5', usage: { input_tokens: 1, output_tokens: 2 } } },
    // 合成消息与非 assistant 行不入账
    { timestamp: iso(6000), type: 'assistant', message: { id: 'm3', model: '<synthetic>', usage: { input_tokens: 99, output_tokens: 99 } } },
  ]);
  const { evs, st } = collect(by.claude, f);
  // m2 出现时 m1 才被 flush；m2 要等 4 秒静默或 finish
  assert.equal(evs.length, 1);
  const e = evs[0];
  assert.deepEqual([e.key, e.model, e.sid], ['m1', 'claude-sonnet-5-5', 's1']);
  assert.deepEqual([e.in, e.cr, e.cw5, e.cw1, e.out, e.rs], [10, 1000, 100, 40, 57, 7]);
  assert.equal(e.ts, T0 + 2000);
  assert.equal(e.dur, 2000); // 从上一行（用户消息）到消息最后一行
  by.claude.finish(st, (x) => evs.push(x));
  assert.equal(evs.length, 2);
  assert.equal(evs[1].key, 'm2');
  assert.equal(evs[1].out, 2);
});

test('Claude：没有 5m/1h 拆分时整体按 5 分钟缓存写；4 秒静默后入账', () => {
  const f = path.join(home, '.claude', 'projects', 'p1', 's2.jsonl');
  writeJsonl(f, [
    { timestamp: iso(0), type: 'assistant', message: { id: 'a1', model: 'claude-opus-5-5', usage: { input_tokens: 1, cache_creation_input_tokens: 80, output_tokens: 3 } } },
  ]);
  const evs = [];
  const st = {};
  by.claude.scan(f, st, (e) => evs.push(e), Date.now());
  assert.equal(evs.length, 0); // 刚写入，可能还在追加
  by.claude.scan(f, st, (e) => evs.push(e), Date.now() + 5000);
  assert.equal(evs.length, 1);
  assert.deepEqual([evs[0].cw5, evs[0].cw1], [80, 0]);
});

test('Claude：增量读取不会重复入账', () => {
  const f = path.join(home, '.claude', 'projects', 'p1', 's3.jsonl');
  const line = (id, t) => ({ timestamp: iso(t), type: 'assistant', message: { id, model: 'claude-sonnet-5-5', usage: { input_tokens: 1, output_tokens: 1 } } });
  writeJsonl(f, [line('x1', 0)]);
  const evs = [];
  const st = {};
  by.claude.scan(f, st, (e) => evs.push(e), Date.now());
  require('fs').appendFileSync(f, JSON.stringify(line('x2', 1000)) + '\n');
  by.claude.scan(f, st, (e) => evs.push(e), Date.now());
  by.claude.finish(st, (e) => evs.push(e));
  assert.deepEqual(evs.map((e) => e.key), ['x1', 'x2']);
});

test('Claude.discover：递归找到 projects 下的 jsonl', () => {
  assert.ok(by.claude.discover().some((p) => p.endsWith('s1.jsonl')));
});

test('Codex：turn_context 取模型；token_usage_record 入账；token_count 的 rate_limits 交给 setLimits', () => {
  const f = path.join(home, '.codex', 'sessions', '2026', '03', '01', 'rollout.jsonl');
  writeJsonl(f, [
    { timestamp: iso(0), type: 'turn_context', payload: { model: 'gpt-6.1-sol' } },
    { timestamp: iso(1000), type: 'response_item', payload: { role: 'user', content: 'x' } },
    { timestamp: iso(4000), type: 'token_usage_record', ordinal: 1, payload: { response_id: 'r1', session_id: 'cs1', usage: { input_tokens: 1000, cached_input_tokens: 800, output_tokens: 200, reasoning_output_tokens: 50 } } },
    { timestamp: iso(5000), type: 'event_msg', payload: { type: 'token_count', rate_limits: { plan_type: 'plus', primary: { used_percent: 12, window_minutes: 300, resets_at: 1 } } } },
  ]);
  const lim = [];
  const { evs } = collect(by.codex, f, {}, Date.now(), (a, t, rl) => lim.push([a, t, rl]));
  assert.equal(evs.length, 1);
  const e = evs[0];
  assert.deepEqual([e.model, e.sid, e.key], ['gpt-6.1-sol', 'cs1', 'r1']);
  assert.deepEqual([e.in, e.cr, e.out, e.rs], [200, 800, 200, 50]); // in = 总输入 - 缓存命中
  assert.equal(e.dur, 3000); // 从最近一次“用户输入”起算
  assert.equal(lim.length, 1);
  assert.equal(lim[0][0], 'codex');
  assert.equal(lim[0][2].plan_type, 'plus');
  assert.equal(lim[0][2].primary.used_percent, 12);
});

test('WorkBuddy：providerData.rawUsage 归一化', () => {
  const f = path.join(home, '.workbuddy-ai', 'projects', 'p', 'w1.jsonl');
  writeJsonl(f, [
    { timestamp: T0, sessionId: 'ws1', message: { role: 'user' } },
    // role:user 行（上面）为请求起点，下一行才是用量
    { timestamp: T0 + 2500, id: 'o1', sessionId: 'ws1', providerData: { model: 'deepseek-v4', messageId: 'wm1', rawUsage: {
      prompt_tokens: 1000, prompt_cache_hit_tokens: 600, completion_tokens: 100, completion_tokens_details: { reasoning_tokens: 30 } } } },
  ]);
  const { evs } = collect(by.workbuddy, f);
  assert.equal(evs.length, 1);
  const e = evs[0];
  assert.deepEqual([e.model, e.key, e.sid], ['deepseek-v4', 'wm1', 'ws1']);
  assert.deepEqual([e.in, e.cr, e.cw5, e.out, e.rs], [400, 600, 0, 100, 30]);
});

test('Antigravity：input_tokens 行入账，Model Selection 切换模型，不同会话的事件键不撞车', () => {
  const mk = (id) => path.join(home, '.gemini', 'antigravity', 'brain', id, '.system_generated', 'logs', 'transcript_full.jsonl');
  const body = (extra) => [
    { timestamp: iso(0), type: 'USER', content: 'Model Selection` from Gemini 3 Pro to Gemini 3 Flash.' },
    { timestamp: iso(3000), step_index: 7, input_tokens: 5000, cache_read_tokens: 4000, output_tokens: 300, ...extra },
  ];
  writeJsonl(mk('conv-a'), body());
  writeJsonl(mk('conv-b'), body());
  const a = collect(by.antigravity, mk('conv-a')).evs;
  const b = collect(by.antigravity, mk('conv-b')).evs;
  assert.equal(a.length, 1);
  assert.equal(a[0].model, 'Gemini 3 Flash');
  assert.deepEqual([a[0].in, a[0].cr, a[0].out], [1000, 4000, 300]);
  assert.equal(a[0].sid, 'conv-a');
  assert.equal(a[0].key, 'conv-a:7');
  // 回归：两个会话同一 step_index 的键必须不同，否则会被引擎去重吞掉
  assert.notEqual(a[0].key, b[0].key);
  assert.deepEqual(by.antigravity.discover().sort().map((p) => path.basename(path.dirname(path.dirname(path.dirname(p))))), ['conv-a', 'conv-b']);
});
