//! 测试公共工具（对应 JS 版 test/helpers.js）。
//! Rust 的采集器显式接收 home 目录，不需要改环境变量，所以测试之间可以并行、互不影响。
#![allow(dead_code)]
use agent_core::collectors::{Collector, Event, FileState, Sink};
use serde_json::Value;
use std::path::Path;
use tempfile::TempDir;

pub fn temp_home() -> TempDir {
    tempfile::Builder::new().prefix("tw-test-").tempdir().unwrap()
}

/// 把若干对象写成 jsonl（每行一个 JSON，末尾带换行）
pub fn write_jsonl(file: &Path, objs: &[Value]) {
    std::fs::create_dir_all(file.parent().unwrap()).unwrap();
    let body: String = objs.iter().map(|o| format!("{}\n", o)).collect();
    std::fs::write(file, body).unwrap();
}

#[derive(Default)]
pub struct Collect {
    pub evs: Vec<Event>,
    pub limits: Vec<(String, i64, Value)>,
    /// (键, 时间, 开始时的模型；结束为 None)
    pub busy: Vec<(String, i64, Option<String>)>,
}

impl Sink for Collect {
    fn emit(&mut self, ev: Event) {
        self.evs.push(ev);
    }
    fn set_limits(&mut self, agent: &str, ts: i64, rl: &Value) {
        self.limits.push((agent.to_string(), ts, rl.clone()));
    }
    fn busy(&mut self, _agent: &'static str, key: &str, ts: i64, model: Option<&str>) {
        self.busy.push((key.to_string(), ts, model.map(str::to_string)));
    }
}

/// 扫描一次，返回 (收集器输出, 文件状态)
pub fn collect(c: &dyn Collector, file: &Path, now: i64) -> (Collect, FileState) {
    let mut sink = Collect::default();
    let mut st = FileState::default();
    c.scan(file, &mut st, &mut sink, now).unwrap();
    (sink, st)
}

pub fn by_name(home: &Path, name: &str) -> Box<dyn Collector> {
    agent_core::collectors::all_collectors(home).into_iter().find(|c| c.name() == name).unwrap()
}

pub fn now() -> i64 {
    agent_core::util::now_ms()
}
