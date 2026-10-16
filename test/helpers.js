// 测试公共工具。
// collectors.js / plans.js 在模块加载时就取 os.homedir()，所以每个测试文件必须在 require 任何 core 模块之前
// 先调用 useTempHome()，把 HOME/USERPROFILE 指到临时目录（每个测试文件是独立进程，互不影响）。
const fs = require('fs');
const os = require('os');
const path = require('path');

function useTempHome() {
  const home = fs.mkdtempSync(path.join(os.tmpdir(), 'tw-test-'));
  process.env.HOME = home;
  process.env.USERPROFILE = home;
  return home;
}

// 把若干对象写成 jsonl（每行一个 JSON，末尾带换行）
function writeJsonl(file, objs) {
  fs.mkdirSync(path.dirname(file), { recursive: true });
  fs.writeFileSync(file, objs.map((o) => (typeof o === 'string' ? o : JSON.stringify(o))).join('\n') + '\n');
}

function collect(collector, file, st = {}, now = Date.now(), setLimits) {
  const evs = [];
  collector.scan(file, st, (e) => evs.push(e), now, setLimits);
  return { evs, st };
}

module.exports = { useTempHome, writeJsonl, collect };
