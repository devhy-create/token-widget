mod common;
use agent_core::{Engine, EngineOptions};
use common::{now, temp_home, write_jsonl};
use serde_json::{json, Value};
use std::fs;
use std::path::Path;

fn engine(home: &Path, name: &str, fresh: bool) -> Engine {
    let dir = home.join(format!("data-{name}"));
    Engine::new(EngineOptions { dir: Some(dir), home: Some(home.to_path_buf()), fresh }).unwrap()
}

fn claude_line(id: &str, ts: i64, out: u64) -> Value {
    let t = chrono::DateTime::from_timestamp_millis(ts).unwrap().format("%Y-%m-%dT%H:%M:%S%.3fZ").to_string();
    json!({ "timestamp": t, "type": "assistant", "sessionId": "s", "message": { "id": id, "model": "claude-sonnet-5-5",
        "usage": { "input_tokens": 1000, "cache_read_input_tokens": 0, "output_tokens": out } } })
}

#[test]
fn tick到snapshot_今日汇总_费用按价格表折算_found标记() {
    let home = temp_home();
    let n = now();
    write_jsonl(&home.path().join(".claude/projects/p/a.jsonl"), &[claude_line("m1", n - 20000, 1000), claude_line("m2", n - 10000, 1000)]);
    let mut eng = engine(home.path(), "a", true);
    eng.tick();
    eng.shutdown(); // 最后一条消息要等 finish 才入账
    let s = eng.snapshot_now();
    let c = &s["agents"]["claude"];
    assert_eq!(c["found"], true);
    assert_eq!(c["today"]["n"], 2);
    assert_eq!(c["today"]["tokens"], 2 * (1000 + 1000));
    // claude-sonnet-5：in 2 / out 10（美元每百万）
    assert!((c["today"]["usd"].as_f64().unwrap() - (2000.0 * 2.0 + 2000.0 * 10.0) / 1e6).abs() < 1e-12);
    assert_eq!(s["agents"]["codex"]["found"], false);
    assert_eq!(s["agents"]["codex"]["today"]["tokens"], 0);
}

#[test]
fn 同一份日志复制两份_按事件键去重_总量不变() {
    let home = temp_home();
    let n = now();
    let lines = [claude_line("d1", n - 30000, 100), claude_line("d2", n - 20000, 100), claude_line("d3", n - 10000, 100)];
    write_jsonl(&home.path().join(".claude/projects/q/one.jsonl"), &lines);
    let mut e1 = engine(home.path(), "b1", true);
    e1.tick();
    e1.shutdown();
    let single = e1.snapshot_now()["agents"]["claude"]["today"]["tokens"].as_u64().unwrap();
    assert!(single > 0);

    write_jsonl(&home.path().join(".claude/projects/q/two.jsonl"), &lines);
    let mut e2 = engine(home.path(), "b2", true);
    e2.tick();
    e2.shutdown();
    assert_eq!(e2.snapshot_now()["agents"]["claude"]["today"]["tokens"].as_u64().unwrap(), single);
}

#[test]
fn 缓存_shutdown落盘后重新加载_数据一致且不重复扫描() {
    let home = temp_home();
    let n = now();
    write_jsonl(&home.path().join(".claude/projects/p/a.jsonl"), &[claude_line("c1", n - 20000, 100), claude_line("c2", n - 10000, 100)]);
    let mut e1 = engine(home.path(), "c", true);
    e1.tick();
    e1.shutdown();
    let t1 = e1.snapshot_now()["agents"]["claude"]["today"]["tokens"].as_u64().unwrap();
    assert!(t1 > 0);
    let mut e2 = engine(home.path(), "c", false);
    e2.tick();
    e2.shutdown();
    assert_eq!(e2.snapshot_now()["agents"]["claude"]["today"]["tokens"].as_u64().unwrap(), t1);
}

#[test]
fn set_external_limits_写入额度窗口_较旧时间戳不覆盖较新的() {
    let home = temp_home();
    let mut eng = engine(home.path(), "d", true);
    eng.set_external_limits("antigravity", &json!({ "primary": { "used_percent": 30, "window_minutes": 300, "resets_at": 1 }, "models": [{ "label": "m", "remaining": 0.7, "resetsAt": 1 }] }));
    let s = eng.snapshot_now();
    let l = &s["agents"]["antigravity"]["limits"];
    assert_eq!(l["primary"]["used_percent"], 30);
    assert_eq!(l["models"].as_array().unwrap().len(), 1);
    eng.set_limits_at("antigravity", now() - 100000, &json!({ "primary": { "used_percent": 99 } }));
    assert_eq!(eng.snapshot_now()["agents"]["antigravity"]["limits"]["primary"]["used_percent"], 30);
}

#[test]
fn read_claude_limits_读取钩子写出的文件_秒与iso两种resets_at() {
    let home = temp_home();
    let mut eng = engine(home.path(), "e", true);
    let reset_sec = now() / 1000 + 3600;
    let iso = chrono::DateTime::from_timestamp(reset_sec + 86400, 0).unwrap().format("%Y-%m-%dT%H:%M:%S%.3fZ").to_string();
    fs::write(eng.dir().join("claude-limits.json"), json!({ "ts": now(), "rate_limits": {
        "five_hour": { "used_percentage": 6, "resets_at": reset_sec },
        "seven_day": { "used_percentage": 28.5, "resets_at": iso },
    } }).to_string()).unwrap();
    eng.read_claude_limits();
    let s = eng.snapshot_now();
    let l = &s["agents"]["claude"]["limits"];
    assert_eq!(l["primary"]["used_percent"], 6.0);
    assert_eq!(l["primary"]["window_minutes"], 300);
    assert_eq!(l["primary"]["resets_at"], reset_sec);
    assert_eq!(l["secondary"]["used_percent"], 28.5);
    assert_eq!(l["secondary"]["window_minutes"], 10080);
    assert_eq!(l["secondary"]["resets_at"], reset_sec + 86400);
}

#[test]
fn read_claude_limits_文件不存在或损坏时不抛错_不产生额度() {
    let home = temp_home();
    let mut eng = engine(home.path(), "f", true);
    eng.read_claude_limits();
    assert!(eng.snapshot_now()["agents"]["claude"]["limits"].is_null());
    fs::write(eng.dir().join("claude-limits.json"), "{坏掉").unwrap();
    eng.read_claude_limits();
    assert!(eng.snapshot_now()["agents"]["claude"]["limits"].is_null());
}

#[test]
fn 实时速度_用事件耗时算tok_s_并按模型拆分() {
    let home = temp_home();
    let n = now();
    let ts = |ms: i64| chrono::DateTime::from_timestamp_millis(n - ms).unwrap().format("%Y-%m-%dT%H:%M:%S%.3fZ").to_string();
    write_jsonl(&home.path().join(".codex/sessions/live.jsonl"), &[
        json!({ "timestamp": ts(12000), "type": "turn_context", "payload": { "model": "gpt-6.1-sol" } }),
        json!({ "timestamp": ts(10000), "type": "response_item", "payload": { "role": "user" } }),
        json!({ "timestamp": ts(5000), "type": "token_usage_record", "ordinal": 1, "payload": { "response_id": "live1", "usage": { "input_tokens": 10, "output_tokens": 500 } } }),
    ]);
    let mut eng = engine(home.path(), "g", true);
    eng.tick();
    let live = eng.snapshot_now()["agents"]["codex"]["live"].clone();
    assert_eq!(live["active"], true);
    assert!((live["tps"].as_f64().unwrap() - 100.0).abs() < 1e-6); // 500 token / 5 秒
    assert_eq!(live["models"].as_array().unwrap().len(), 1);
    assert_eq!(live["models"][0]["model"], "gpt-6.1-sol");
}

#[test]
fn 没装的agent_found为false_首轮发现前booted为false() {
    let home = temp_home();
    // 只装了 Claude
    write_jsonl(&home.path().join(".claude/projects/p/a.jsonl"), &[claude_line("m1", now() - 5_000, 100)]);
    let mut eng = engine(home.path(), "found", true);
    assert!(!eng.booted());
    assert_eq!(eng.snapshot_now()["booted"], json!(false));
    eng.tick();
    assert!(eng.booted());
    let snap = eng.snapshot_now();
    assert_eq!(snap["booted"], json!(true));
    let found = |ag: &str| snap["agents"][ag]["found"].as_bool().unwrap();
    assert!(found("claude"));
    assert!(!found("codex") && !found("antigravity") && !found("workbuddy"));
    assert!(eng.found("claude") && !eng.found("codex"));
}
