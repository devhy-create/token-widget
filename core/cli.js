// 命令行对账工具：node core/cli.js [--fresh] [--json]
// --fresh 忽略缓存重新全量扫描（不会覆盖正式缓存，除非加 --save）
const path = require('path');
const { Engine, AGENTS } = require('./engine');

const fmt = (n) => (n >= 1e9 ? (n / 1e9).toFixed(2) + 'B' : n >= 1e6 ? (n / 1e6).toFixed(2) + 'M' : n >= 1e3 ? (n / 1e3).toFixed(1) + 'K' : String(Math.round(n)));
const usd = (n) => '$' + n.toFixed(2);

(async () => {
  const args = process.argv.slice(2);
  const fresh = args.includes('--fresh');
  const eng = new Engine({ fresh, dir: fresh && !args.includes('--save') ? path.join(require('os').tmpdir(), 'token-widget-cli') : undefined });
  const t0 = Date.now();
  await eng.tick();
  console.log(`扫描耗时 ${((Date.now() - t0) / 1000).toFixed(1)}s，文件 ${Object.keys(eng.files).length} 个，去重键 ${eng.seen.size}`);
  const snap = eng.snapshot();
  if (args.includes('--json')) { console.log(JSON.stringify(snap, null, 1)); }
  for (const ag of AGENTS) {
    const a = snap.agents[ag];
    console.log(`\n=== ${ag}`);
    for (const [k, label] of [['today', '今日'], ['d7', '近7天'], ['d30', '近30天'], ['all', '累计']]) {
      const v = a[k];
      console.log(`${label.padEnd(5)} 请求 ${String(v.n).padStart(6)}  tokens ${fmt(v.tokens).padStart(8)}  (in ${fmt(v.in)} / 缓存读 ${fmt(v.cr)} / 缓存写 ${fmt(v.cw)} / out ${fmt(v.out)})  ${usd(v.usd)}${v.est ? ' ≈估价' : ''}${v.unknown ? ' [含未定价模型]' : ''}`);
    }
    for (const m of a.all.models.slice(0, 8)) console.log(`   ${m.model.padEnd(28)} ${fmt(m.tokens).padStart(8)}  ${usd(m.usd)}${m.known ? '' : ' [未定价]'}${m.est ? ' ≈' : ''}`);
    if (a.limits) console.log('   额度:', JSON.stringify(a.limits));
  }
  eng.save();
})();
