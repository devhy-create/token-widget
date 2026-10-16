//! 命令行对账工具：cargo run -p agent_core --example cli -- [--fresh] [--save] [--out <文件>]
//! --fresh 忽略缓存重新全量扫描，数据写到临时目录（不覆盖正式缓存，除非加 --save）。
//! 输出完整快照 JSON（stdout 或 --out 指定的文件），用于和 Electron 版 core/cli.js 对拍。
use agent_core::{Engine, EngineOptions};
use std::time::Instant;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let fresh = args.iter().any(|a| a == "--fresh");
    let save = args.iter().any(|a| a == "--save");
    let out = args.iter().position(|a| a == "--out").and_then(|i| args.get(i + 1)).cloned();
    let dir = (fresh && !save).then(|| std::env::temp_dir().join("token-widget-cli-rs"));
    let mut eng = Engine::new(EngineOptions { dir, home: None, fresh }).expect("初始化引擎失败");
    let t0 = Instant::now();
    eng.tick();
    eprintln!("扫描耗时 {:.2}s", t0.elapsed().as_secs_f64());
    // --now <毫秒>：用指定时刻生成快照，便于和 JS 版同一时刻的快照逐字段对拍
    let now = args.iter().position(|a| a == "--now").and_then(|i| args.get(i + 1)).and_then(|s| s.parse().ok());
    let snap = serde_json::to_string_pretty(&match now {
        Some(n) => eng.snapshot(n),
        None => eng.snapshot_now(),
    }).unwrap();
    match out {
        Some(f) => std::fs::write(f, snap).unwrap(),
        None => println!("{snap}"),
    }
    let _ = eng.save();
}
