const test = require('node:test');
const assert = require('node:assert/strict');
const fs = require('fs');
const path = require('path');
const { useTempHome, writeJsonl } = require('./helpers');

const home = useTempHome(); // 必须先于 require('../core/engine')
const { Engine } = require('../core/engine');

const mkDir = (n) => { const d = path.join(home, 'data-' + n); fs.mkdirSync(d, { recursive: true }); return d; };
const claudeLine = (id, ts, out = 100) => ({ timestamp: new Date(ts).toISOString(), type: 'assistant', sessionId: 's', message: { id, model: 'claude-sonnet-5-5', usage: { input_tokens: 1000, cache_read_input_tokens: 0, output_tokens: out } } });

test('tick → snapshot：今日汇总、费用按价格表折算、agent 的 found 标记', async () => {
  const now = Date.now();
  writeJsonl(path.join(home, '.claude', 'projects', 'p', 'a.jsonl'), [claudeLine('m1', now - 20000, 1000), claudeLine('m2', now - 10000, 1000)]);
  const eng = new Engine({ dir: mkDir('a'), fresh: true });
  await eng.tick();
  eng.shutdown(); // 最后一条消息要等 finish 才入账
  const s = eng.snapshot();
  const c = s.agents.claude;
  assert.equal(c.found, true);
  assert.equal(c.today.n, 2);
  assert.equal(c.today.tokens, 2 * (1000 + 1000));
  // claude-sonnet-5：in 2 / out 10（美元每百万）
  assert.ok(Math.abs(c.today.usd - (2000 * 2 + 2000 * 10) / 1e6) < 1e-12);
  assert.equal(s.agents.codex.found, false);
  assert.equal(s.agents.codex.today.tokens, 0);
});

test('同一份日志复制两份（会话分叉/归档）：按事件键去重，总量不变', async () => {
  const now = Date.now();
  const lines = [claudeLine('d1', now - 30000), claudeLine('d2', now - 20000), claudeLine('d3', now - 10000)];
  writeJsonl(path.join(home, '.claude', 'projects', 'q', 'one.jsonl'), lines);
  const eng1 = new Engine({ dir: mkDir('b1'), fresh: true });
  await eng1.tick(); eng1.shutdown();
  const single = eng1.snapshot().agents.claude.today.tokens;

  writeJsonl(path.join(home, '.claude', 'projects', 'q', 'two.jsonl'), lines);
  const eng2 = new Engine({ dir: mkDir('b2'), fresh: true });
  await eng2.tick(); eng2.shutdown();
  const dup = eng2.snapshot().agents.claude.today.tokens;
  // 两个引擎都包含前一个测试写入的 p/a.jsonl；复制 one→two 后总量应与只有 one 时一致
  assert.equal(dup, single);
});

test('缓存：shutdown 落盘后重新加载，数据一致且不重复扫描', async () => {
  const dir = mkDir('c');
  const e1 = new Engine({ dir, fresh: true });
  await e1.tick(); e1.shutdown();
  const t1 = e1.snapshot().agents.claude.today.tokens;
  const e2 = new Engine({ dir });
  await e2.tick(); e2.shutdown();
  assert.equal(e2.snapshot().agents.claude.today.tokens, t1);
});

test('setExternalLimits：写入额度窗口，snapshot 带出；较旧时间戳不覆盖较新的', () => {
  const eng = new Engine({ dir: mkDir('d'), fresh: true });
  eng.setExternalLimits('antigravity', { primary: { used_percent: 30, window_minutes: 300, resets_at: 1 }, models: [{ label: 'm', remaining: 0.7, resetsAt: 1 }] });
  const l = eng.snapshot().agents.antigravity.limits;
  assert.equal(l.primary.used_percent, 30);
  assert.equal(l.models.length, 1);
  eng._setLimits('antigravity', Date.now() - 100000, { primary: { used_percent: 99 } });
  assert.equal(eng.snapshot().agents.antigravity.limits.primary.used_percent, 30);
});

test('_readClaudeLimits：读取钩子写出的 claude-limits.json（秒/ISO 两种 resets_at）', () => {
  const dir = mkDir('e');
  const eng = new Engine({ dir, fresh: true });
  const resetSec = Math.floor(Date.now() / 1000) + 3600;
  fs.writeFileSync(path.join(dir, 'claude-limits.json'), JSON.stringify({ ts: Date.now(), rate_limits: {
    five_hour: { used_percentage: 6, resets_at: resetSec },
    seven_day: { used_percentage: 28.5, resets_at: new Date((resetSec + 86400) * 1000).toISOString() },
  } }));
  eng._readClaudeLimits();
  const l = eng.snapshot().agents.claude.limits;
  assert.equal(l.primary.used_percent, 6);
  assert.equal(l.primary.window_minutes, 300);
  assert.equal(l.primary.resets_at, resetSec);
  assert.equal(l.secondary.used_percent, 28.5);
  assert.equal(l.secondary.window_minutes, 10080);
  assert.equal(l.secondary.resets_at, resetSec + 86400);
});

test('_readClaudeLimits：文件不存在或损坏时不抛错、不产生额度', () => {
  const dir = mkDir('f');
  const eng = new Engine({ dir, fresh: true });
  eng._readClaudeLimits();
  assert.equal(eng.snapshot().agents.claude.limits, null);
  fs.writeFileSync(path.join(dir, 'claude-limits.json'), '{坏掉');
  eng._readClaudeLimits();
  assert.equal(eng.snapshot().agents.claude.limits, null);
});

test('实时速度：用事件耗时算 tok/s，并按模型拆分', async () => {
  const now = Date.now();
  const f = path.join(home, '.codex', 'sessions', 'live.jsonl');
  const ts = (ms) => new Date(now - ms).toISOString();
  writeJsonl(f, [
    { timestamp: ts(12000), type: 'turn_context', payload: { model: 'gpt-6.1-sol' } },
    { timestamp: ts(10000), type: 'response_item', payload: { role: 'user' } },
    { timestamp: ts(5000), type: 'token_usage_record', ordinal: 1, payload: { response_id: 'live1', usage: { input_tokens: 10, output_tokens: 500 } } },
  ]);
  const eng = new Engine({ dir: mkDir('g'), fresh: true });
  await eng.tick();
  const live = eng.snapshot().agents.codex.live;
  assert.equal(live.active, true);
  assert.ok(Math.abs(live.tps - 100) < 1e-6); // 500 token / 5 秒
  assert.equal(live.models.length, 1);
  assert.equal(live.models[0].model, 'gpt-6.1-sol');
});
