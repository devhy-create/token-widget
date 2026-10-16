// 模型定价：默认表 + 用户覆盖表，按模型名前缀（最长匹配）取价。
const fs = require('fs');
const path = require('path');

let table = [];

function load(userFile) {
  const def = JSON.parse(fs.readFileSync(path.join(__dirname, 'pricing.default.json'), 'utf8')).models;
  let user = [];
  try {
    if (userFile && fs.existsSync(userFile)) {
      const j = JSON.parse(fs.readFileSync(userFile, 'utf8'));
      user = Array.isArray(j) ? j : j.models || [];
    }
  } catch { /* 用户文件写坏了就忽略，沿用默认表 */ }
  // 用户条目放前面；同样长度的前缀优先取用户条目
  table = [...user, ...def].map((e) => ({ ...e, match: String(e.match).toLowerCase() }));
}

function find(model) {
  const m = String(model || '').toLowerCase();
  let best = null;
  for (const e of table) {
    if (m.startsWith(e.match) && (!best || e.match.length > best.match.length)) best = e;
  }
  return best;
}

// b: 一个汇总桶 {in, cr, cw5, cw1, out}，返回 {usd, est, known}
function cost(model, b) {
  const p = find(model);
  if (!p) return { usd: 0, est: false, known: false };
  const usd = (b.in * p.in + b.out * p.out + b.cr * p.cr + b.cw5 * (p.cw5 || p.in) + b.cw1 * (p.cw1 || p.in)) / 1e6;
  return { usd, est: !!p.est, known: true };
}

module.exports = { load, find, cost };
