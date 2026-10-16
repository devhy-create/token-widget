// 订阅套餐检测：Claude 读 ~/.claude.json 的账号信息；Codex 优先用日志里的 plan_type，其次解码 auth.json 里 id_token 的套餐声明。
// 只提取套餐字段，不保存也不输出任何令牌、邮箱。用户可用 plans.user.json 覆盖。
const fs = require('fs');
const path = require('path');
const os = require('os');

const HOME = os.homedir();
const defaults = JSON.parse(fs.readFileSync(path.join(__dirname, 'plans.default.json'), 'utf8'));

function readJson(f) {
  try { return JSON.parse(fs.readFileSync(f, 'utf8')); } catch { return null; }
}

function claudeKey() {
  const j = readJson(path.join(HOME, '.claude.json'));
  const a = j && j.oauthAccount;
  if (!a) return null;
  const org = String(a.organizationType || '');
  const tier = String(a.organizationRateLimitTier || '');
  const m = /max_(\d+)x/.exec(tier);
  if (/max/.test(org) || m) return m ? `max_${m[1]}x` : 'max';
  if (/pro/.test(org)) return 'pro';
  if (/team|enterprise/.test(org)) return 'team';
  if (/free/.test(org)) return 'free';
  return null;
}

function codexKey(logPlan) {
  if (logPlan) return String(logPlan).toLowerCase();
  const a = readJson(path.join(HOME, '.codex', 'auth.json'));
  const t = a && a.tokens && a.tokens.id_token;
  if (!t) return null;
  try {
    const pl = JSON.parse(Buffer.from(t.split('.')[1], 'base64url').toString());
    const p = pl['https://api.openai.com/auth'] && pl['https://api.openai.com/auth'].chatgpt_plan_type;
    return p ? String(p).toLowerCase() : null;
  } catch { return null; }
}

// 返回 {agent: {key, name, usd, est, source}}；检测不到的 agent 为 null（界面不显示套餐，可用用户文件补）
function detect(userFile, logPlans = {}) {
  const user = readJson(userFile) || {};
  const out = {};
  const det = { claude: claudeKey(), codex: codexKey(logPlans.codex), antigravity: null, workbuddy: null };
  for (const ag of Object.keys(det)) {
    const u = user[ag];
    if (u && u.name) { out[ag] = { key: 'user', name: u.name, usd: Number(u.usd) || 0, est: false, source: 'user' }; continue; }
    const key = det[ag];
    if (!key) { out[ag] = null; continue; }
    const p = (defaults[ag] || {})[key];
    // 检测到了套餐但价格表里没有：只显示名字，不做价值换算
    out[ag] = p ? { key, name: p.name, usd: p.usd, est: !!p.est, source: 'detected' } : { key, name: key, usd: null, est: false, source: 'detected' };
  }
  return out;
}

// 读写用户覆盖文件 plans.user.json（设置面板用）：entry 为 {name, usd} 表示覆盖，null 表示恢复自动识别。其余键原样保留。
function readUser(file) {
  return readJson(file) || {};
}
function writeUser(file, agent, entry) {
  const u = readJson(file) || {};
  if (!u['_说明']) u['_说明'] = '键名为 agent（claude/codex/antigravity/workbuddy）的条目会覆盖自动识别的套餐；usd 为月费（美元）。可在小组件设置面板里改';
  if (entry) u[agent] = { name: String(entry.name || '自定义').slice(0, 24), usd: Math.max(0, Number(entry.usd) || 0) };
  else delete u[agent];
  fs.writeFileSync(file, JSON.stringify(u, null, 2));
}

module.exports = { detect, defaults, readUser, writeUser };
