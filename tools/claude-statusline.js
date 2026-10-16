#!/usr/bin/env node
// Claude Code statusLine 钩子：把 stdin 里的 rate_limits（5 小时 / 每周额度）写到本地文件，供小组件读取。
// 只落盘额度百分比和重置时间，不碰任何令牌；输出一行简短状态给 Claude Code 显示。
// 用法（~/.claude/settings.json）："statusLine": { "type": "command", "command": "node <本文件绝对路径>" }
const fs = require('fs');
const os = require('os');
const path = require('path');

const dir = process.env.TOKEN_WIDGET_DIR || path.join(process.env.APPDATA || path.join(os.homedir(), '.config'), 'token-widget');

let raw = '';
process.stdin.setEncoding('utf8');
process.stdin.on('data', (c) => { raw += c; });
process.stdin.on('end', () => {
  let line = '';
  try {
    const j = JSON.parse(raw);
    const rl = j.rate_limits;
    // 诊断：只记录“被调用的时间 + 输入里有哪些字段名”，不记录任何内容；用来排查为什么没有额度数据
    try {
      fs.mkdirSync(dir, { recursive: true });
      fs.writeFileSync(path.join(dir, 'claude-statusline-seen.json'), JSON.stringify({ ts: Date.now(), keys: Object.keys(j), hasRateLimits: !!rl }));
    } catch { /* 诊断失败不影响主流程 */ }
    if (rl) {
      fs.mkdirSync(dir, { recursive: true });
      // 先写临时文件再改名，避免小组件读到写了一半的内容
      const f = path.join(dir, 'claude-limits.json');
      fs.writeFileSync(f + '.tmp', JSON.stringify({ ts: Date.now(), rate_limits: rl }));
      fs.renameSync(f + '.tmp', f);
      const p = (w) => (w && (w.used_percentage ?? w.used_percent) != null ? Math.round(w.used_percentage ?? w.used_percent) + '%' : '—');
      line = `5h ${p(rl.five_hour)} · 周 ${p(rl.seven_day)}`;
    }
  } catch { /* 输入不是 JSON 就静默，不影响 Claude Code */ }
  process.stdout.write(line);
});
