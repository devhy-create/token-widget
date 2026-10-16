const test = require('node:test');
const assert = require('node:assert/strict');
const fs = require('fs');
const os = require('os');
const path = require('path');
const { readLines, extractTs } = require('../core/lines');

const tmp = fs.mkdtempSync(path.join(os.tmpdir(), 'tw-lines-'));
const B = (s) => Buffer.from(s);

test('extractTs：ISO 字符串、毫秒数字、created_at、缺失', () => {
  assert.equal(extractTs(B('{"timestamp":"2026-01-02T03:04:05.000Z","x":1}')), Date.parse('2026-01-02T03:04:05.000Z'));
  assert.equal(extractTs(B('{"timestamp":1767323045000,"x":1}')), 1767323045000);
  assert.equal(extractTs(B('{"created_at":1700000000000}')), 1700000000000);
  assert.equal(extractTs(B('{"x":1}')), 0);
  assert.equal(extractTs(B('{"timestamp":"not-a-date"}')), 0);
});

test('readLines：只解码命中关键字的行，其余行走 onSkip，marks 标记起点', () => {
  const f = path.join(tmp, 'a.jsonl');
  fs.writeFileSync(f, [
    '{"timestamp":1000,"k":"hit"}',
    '{"timestamp":2000,"k":"skip"}',
    '{"timestamp":3000,"k":"mark"}',
  ].join('\n') + '\n');
  const hits = []; const skips = [];
  const r = readLines(f, 0, ['"hit"'], (t, ts) => hits.push([t, ts]), (ts, m) => skips.push([ts, m]), ['"mark"']);
  assert.equal(hits.length, 1);
  assert.equal(hits[0][1], 1000);
  assert.deepEqual(skips, [[2000, false], [3000, true]]);
  assert.equal(r.offset, fs.statSync(f).size);
});

test('readLines：增量读取，半行留到下次，不重复不丢失', () => {
  const f = path.join(tmp, 'b.jsonl');
  fs.writeFileSync(f, '{"timestamp":1,"k":"hit","n":1}\n{"timestamp":2,"k":"hit","n":');
  const got = [];
  let r = readLines(f, 0, ['"hit"'], (t) => got.push(JSON.parse(t).n), null);
  assert.deepEqual(got, [1]);
  // offset 只推进到最后一个完整换行之后
  assert.equal(r.offset, Buffer.byteLength('{"timestamp":1,"k":"hit","n":1}\n'));
  fs.appendFileSync(f, '2}\n{"timestamp":3,"k":"hit","n":3}\n');
  r = readLines(f, r.offset, ['"hit"'], (t) => got.push(JSON.parse(t).n), null);
  assert.deepEqual(got, [1, 2, 3]);
  assert.equal(r.offset, fs.statSync(f).size);
  // 没有新内容：直接返回
  assert.equal(readLines(f, r.offset, ['"hit"'], () => assert.fail('不应再回调'), null).offset, r.offset);
});

test('readLines：多字节字符跨读取边界不乱码（整行解码）', () => {
  const f = path.join(tmp, 'c.jsonl');
  fs.writeFileSync(f, '{"timestamp":1,"k":"hit","s":"中文内容"}\n');
  const got = [];
  readLines(f, 0, ['"hit"'], (t) => got.push(JSON.parse(t).s), null);
  assert.deepEqual(got, ['中文内容']);
});

test('readLines：needles 为空数组时所有行都解码', () => {
  const f = path.join(tmp, 'd.jsonl');
  fs.writeFileSync(f, '{"timestamp":1}\n{"timestamp":2}\n');
  let n = 0;
  readLines(f, 0, [], () => n++, null);
  assert.equal(n, 2);
});
