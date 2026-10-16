const test = require('node:test');
const assert = require('node:assert/strict');
const { AntigravityQuota } = require('../core/antigravity');

const q = new AntigravityQuota();
const cfg = (label, remaining, resetTime) => ({ label, quotaInfo: remaining == null ? {} : { remainingFraction: remaining, resetTime } });
const wrap = (list) => ({ userStatus: { cascadeModelConfigData: { clientModelConfigs: list } } });

test('_parse：primary 取剩余最少的模型，百分比 = 已用', () => {
  const r = q._parse(wrap([
    cfg('Gemini 3 Pro', 0.8, '2026-03-01T12:00:00Z'),
    cfg('Claude Sonnet', 0.25, '2026-03-01T13:00:00Z'),
    cfg('无额度信息', null),
  ]));
  assert.equal(r.models.length, 2);
  assert.equal(r.primary.used_percent, 75);
  assert.equal(r.primary.window_minutes, 300);
  assert.equal(r.primary.resets_at, Math.round(Date.parse('2026-03-01T13:00:00Z') / 1000));
});

test('_parse：剩余比例越界时百分比钳制在 0~100', () => {
  assert.equal(q._parse(wrap([cfg('a', 1.5, '2026-03-01T12:00:00Z')])).primary.used_percent, 0);
  assert.equal(q._parse(wrap([cfg('a', -0.5, '2026-03-01T12:00:00Z')])).primary.used_percent, 100);
});

test('_parse：缺少 resetTime 时 resets_at 为 0', () => {
  assert.equal(q._parse(wrap([cfg('a', 0.5, undefined)])).primary.resets_at, 0);
});

test('_parse：没有任何带额度的模型 / 结构缺失 → null', () => {
  assert.equal(q._parse(wrap([])), null);
  assert.equal(q._parse(wrap([cfg('a', null)])), null);
  assert.equal(q._parse({}), null);
});
