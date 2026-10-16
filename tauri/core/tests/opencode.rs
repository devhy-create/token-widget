//! OpenCode 采集器：用按文档结构现建的 SQLite 样例库测试（1.x 的 message 表 / 2.x 的 session_message 表），不需要安装 OpenCode。
mod common;
use agent_core::collectors::FileState;
use common::{by_name, now, temp_home, Collect};
use rusqlite::Connection;
use std::path::{Path, PathBuf};

fn db_path(home: &Path) -> PathBuf {
    home.join(".local/share/opencode/opencode.db")
}

fn open(home: &Path) -> Connection {
    let f = db_path(home);
    std::fs::create_dir_all(f.parent().unwrap()).unwrap();
    Connection::open(&f).unwrap()
}

fn data(role: &str, model: &str, completed: Option<i64>, tokens: (i64, i64, i64, i64, i64)) -> String {
    let (i, o, r, cr, cw) = tokens;
    let time = completed.map_or(String::new(), |c| format!(r#","time":{{"created":1,"completed":{c}}}"#));
    format!(
        r#"{{"role":"{role}","modelID":"{model}","providerID":"p","cost":0,"tokens":{{"input":{i},"output":{o},"reasoning":{r},"cache":{{"read":{cr},"write":{cw}}}}}{time}}}"#
    )
}

fn v1_db(home: &Path) -> Connection {
    let c = open(home);
    c.execute_batch("CREATE TABLE message (id TEXT PRIMARY KEY, session_id TEXT, time_created INTEGER, time_updated INTEGER, data TEXT);").unwrap();
    c
}

fn put_v1(c: &Connection, id: &str, created: i64, json: &str) {
    c.execute("INSERT INTO message VALUES (?1, 's1', ?2, ?2, ?3)", rusqlite::params![id, created, json]).unwrap();
}

fn scan(home: &Path, st: &mut FileState, now: i64) -> Collect {
    let c = by_name(home, "opencode");
    let mut sink = Collect::default();
    c.scan(&db_path(home), st, &mut sink, now).unwrap();
    sink
}

#[test]
fn opencode_v1_只收助手消息_推理并入输出_时间取完成时间() {
    let home = temp_home();
    let t = now() - 60_000;
    let c = v1_db(home.path());
    put_v1(&c, "u1", t, &data("user", "m", None, (0, 0, 0, 0, 0)));
    put_v1(&c, "a1", t, &data("assistant", "claude-sonnet-5-5", Some(t + 5_000), (1_000, 200, 50, 800, 100)));
    put_v1(&c, "bad", t + 1, "{not json");
    let s = scan(home.path(), &mut FileState::default(), now());
    assert_eq!(s.evs.len(), 1);
    let e = &s.evs[0];
    assert_eq!((e.agent, e.model.as_str(), e.key.as_str(), e.sid.as_str()), ("opencode", "claude-sonnet-5-5", "a1", "s1"));
    assert_eq!([e.input, e.cr, e.cw5, e.out, e.rs], [1_000, 800, 100, 250, 50]);
    assert_eq!((e.ts, e.dur), (t + 5_000, 5_000));
}

#[test]
fn opencode_未写完的消息等完成再入账_超过十分钟当已结束() {
    let home = temp_home();
    let n = now();
    let c = v1_db(home.path());
    put_v1(&c, "running", n - 3_000, &data("assistant", "m", None, (10, 5, 0, 0, 0)));
    put_v1(&c, "stale", n - 20 * 60_000, &data("assistant", "m", None, (10, 5, 0, 0, 0)));
    let mut st = FileState::default();
    let keys: Vec<String> = scan(home.path(), &mut st, n).evs.into_iter().map(|e| e.key).collect();
    assert_eq!(keys, ["stale"]);
    // 写完后补上完成时间，下一轮要能入账
    c.execute("UPDATE message SET data = ?1 WHERE id = 'running'", [data("assistant", "m", Some(n), (10, 50, 0, 0, 0))]).unwrap();
    let keys: Vec<String> = scan(home.path(), &mut st, n + 1_000).evs.into_iter().map(|e| e.key).collect();
    assert!(keys.contains(&"running".to_string()));
}

#[test]
fn opencode_v2_角色在type列_模型在model对象里() {
    let home = temp_home();
    let t = now() - 60_000;
    let c = open(home.path());
    c.execute_batch("CREATE TABLE session_message (id TEXT PRIMARY KEY, session_id TEXT, type TEXT, seq INTEGER, time_created INTEGER, time_updated INTEGER, data TEXT);").unwrap();
    let json = r#"{"model":{"id":"deepseek-v4","providerID":"opencode-go"},"cost":0.1,"tokens":{"input":400,"output":90,"reasoning":10,"cache":{"read":600,"write":0}}}"#;
    c.execute("INSERT INTO session_message VALUES ('m1','s2','assistant',1,?1,?2,?3)", rusqlite::params![t, t + 8_000, json]).unwrap();
    c.execute("INSERT INTO session_message VALUES ('m2','s2','user',2,?1,?1,'{}')", [t]).unwrap();
    let s = scan(home.path(), &mut FileState::default(), now());
    assert_eq!(s.evs.len(), 1);
    let e = &s.evs[0];
    assert_eq!((e.model.as_str(), e.sid.as_str(), e.key.as_str()), ("deepseek-v4", "s2", "m1"));
    assert_eq!([e.input, e.cr, e.out, e.rs], [400, 600, 100, 10]);
    assert_eq!((e.ts, e.dur), (t + 8_000, 8_000));
}

#[test]
fn opencode_发现数据目录里的所有库_没装时为空() {
    let home = temp_home();
    let c = by_name(home.path(), "opencode");
    assert!(c.discover().is_empty());
    let dir = home.path().join(".local/share/opencode");
    std::fs::create_dir_all(&dir).unwrap();
    for n in ["opencode.db", "opencode-dev.db", "opencode.db-wal", "other.db"] {
        std::fs::write(dir.join(n), b"").unwrap();
    }
    let names: Vec<String> = c.discover().iter().map(|p| p.file_name().unwrap().to_string_lossy().into_owned()).collect();
    assert_eq!(names, ["opencode-dev.db", "opencode.db"]);
}
