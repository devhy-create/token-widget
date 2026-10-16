//! ZCode 采集器：用按 CodeBurn 文档结构现建的 SQLite 样例库测试，不需要安装 ZCode。
mod common;
use agent_core::collectors::FileState;
use common::{by_name, now, temp_home, Collect};
use rusqlite::Connection;
use std::path::{Path, PathBuf};

fn db_path(home: &Path) -> PathBuf {
    home.join(".zcode/cli/db/db.sqlite")
}

/// 建一个带 model_usage 表的样例库（列名与 CodeBurn 文档里的一致）
fn make_db(home: &Path) -> Connection {
    let f = db_path(home);
    std::fs::create_dir_all(f.parent().unwrap()).unwrap();
    let c = Connection::open(&f).unwrap();
    c.execute_batch(
        "CREATE TABLE model_usage (id TEXT PRIMARY KEY, session_id TEXT, turn_id TEXT, model_id TEXT,
           input_tokens INTEGER, output_tokens INTEGER, reasoning_tokens INTEGER,
           cache_creation_input_tokens INTEGER, cache_read_input_tokens INTEGER,
           started_at INTEGER, completed_at INTEGER);",
    ).unwrap();
    c
}

#[allow(clippy::too_many_arguments)]
fn insert(c: &Connection, id: &str, inp: i64, out: i64, cw: i64, cr: i64, started: i64, completed: Option<i64>) {
    c.execute(
        "INSERT INTO model_usage VALUES (?1, 's1', 't1', 'GLM-5.2', ?2, ?3, 5, ?4, ?5, ?6, ?7)",
        rusqlite::params![id, inp, out, cw, cr, started, completed],
    ).unwrap();
}

fn scan(home: &Path, st: &mut FileState, now: i64) -> Collect {
    let c = by_name(home, "zcode");
    let mut sink = Collect::default();
    c.scan(&db_path(home), st, &mut sink, now).unwrap();
    sink
}

#[test]
fn zcode_input含缓存要减掉_时间取完成时间_耗时为完成减开始() {
    let home = temp_home();
    let t = now() - 60_000;
    let db = make_db(home.path());
    // input_tokens=10000 里包含缓存读 6000、缓存写 1000
    insert(&db, "r1", 10_000, 300, 1_000, 6_000, t, Some(t + 4_000));
    let s = scan(home.path(), &mut FileState::default(), now());
    assert_eq!(s.evs.len(), 1);
    let e = &s.evs[0];
    assert_eq!((e.agent, e.model.as_str(), e.key.as_str(), e.sid.as_str()), ("zcode", "GLM-5.2", "r1", "s1"));
    assert_eq!([e.input, e.cr, e.cw5, e.out, e.rs], [3_000, 6_000, 1_000, 300, 5]);
    assert_eq!((e.ts, e.dur), (t + 4_000, 4_000));
}

#[test]
fn zcode_增量只读新行_未完成的请求等十分钟后才入账() {
    let home = temp_home();
    let n = now();
    let db = make_db(home.path());
    insert(&db, "old", 100, 10, 0, 0, n - 3_600_000, Some(n - 3_599_000));
    let mut st = FileState::default();
    assert_eq!(scan(home.path(), &mut st, n).evs.len(), 1);
    // 新增一条已完成的、一条刚开始还没完成的、一条 20 分钟前开始一直没完成的
    insert(&db, "new", 100, 10, 0, 0, n - 5_000, Some(n - 1_000));
    insert(&db, "running", 100, 10, 0, 0, n - 2_000, None);
    insert(&db, "stale", 100, 10, 0, 0, n - 20 * 60_000, None);
    let keys: Vec<String> = scan(home.path(), &mut st, n).evs.into_iter().map(|e| e.key).collect();
    // 回看窗口内会重复读到 old（由引擎的去重挡掉），这里只确认新数据都在、running 不在
    assert!(keys.contains(&"new".to_string()) && keys.contains(&"stale".to_string()));
    assert!(!keys.contains(&"running".to_string()));
    // running 完成后补上完成时间，下一轮要能入账
    db.execute("UPDATE model_usage SET completed_at = ?1 WHERE id = 'running'", [n + 500]).unwrap();
    let keys: Vec<String> = scan(home.path(), &mut st, n + 1_000).evs.into_iter().map(|e| e.key).collect();
    assert!(keys.contains(&"running".to_string()));
}

#[test]
fn zcode_表结构变了或没有该库_不报错也不产出() {
    let home = temp_home();
    let c = by_name(home.path(), "zcode");
    assert!(c.discover().is_empty());
    // 有库但缺列（模拟 agent 升级后改了表）
    let f = db_path(home.path());
    std::fs::create_dir_all(f.parent().unwrap()).unwrap();
    Connection::open(&f).unwrap().execute_batch("CREATE TABLE model_usage (id TEXT, tokens INTEGER);").unwrap();
    assert_eq!(c.discover(), vec![f]);
    assert!(scan(home.path(), &mut FileState::default(), now()).evs.is_empty());
}

#[test]
fn zcode_只读打开不会改动对方的库() {
    let home = temp_home();
    let db = make_db(home.path());
    insert(&db, "r1", 100, 10, 0, 0, now() - 5_000, Some(now() - 1_000));
    drop(db);
    let conn = agent_core::sqlite::open_ro(&db_path(home.path())).unwrap();
    assert!(conn.execute("DELETE FROM model_usage", []).is_err());
}
