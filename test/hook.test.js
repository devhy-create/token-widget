const test = require('node:test');
const assert = require('node:assert/strict');
const fs = require('fs');
const os = require('os');
const path = require('path');
const { spawnSync } = require('child_process');

const script = path.join(__dirname, '..', 'tools', 'claude-statusline.js');
const run = (dir, input) => spawnSync(process.execPath, [script], { input, encoding: 'utf8', env: { ...process.env, TOKEN_WIDGET_DIR: dir } });
const mkDir = () => fs.mkdtempSync(path.join(os.tmpdir(), 'tw-hook-'));

test('钩子：把 rate_limits 写到 claude-limits.json，并输出一行状态', () => {
  const dir = mkDir();
  const rl = { five_hour: { used_percentage: 6.4, resets_at: 1700000000 }, seven_day: { used_percentage: 28, resets_at: 1700500000 } };
  const r = run(dir, JSON.stringify({ model: 'x', rate_limits: rl }));
  assert.equal(r.status, 0);
  assert.equal(r.stdout, '5h 6% · 周 28%');
  const out = JSON.parse(fs.readFileSync(path.join(dir, 'claude-limits.json'), 'utf8'));
  assert.deepEqual(out.rate_limits, rl);
  assert.equal(typeof out.ts, 'number');
  assert.equal(fs.existsSync(path.join(dir, 'claude-limits.json.tmp')), false); // 临时文件已改名
});

test('钩子：只记录字段名到诊断文件，不记录内容', () => {
  const dir = mkDir();
  run(dir, JSON.stringify({ secret_field_value: 'TOPSECRET', rate_limits: { five_hour: { used_percentage: 1 } } }));
  const seen = fs.readFileSync(path.join(dir, 'claude-statusline-seen.json'), 'utf8');
  assert.ok(seen.includes('secret_field_value'));
  assert.ok(!seen.includes('TOPSECRET'));
  assert.equal(JSON.parse(seen).hasRateLimits, true);
});

test('钩子：输入里没有 rate_limits（如桌面版/订阅外）时不写额度文件', () => {
  const dir = mkDir();
  const r = run(dir, JSON.stringify({ model: 'x' }));
  assert.equal(r.status, 0);
  assert.equal(r.stdout, '');
  assert.equal(fs.existsSync(path.join(dir, 'claude-limits.json')), false);
  assert.equal(JSON.parse(fs.readFileSync(path.join(dir, 'claude-statusline-seen.json'), 'utf8')).hasRateLimits, false);
});

test('钩子：输入不是 JSON 时静默退出，不影响 Claude Code', () => {
  const r = run(mkDir(), '这不是 json');
  assert.equal(r.status, 0);
  assert.equal(r.stdout, '');
});

test('钩子：只有一个窗口时另一个显示 —', () => {
  const dir = mkDir();
  const r = run(dir, JSON.stringify({ rate_limits: { five_hour: { used_percent: 50 } } }));
  assert.equal(r.stdout, '5h 50% · 周 —');
});
