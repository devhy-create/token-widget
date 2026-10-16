use agent_core::lines::{extract_ts, read_lines, Line};
use agent_core::util::parse_date_ms;
use std::fs;
use std::io::Write;

#[test]
fn extract_ts_iso_毫秒数字_created_at_缺失() {
    assert_eq!(extract_ts(br#"{"timestamp":"2026-01-02T03:04:05.000Z","x":1}"#), parse_date_ms("2026-01-02T03:04:05.000Z").unwrap());
    assert_eq!(extract_ts(br#"{"timestamp":1767323045000,"x":1}"#), 1767323045000);
    assert_eq!(extract_ts(br#"{"created_at":1700000000000}"#), 1700000000000);
    assert_eq!(extract_ts(br#"{"x":1}"#), 0);
    assert_eq!(extract_ts(br#"{"timestamp":"not-a-date"}"#), 0);
}

#[test]
fn read_lines_只解码命中行_其余走skip_marks标记起点() {
    let dir = tempfile::tempdir().unwrap();
    let f = dir.path().join("a.jsonl");
    fs::write(&f, "{\"timestamp\":1000,\"k\":\"hit\"}\n{\"timestamp\":2000,\"k\":\"skip\"}\n{\"timestamp\":3000,\"k\":\"mark\"}\n").unwrap();
    let (mut hits, mut skips) = (vec![], vec![]);
    let r = read_lines(&f, 0, &["\"hit\""], &["\"mark\""], &mut |l| match l {
        Line::Hit(t, ts) => hits.push((t.to_string(), ts)),
        Line::Skip(ts, m) => skips.push((ts, m)),
    }).unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].1, 1000);
    assert_eq!(skips, vec![(2000, false), (3000, true)]);
    assert_eq!(r.offset, fs::metadata(&f).unwrap().len());
}

#[test]
fn read_lines_增量读取_半行留到下次_不重复不丢失() {
    let dir = tempfile::tempdir().unwrap();
    let f = dir.path().join("b.jsonl");
    fs::write(&f, "{\"timestamp\":1,\"k\":\"hit\",\"n\":1}\n{\"timestamp\":2,\"k\":\"hit\",\"n\":").unwrap();
    let mut got: Vec<i64> = vec![];
    let mut grab = |l: Line| if let Line::Hit(t, _) = l {
        got.push(serde_json::from_str::<serde_json::Value>(t).unwrap()["n"].as_i64().unwrap());
    };
    let r = read_lines(&f, 0, &["\"hit\""], &[], &mut grab).unwrap();
    // offset 只推进到最后一个完整换行之后
    assert_eq!(r.offset, "{\"timestamp\":1,\"k\":\"hit\",\"n\":1}\n".len() as u64);
    fs::OpenOptions::new().append(true).open(&f).unwrap().write_all(b"2}\n{\"timestamp\":3,\"k\":\"hit\",\"n\":3}\n").unwrap();
    let r = read_lines(&f, r.offset, &["\"hit\""], &[], &mut grab).unwrap();
    assert_eq!(r.offset, fs::metadata(&f).unwrap().len());
    // 没有新内容：直接返回，不再回调
    let r2 = read_lines(&f, r.offset, &["\"hit\""], &[], &mut |_| panic!("不应再回调")).unwrap();
    assert_eq!(r2.offset, r.offset);
    assert_eq!(got, vec![1, 2, 3]);
}

#[test]
fn read_lines_多字节字符整行解码不乱码() {
    let dir = tempfile::tempdir().unwrap();
    let f = dir.path().join("c.jsonl");
    fs::write(&f, "{\"timestamp\":1,\"k\":\"hit\",\"s\":\"中文内容\"}\n").unwrap();
    let mut got = vec![];
    read_lines(&f, 0, &["\"hit\""], &[], &mut |l| if let Line::Hit(t, _) = l {
        got.push(serde_json::from_str::<serde_json::Value>(t).unwrap()["s"].as_str().unwrap().to_string());
    }).unwrap();
    assert_eq!(got, vec!["中文内容"]);
}

#[test]
fn read_lines_needles为空时所有行都解码() {
    let dir = tempfile::tempdir().unwrap();
    let f = dir.path().join("d.jsonl");
    fs::write(&f, "{\"timestamp\":1}\n{\"timestamp\":2}\n").unwrap();
    let mut n = 0;
    read_lines(&f, 0, &[], &[], &mut |l| if let Line::Hit(..) = l { n += 1 }).unwrap();
    assert_eq!(n, 2);
}
