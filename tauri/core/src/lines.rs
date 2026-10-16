//! 增量按行读取大文件：只解码命中关键字的行，其余行只抽取时间戳。
//! Codex 的 rollout 单文件可达数百 MB，所以全程基于字节，不整体转字符串。
use crate::util::parse_date_ms;
use memchr::memmem;
use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom};
use std::path::Path;

const TS_KEY: &[u8] = b"\"timestamp\":";
const CA_KEY: &[u8] = b"\"created_at\":";
const CHUNK: u64 = 8 * 1024 * 1024;

/// 一行的回调参数
pub enum Line<'a> {
    /// 命中关键字的行：整行解码后的文本 + 时间戳（取不到为 0）
    Hit(&'a str, i64),
    /// 未命中的行：时间戳 + 是否命中 marks（界定一次请求的起点）
    Skip(i64, bool),
}

pub struct ReadResult {
    pub offset: u64,
    pub size: u64,
}

/// 从一行字节里抽取时间戳（兼容 ISO 字符串与毫秒数字两种写法），失败返回 0
pub fn extract_ts(line: &[u8]) -> i64 {
    let mut key = TS_KEY;
    let mut found = memmem::find(line, key);
    if found.is_none() {
        key = CA_KEY;
        found = memmem::find(line, key);
    }
    let Some(i) = found else { return 0 };
    let mut p = i + key.len();
    if line.get(p) == Some(&b'"') {
        // 引号开头 => ISO 字符串（带或不带毫秒），取到闭合引号，最多 40 字节防止跑飞
        let mut q = p + 1;
        while q < line.len() && q < p + 40 && line[q] != b'"' {
            q += 1;
        }
        let s: String = line[p + 1..q].iter().map(|&b| b as char).collect(); // latin1
        return parse_date_ms(&s).unwrap_or(0);
    }
    let mut n: i64 = 0;
    while p < line.len() && line[p].is_ascii_digit() {
        n = n.saturating_mul(10).saturating_add((line[p] - b'0') as i64);
        p += 1;
    }
    n
}

/// 从 offset 起读取完整行（以 \n 结尾）。
/// needles 为空时所有行都算命中；命中任一关键字的行以 Line::Hit 回调，其余行以 Line::Skip 回调。
/// 返回新的 offset：只推进到最后一个完整换行之后，半行留到下次。
pub fn read_lines(
    file: &Path,
    offset: u64,
    needles: &[&str],
    marks: &[&str],
    handler: &mut dyn FnMut(Line),
) -> io::Result<ReadResult> {
    let mut f = File::open(file)?;
    let size = f.metadata()?.len();
    if size <= offset {
        return Ok(ReadResult { offset: size, size });
    }
    let keys: Vec<memmem::Finder> = needles.iter().map(|s| memmem::Finder::new(s.as_bytes())).collect();
    let mark_keys: Vec<memmem::Finder> = marks.iter().map(|s| memmem::Finder::new(s.as_bytes())).collect();
    f.seek(SeekFrom::Start(offset))?;
    let mut pos = offset;
    let mut consumed = offset;
    let mut carry: Vec<u8> = Vec::new();
    let mut buf = vec![0u8; CHUNK.min(size - offset) as usize];
    while pos < size {
        let want = buf.len().min((size - pos) as usize);
        let n = f.read(&mut buf[..want])?;
        if n == 0 {
            break;
        }
        pos += n as u64;
        // 上一轮剩下的半行拼在本轮数据前面
        let mut data = std::mem::take(&mut carry);
        data.extend_from_slice(&buf[..n]);
        let mut start = 0usize;
        while let Some(rel) = memchr::memchr(b'\n', &data[start..]) {
            let nl = start + rel;
            let line = &data[start..nl];
            consumed += (nl - start + 1) as u64;
            start = nl + 1;
            if line.len() < 2 {
                continue;
            }
            let ts = extract_ts(line);
            if keys.is_empty() || keys.iter().any(|k| k.find(line).is_some()) {
                handler(Line::Hit(&String::from_utf8_lossy(line), ts));
            } else {
                handler(Line::Skip(ts, mark_keys.iter().any(|k| k.find(line).is_some())));
            }
        }
        carry = data[start..].to_vec();
    }
    Ok(ReadResult { offset: consumed, size })
}
