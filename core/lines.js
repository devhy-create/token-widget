// 增量按行读取大文件：只解码命中关键字的行，其余行只抽取时间戳。
// Codex 的 rollout 单文件可达数百 MB，所以全程基于 Buffer，不整体转字符串。
const fs = require('fs');

const TS_KEY = Buffer.from('"timestamp":');
const CA_KEY = Buffer.from('"created_at":');
const CHUNK = 8 * 1024 * 1024;

// 从一行 Buffer 里抽取时间戳（兼容 ISO 字符串与毫秒数字两种写法），失败返回 0
function extractTs(line) {
  let key = TS_KEY;
  let i = line.indexOf(key);
  if (i < 0) { key = CA_KEY; i = line.indexOf(key); }
  if (i < 0) return 0;
  let p = i + key.length;
  if (line[p] === 0x22) { // 引号开头 => ISO 字符串（带或不带毫秒），取到闭合引号
    let q = p + 1;
    while (q < line.length && q < p + 40 && line[q] !== 0x22) q++;
    const t = Date.parse(line.toString('latin1', p + 1, q));
    return Number.isNaN(t) ? 0 : t;
  }
  let n = 0;
  while (p < line.length && line[p] >= 0x30 && line[p] <= 0x39) n = n * 10 + (line[p++] - 0x30);
  return n;
}

/**
 * 从 offset 起读取完整行（以 \n 结尾）。
 * needles: 命中任一关键字的行才会解码为字符串交给 onLine(text, ts)；
 * 其余行仅回调 onSkip(ts, isMark)，命中 marks 关键字的行 isMark=true（用于界定一次请求的起点）。
 * 返回新的 offset（只推进到最后一个完整换行之后，半行留到下次）。
 */
function readLines(file, offset, needles, onLine, onSkip, marks = []) {
  const fd = fs.openSync(file, 'r');
  try {
    const size = fs.fstatSync(fd).size;
    if (size <= offset) return { offset: size, size };
    const keys = needles.map((s) => Buffer.from(s));
    const markKeys = marks.map((s) => Buffer.from(s));
    let pos = offset;
    let carry = Buffer.alloc(0);
    let consumed = offset;
    const buf = Buffer.allocUnsafe(Math.min(CHUNK, size - offset));
    while (pos < size) {
      const n = fs.readSync(fd, buf, 0, Math.min(buf.length, size - pos), pos);
      if (n <= 0) break;
      pos += n;
      let data = carry.length ? Buffer.concat([carry, buf.subarray(0, n)]) : buf.subarray(0, n);
      let start = 0;
      let nl;
      while ((nl = data.indexOf(0x0a, start)) >= 0) {
        const line = data.subarray(start, nl);
        const lineLen = nl - start + 1;
        start = nl + 1;
        consumed += lineLen;
        if (line.length < 2) continue;
        const ts = extractTs(line);
        if (keys.length === 0 || keys.some((k) => line.indexOf(k) >= 0)) onLine(line.toString('utf8'), ts);
        else if (onSkip) onSkip(ts, markKeys.some((k) => line.indexOf(k) >= 0));
      }
      // 剩余半行：复制一份，避免被下一轮覆盖复用的 buf
      carry = Buffer.from(data.subarray(start));
    }
    return { offset: consumed, size };
  } finally {
    fs.closeSync(fd);
  }
}

module.exports = { readLines, extractTs };
