// Antigravity 额度：本地日志里没有，只能问它自己的本地语言服务（127.0.0.1 上的 GetUserStatus）。
// 接口令牌取自该进程的命令行参数，只放在内存里：不打印、不写盘、不发往任何非本机地址。
// 这是未公开的内部接口，可能随版本变化；任何一步失败都返回 null，界面回退成“暂无数据”。
const { execFile } = require('child_process');
const http = require('http');
const https = require('https');

const run = (cmd, args) => new Promise((resolve) => execFile(cmd, args, { windowsHide: true, maxBuffer: 8e6, timeout: 15000 }, (e, out) => resolve(e ? '' : String(out))));

const PATH = '/exa.language_server_pb.LanguageServerService/GetUserStatus';
const BODY = JSON.stringify({ metadata: { ideName: 'antigravity', extensionName: 'antigravity', locale: 'en' } });

class AntigravityQuota {
  constructor() {
    this.conn = null;      // { port, secure, tok }：当前可用的连接信息（只在内存）
    this.lastFind = 0;     // 上次重新发现进程的时间，避免失败时每次都跑 PowerShell
  }

  // 找语言服务进程：取令牌参数和监听端口
  async _discover() {
    if (process.platform !== 'win32') return null;
    const out = await run('powershell', ['-NoProfile', '-NonInteractive', '-Command',
      "Get-CimInstance Win32_Process -Filter \"Name='language_server.exe'\" | Where-Object { $_.CommandLine -match 'antigravity' } | Select-Object -First 1 | ForEach-Object { \"$($_.ProcessId)|$($_.CommandLine)\" }"]);
    const m = /^(\d+)\|/.exec(out.trim());
    const tok = /--csrf_token\s+(\S+)/.exec(out);
    if (!m || !tok) return null;
    const net = await run('netstat', ['-ano', '-p', 'tcp']);
    const ports = [...new Set(net.split(/\r?\n/)
      .filter((l) => /LISTENING/.test(l) && l.trim().endsWith(' ' + m[1]))
      .map((l) => /:(\d+)\s/.exec(l)).filter(Boolean).map((x) => Number(x[1])))];
    return { tok: tok[1], ports };
  }

  _post(port, secure, tok) {
    return new Promise((resolve) => {
      const req = (secure ? https : http).request({
        host: '127.0.0.1', port, method: 'POST', path: PATH, timeout: 6000,
        rejectUnauthorized: false, // 本机自签证书
        headers: { 'Content-Type': 'application/json', 'Connect-Protocol-Version': '1', 'X-Codeium-Csrf-Token': tok, 'Content-Length': Buffer.byteLength(BODY) },
      }, (r) => {
        let d = '';
        r.on('data', (c) => { d += c; });
        r.on('end', () => resolve(r.statusCode === 200 ? d : null));
      });
      req.on('error', () => resolve(null));
      req.on('timeout', () => { req.destroy(); resolve(null); });
      req.end(BODY);
    });
  }

  // 返回 { primary, models } 或 null。primary 取各模型里剩余最少的那个（最紧张的）。
  async fetch() {
    let raw = null;
    if (this.conn) raw = await this._post(this.conn.port, this.conn.secure, this.conn.tok);
    if (!raw) {
      this.conn = null;
      const now = Date.now();
      if (now - this.lastFind < 30000) return null;
      this.lastFind = now;
      const d = await this._discover();
      if (!d) return null;
      for (const port of d.ports) {
        for (const secure of [true, false]) {
          raw = await this._post(port, secure, d.tok);
          if (raw) { this.conn = { port, secure, tok: d.tok }; break; }
        }
        if (raw) break;
      }
      if (!raw) return null;
    }
    try { return this._parse(JSON.parse(raw)); } catch { return null; }
  }

  _parse(j) {
    const cfgs = (((j.userStatus || {}).cascadeModelConfigData || {}).clientModelConfigs) || [];
    const models = [];
    for (const c of cfgs) {
      const q = c.quotaInfo;
      if (!q || q.remainingFraction == null) continue;
      models.push({ label: c.label || '', remaining: Number(q.remainingFraction), resetsAt: q.resetTime ? Math.round(Date.parse(q.resetTime) / 1000) : 0 });
    }
    if (!models.length) return null;
    const worst = models.reduce((a, b) => (b.remaining < a.remaining ? b : a));
    return {
      primary: { used_percent: Math.max(0, Math.min(100, (1 - worst.remaining) * 100)), window_minutes: 300, resets_at: worst.resetsAt },
      models,
    };
  }
}

module.exports = { AntigravityQuota };
