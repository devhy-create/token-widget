//! SQLite 只读访问：ZCode / OpenCode 等 agent 把用量存在它们自己的 SQLite 库里。
//! 原则：只读打开、不加锁等待太久、不建 -shm 以外的任何文件，绝不影响 agent 自己的读写。
use rusqlite::{Connection, OpenFlags};
use std::path::Path;
use std::time::Duration;

/// 只读打开。agent 正在写库时（WAL 模式）读者不会阻塞，最多等 1.5 秒应对偶发的检查点
pub fn open_ro(path: &Path) -> rusqlite::Result<Connection> {
    let conn = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX)?;
    conn.busy_timeout(Duration::from_millis(1500))?;
    // 双保险：即使误写了 SQL，也不会修改对方的库
    conn.pragma_update(None, "query_only", true)?;
    Ok(conn)
}

/// 表里是否有这些列（agent 升级后表结构可能变化：缺列就当没数据，不报错刷屏）
pub fn has_columns(conn: &Connection, table: &str, cols: &[&str]) -> bool {
    // 表名只来自代码里的常量，不是用户输入
    let Ok(mut st) = conn.prepare(&format!("PRAGMA table_info({table})")) else { return false };
    let Ok(rows) = st.query_map([], |r| r.get::<_, String>(1)) else { return false };
    let have: Vec<String> = rows.flatten().collect();
    cols.iter().all(|c| have.iter().any(|h| h == c))
}

/// 引擎用来判断“库有没有新内容”的探针：返回 (伪大小, 修改时间 ms)。
/// WAL 模式下新数据先写进 -wal 文件，主库文件的大小 / 修改时间可能很久不变，所以取主库和 -wal 里较晚的修改时间；
/// 伪大小直接用这个时间（只增不减），引擎的“大小增长 = 有新内容”判断就能沿用。
pub fn probe(db: &Path) -> Option<(u64, f64)> {
    let mt = |p: &Path| -> Option<f64> {
        let m = std::fs::metadata(p).ok()?.modified().ok()?;
        Some(m.duration_since(std::time::UNIX_EPOCH).ok()?.as_secs_f64() * 1000.0)
    };
    let main = mt(db)?;
    let mut wal = db.as_os_str().to_os_string();
    wal.push("-wal");
    let latest = mt(Path::new(&wal)).map_or(main, |w| w.max(main));
    Some((latest as u64, latest))
}
