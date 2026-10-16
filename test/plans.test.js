const test = require('node:test');
const assert = require('node:assert/strict');
const fs = require('fs');
const path = require('path');
const { useTempHome } = require('./helpers');

const home = useTempHome(); // 必须先于 require('../core/plans')
const plans = require('../core/plans');

const userFile = path.join(home, 'plans.user.json');

test('Claude：从 ~/.claude.json 的 oauthAccount 识别 Max 5x', () => {
  fs.writeFileSync(path.join(home, '.claude.json'), JSON.stringify({ oauthAccount: { organizationType: 'claude_max', organizationRateLimitTier: 'default_claude_max_5x' } }));
  const p = plans.detect(userFile);
  assert.equal(p.claude.key, 'max_5x');
  assert.equal(p.claude.name, 'Max 5x');
  assert.equal(p.claude.usd, 100);
  assert.equal(p.claude.source, 'detected');
  assert.equal(p.antigravity, null);
});

test('Codex：优先用日志里的 plan_type；价格表里没有的只显示名字', () => {
  assert.equal(plans.detect(userFile, { codex: 'Plus' }).codex.name, 'Plus');
  const p = plans.detect(userFile, { codex: 'weird_plan' }).codex;
  assert.equal(p.name, 'weird_plan');
  assert.equal(p.usd, null);
});

test('Codex：日志没有时解码 auth.json 的 id_token 套餐声明', () => {
  const payload = Buffer.from(JSON.stringify({ 'https://api.openai.com/auth': { chatgpt_plan_type: 'pro' } })).toString('base64url');
  fs.mkdirSync(path.join(home, '.codex'), { recursive: true });
  fs.writeFileSync(path.join(home, '.codex', 'auth.json'), JSON.stringify({ tokens: { id_token: `x.${payload}.y` } }));
  assert.equal(plans.detect(userFile).codex.key, 'pro');
});

test('用户文件覆盖自动识别；usd 非法按 0 处理', () => {
  plans.writeUser(userFile, 'claude', { name: '我的套餐', usd: 'abc' });
  const p = plans.detect(userFile);
  assert.equal(p.claude.source, 'user');
  assert.equal(p.claude.name, '我的套餐');
  assert.equal(p.claude.usd, 0);
});

test('writeUser / readUser：保留其它键；传 null 恢复自动识别；名称截断到 24 字', () => {
  fs.writeFileSync(userFile, JSON.stringify({ other: { keep: 1 } }));
  plans.writeUser(userFile, 'workbuddy', { name: 'x'.repeat(50), usd: 20 });
  let u = plans.readUser(userFile);
  assert.deepEqual(u.other, { keep: 1 });
  assert.equal(u.workbuddy.name.length, 24);
  assert.equal(u.workbuddy.usd, 20);
  plans.writeUser(userFile, 'workbuddy', null);
  u = plans.readUser(userFile);
  assert.equal(u.workbuddy, undefined);
  assert.deepEqual(u.other, { keep: 1 });
});

test('readUser：文件不存在返回空对象', () => {
  assert.deepEqual(plans.readUser(path.join(home, 'nope.json')), {});
});
