const test = require('node:test');
const assert = require('node:assert/strict');
const fs = require('fs');
const os = require('os');
const path = require('path');
const pricing = require('../core/pricing');

const tmp = fs.mkdtempSync(path.join(os.tmpdir(), 'tw-pricing-'));
const zero = { in: 0, cr: 0, cw5: 0, cw1: 0, out: 0 };

test('最长前缀匹配：claude-opus-5-5 优先于 claude-opus-5', () => {
  pricing.load(null);
  assert.equal(pricing.find('claude-opus-5-5-20260101').match, 'claude-opus-5-5');
  assert.equal(pricing.find('claude-opus-5-1').match, 'claude-opus-5');
});

test('大小写不敏感；未知模型返回 null', () => {
  pricing.load(null);
  assert.ok(pricing.find('Claude-Sonnet-5-5'));
  assert.equal(pricing.find('totally-unknown-model'), null);
  assert.equal(pricing.find(undefined), null);
});

test('cost：按 美元/百万 token 计算，缓存写区分 5 分钟与 1 小时', () => {
  pricing.load(null);
  // claude-sonnet-5：in 2、out 10、cr 0.2、cw5 2.5、cw1 4
  const c = pricing.cost('claude-sonnet-5-5', { in: 1e6, out: 1e6, cr: 1e6, cw5: 1e6, cw1: 1e6 });
  assert.ok(Math.abs(c.usd - (2 + 10 + 0.2 + 2.5 + 4)) < 1e-9);
  assert.equal(c.known, true);
  assert.equal(c.est, false);
});

test('cost：未知模型 known=false、金额 0；est 价格带估价标记', () => {
  pricing.load(null);
  assert.deepEqual(pricing.cost('nope', { ...zero, in: 1e6 }), { usd: 0, est: false, known: false });
  assert.equal(pricing.cost('gpt-6.1-sol', { ...zero, in: 1 }).est, true);
});

test('用户覆盖：同长度前缀优先用户条目，且可追加新模型', () => {
  const f = path.join(tmp, 'pricing.user.json');
  fs.writeFileSync(f, JSON.stringify({ models: [
    { match: 'claude-sonnet-5', in: 100, out: 0, cr: 0, cw5: 0, cw1: 0 },
    { match: 'my-model', in: 1, out: 2, cr: 0, cw5: 0, cw1: 0 },
  ] }));
  pricing.load(f);
  assert.equal(pricing.cost('claude-sonnet-5-5', { ...zero, in: 1e6 }).usd, 100);
  assert.equal(pricing.cost('my-model-x', { ...zero, out: 1e6 }).usd, 2);
});

test('用户文件损坏时忽略，沿用默认表', () => {
  const f = path.join(tmp, 'bad.json');
  fs.writeFileSync(f, '{ 这不是 json');
  pricing.load(f);
  assert.equal(pricing.find('claude-sonnet-5-5').in, 2);
});
