//! WorkBuddy 主日志（logs/<日期>/<启动时间>__<哈希>.log）里的 qua.enqueue / qua.start / qua.end 行。
//! 行格式取自本机 WorkBuddy 5.7.6 的真实日志（字段删减）。
mod common;
use agent_core::collectors::FileState;
use agent_core::{Engine, EngineOptions};
use chrono::{Local, TimeZone, Timelike};
use common::{by_name, now, temp_home, write_jsonl, Collect};
use serde_json::{json, Value};
use std::io::Write;
use std::path::{Path, PathBuf};

/// 中文区域设置下的行首时间：“2026年10月16日 下午1:43:39.584”
fn zh(ms: i64) -> String {
    let t = Local.timestamp_millis_opt(ms).unwrap();
    let ap = if t.hour() < 12 { "上午" } else { "下午" };
    format!("{}{}{}", t.format("%Y年%-m月%-d日 "), ap, t.format("%-I:%M:%S%.3f"))
}

fn qua(ts: &str, kind: &str, f: Value) -> String {
    format!("[{ts}] [Info] [pid=30564] [ModelProvider]  [SseMonitor#01a1433c] qua.{kind}: {f}\n")
}

fn end_fields(id: &str, ty: &str, flag: &str, pt: u64, ct: u64, cost: u64) -> Value {
    json!({ "conversation_id": "c1", "message_id": id, "model": "step-5-preview", "request_type": ty,
        "finish_flag": flag, "total_cost_ms": cost.to_string(), "prompt_tokens": pt.to_string(),
        "completion_tokens": ct.to_string(), "think_total_tokens": "532" })
}

fn log_path(home: &Path) -> PathBuf {
    home.join(".workbuddy-ai/logs/2026-10-16/2026-10-16-13-43-33__d578f9e1b363bf70ba9a2ac7091ae78b.log")
}

fn write(file: &Path, body: &str) {
    std::fs::create_dir_all(file.parent().unwrap()).unwrap();
    std::fs::write(file, body).unwrap();
}

fn append(file: &Path, body: &str) {
    std::fs::OpenOptions::new().append(true).open(file).unwrap().write_all(body.as_bytes()).unwrap();
}

#[test]
fn discover_只收主日志_排在jsonl前面() {
    let home = temp_home();
    let root = home.path().join(".workbuddy-ai");
    write(&log_path(home.path()), "");
    write(&root.join("logs/2026-10-16/workbuddyMainThread__6afe.log"), "");
    write(&root.join("logs/2026-10-16/sdk/conversations/e8f9.log"), "");
    write(&root.join("logs/sandbox/20261016/sandbox_center_37244_000.log"), "");
    write_jsonl(&root.join("projects/p/w1.jsonl"), &[json!({})]);
    let files = by_name(home.path(), "workbuddy").discover();
    let names: Vec<String> = files.iter().map(|p| p.file_name().unwrap().to_string_lossy().into_owned()).collect();
    assert_eq!(names, ["2026-10-16-13-43-33__d578f9e1b363bf70ba9a2ac7091ae78b.log", "w1.jsonl"]);
}

#[test]
fn 开始行报生成中_结束行清除_附带请求入账_chat请求和中断请求不入账() {
    let home = temp_home();
    let c = by_name(home.path(), "workbuddy");
    let f = log_path(home.path());
    let n = now();
    let t0 = n - 20_000;
    let start = json!({ "conversation_id": "c1", "message_id": "m1", "model": "step-5-preview", "request_type": "conversation_topic" });
    let body = [
        qua(&zh(t0), "enqueue", start.clone()),
        qua(&zh(t0 + 2000), "start", start),
        qua(&zh(t0 + 15_059), "end", end_fields("m1", "conversation_topic", "done", 888, 1517, 15059)),
        qua(&zh(t0 + 16_000), "end", end_fields("m2", "chat", "done", 35135, 42199, 240503)),
        qua(&zh(t0 + 17_000), "end", end_fields("m3", "chat", "interrupt", 0, 0, 900)),
        // 被截断的半行：等下次补全再读
        "[2026年10月16日 下午1:44:00.000] [Info] qua.start: {\"message_id\":\"m4\"".to_string(),
    ]
    .concat();
    write(&f, &body);
    let mut s = Collect::default();
    let mut st = FileState::default();
    c.scan(&f, &mut st, &mut s, n).unwrap();

    // 排队、开始各报一次“生成中”（时间取行首本地时间，精确到毫秒），结束行清除
    assert_eq!(s.busy.len(), 5);
    assert_eq!(s.busy[0], ("m1".to_string(), t0, Some("step-5-preview".to_string())));
    assert_eq!(s.busy[1].1, t0 + 2000);
    assert_eq!(s.busy[2], ("m1".to_string(), t0 + 15_059, None));
    // 只有标题生成这类附带请求从日志入账；正常结束的 chat 由 jsonl 入账，中断的 token 为 0
    assert_eq!(s.evs.len(), 1);
    let e = &s.evs[0];
    assert_eq!((e.key.as_str(), e.model.as_str(), e.sid.as_str(), e.ts), ("m1", "step-5-preview", "c1", t0 + 15_059));
    assert_eq!([e.input, e.cr, e.out, e.rs], [888, 0, 1517, 0]);
    assert_eq!(e.dur, 15059);
    assert!(st.off < std::fs::metadata(&f).unwrap().len(), "半行不推进 offset");
}

#[test]
fn jsonl用量行取主日志里的精确耗时_没有时回退到距请求起点() {
    let home = temp_home();
    let c = by_name(home.path(), "workbuddy");
    let n = now();
    let lf = log_path(home.path());
    write(&lf, &qua(&zh(n - 5000), "end", end_fields("wm1", "chat", "done", 1000, 100, 240503)));
    let jf = home.path().join(".workbuddy-ai/projects/p/w1.jsonl");
    let usage = |id: &str, ts: i64| json!({ "timestamp": ts, "id": format!("o-{id}"), "sessionId": "ws1",
        "providerData": { "model": "deepseek-v4.1-flash", "messageId": id, "rawUsage": { "prompt_tokens": 1000, "completion_tokens": 100 } } });
    write_jsonl(&jf, &[
        json!({ "timestamp": n - 9000, "message": { "role": "user" } }),
        usage("wm1", n - 4960),
        json!({ "timestamp": n - 4000, "type": "function_call_result" }),
        usage("wm2", n - 1000),
    ]);
    // 同一个采集器实例：先扫主日志、再扫 jsonl（与 discover 的顺序一致）
    let mut s = Collect::default();
    c.scan(&lf, &mut FileState::default(), &mut s, n).unwrap();
    c.scan(&jf, &mut FileState::default(), &mut s, n).unwrap();
    assert_eq!(s.evs.iter().map(|e| (e.key.as_str(), e.dur)).collect::<Vec<_>>(), [("wm1", 240503), ("wm2", 3000)]);
}

#[test]
fn 行首时间兼容美式区域写法() {
    let home = temp_home();
    let c = by_name(home.path(), "workbuddy");
    let f = log_path(home.path());
    write(&f, &qua("10/16/2026, 1:43:39.584 PM", "end", end_fields("m1", "compress", "done", 10, 20, 1000)));
    let mut s = Collect::default();
    c.scan(&f, &mut FileState::default(), &mut s, now()).unwrap();
    let want = Local.with_ymd_and_hms(2026, 10, 16, 13, 43, 39).unwrap().timestamp_millis() + 584;
    assert_eq!(s.evs[0].ts, want);
}

#[test]
fn 引擎快照_请求进行中显示生成中和模型_结束后入账并清除() {
    let home = temp_home();
    let n = now();
    let f = log_path(home.path());
    let start = json!({ "conversation_id": "c1", "message_id": "m1", "model": "step-5-preview", "request_type": "conversation_topic" });
    write(&f, &qua(&zh(n - 8000), "start", start));
    let dir = home.path().join("data");
    let mut eng = Engine::new(EngineOptions { dir: Some(dir), home: Some(home.path().to_path_buf()), fresh: true }).unwrap();
    eng.tick();
    let s = eng.snapshot_now();
    let lv = &s["agents"]["workbuddy"]["live"];
    assert_eq!(lv["active"], true);
    assert_eq!(lv["busy"], n - 8000);
    assert_eq!(lv["model"], "step-5-preview");
    assert_eq!(lv["models"][0]["model"], "step-5-preview");
    assert_eq!(lv["models"][0]["busy"], n - 8000);
    assert_eq!(lv["models"][0]["tps"], Value::Null);
    assert_eq!(s["agents"]["workbuddy"]["found"], true);

    append(&f, &qua(&zh(n - 1000), "end", end_fields("m1", "conversation_topic", "done", 888, 1517, 7000)));
    eng.tick();
    let s = eng.snapshot_now();
    let w = &s["agents"]["workbuddy"];
    assert_eq!(w["live"]["busy"], Value::Null);
    assert_eq!(w["live"]["models"][0].get("busy"), None);
    assert_eq!(w["today"]["tokens"], 888 + 1517);
    // 结束后 20 秒内仍算“生成中”，速度 = 1517 / 7 秒
    assert_eq!(w["live"]["active"], true);
    assert!((w["live"]["lastTps"].as_f64().unwrap() - 1517.0 / 7.0).abs() < 1e-9);
}
