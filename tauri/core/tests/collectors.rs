mod common;
use agent_core::collectors::FileState;
use common::{by_name, collect, temp_home, write_jsonl, Collect};
use serde_json::{json, Value};
use std::io::Write;
use std::path::PathBuf;

const T0: i64 = 1_772_359_200_000; // 2026-03-01T10:00:00.000Z
fn iso(ms: i64) -> String {
    chrono::DateTime::from_timestamp_millis(T0 + ms).unwrap().format("%Y-%m-%dT%H:%M:%S%.3fZ").to_string()
}

#[test]
fn claude_同一消息多行合并取最大值_out等于output加thinking_缓存写按5m_1h拆分() {
    let home = temp_home();
    let c = by_name(home.path(), "claude");
    let f = home.path().join(".claude/projects/p1/s1.jsonl");
    let usage = |out: u64, think: u64| json!({
        "input_tokens": 10, "cache_read_input_tokens": 1000, "cache_creation_input_tokens": 140,
        "cache_creation": { "ephemeral_5m_input_tokens": 100, "ephemeral_1h_input_tokens": 40 },
        "output_tokens": out, "output_tokens_details": { "thinking_tokens": think },
    });
    write_jsonl(&f, &[
        json!({ "timestamp": iso(0), "type": "user", "sessionId": "s1", "message": { "role": "user", "content": "hi" } }),
        // 同一条消息被拆成两行写入：第一行 output 还没写全
        json!({ "timestamp": iso(1000), "type": "assistant", "sessionId": "s1", "message": { "id": "m1", "model": "claude-sonnet-5-5", "usage": usage(5, 0) } }),
        json!({ "timestamp": iso(2000), "type": "assistant", "sessionId": "s1", "message": { "id": "m1", "model": "claude-sonnet-5-5", "usage": usage(50, 7) } }),
        json!({ "timestamp": iso(5000), "type": "assistant", "sessionId": "s1", "message": { "id": "m2", "model": "claude-sonnet-5-5", "usage": { "input_tokens": 1, "output_tokens": 2 } } }),
        // 合成消息与非 assistant 行不入账
        json!({ "timestamp": iso(6000), "type": "assistant", "message": { "id": "m3", "model": "<synthetic>", "usage": { "input_tokens": 99, "output_tokens": 99 } } }),
    ]);
    let (mut s, mut st) = collect(c.as_ref(), &f, common::now());
    // m2 出现时 m1 才被 flush；m2 要等 4 秒静默或 finish
    assert_eq!(s.evs.len(), 1);
    let e = &s.evs[0];
    assert_eq!((e.key.as_str(), e.model.as_str(), e.sid.as_str()), ("m1", "claude-sonnet-5-5", "s1"));
    assert_eq!([e.input, e.cr, e.cw5, e.cw1, e.out, e.rs], [10, 1000, 100, 40, 57, 7]);
    assert_eq!(e.ts, T0 + 2000);
    assert_eq!(e.dur, 2000); // 从上一行（用户消息）到消息最后一行
    c.finish(&mut st, &mut s);
    assert_eq!(s.evs.len(), 2);
    assert_eq!(s.evs[1].key, "m2");
    assert_eq!(s.evs[1].out, 2);
}

#[test]
fn claude_没有5m_1h拆分时整体按5分钟缓存写_4秒静默后入账() {
    let home = temp_home();
    let c = by_name(home.path(), "claude");
    let f = home.path().join(".claude/projects/p1/s2.jsonl");
    write_jsonl(&f, &[json!({ "timestamp": iso(0), "type": "assistant", "message": { "id": "a1", "model": "claude-opus-5-5", "usage": { "input_tokens": 1, "cache_creation_input_tokens": 80, "output_tokens": 3 } } })]);
    let (mut s, mut st) = (Collect::default(), FileState::default());
    let now = common::now();
    c.scan(&f, &mut st, &mut s, now).unwrap();
    assert_eq!(s.evs.len(), 0); // 刚写入，可能还在追加
    c.scan(&f, &mut st, &mut s, now + 5000).unwrap();
    assert_eq!(s.evs.len(), 1);
    assert_eq!([s.evs[0].cw5, s.evs[0].cw1], [80, 0]);
}

#[test]
fn claude_增量读取不会重复入账() {
    let home = temp_home();
    let c = by_name(home.path(), "claude");
    let f = home.path().join(".claude/projects/p1/s3.jsonl");
    let line = |id: &str, t: i64| json!({ "timestamp": iso(t), "type": "assistant", "message": { "id": id, "model": "claude-sonnet-5-5", "usage": { "input_tokens": 1, "output_tokens": 1 } } });
    write_jsonl(&f, &[line("x1", 0)]);
    let (mut s, mut st) = (Collect::default(), FileState::default());
    let now = common::now();
    c.scan(&f, &mut st, &mut s, now).unwrap();
    std::fs::OpenOptions::new().append(true).open(&f).unwrap().write_all(format!("{}\n", line("x2", 1000)).as_bytes()).unwrap();
    c.scan(&f, &mut st, &mut s, now).unwrap();
    c.finish(&mut st, &mut s);
    assert_eq!(s.evs.iter().map(|e| e.key.as_str()).collect::<Vec<_>>(), ["x1", "x2"]);
}

#[test]
fn claude_discover_递归找到_projects_下的_jsonl() {
    let home = temp_home();
    write_jsonl(&home.path().join(".claude/projects/p1/deep/s1.jsonl"), &[json!({ "a": 1 })]);
    let c = by_name(home.path(), "claude");
    assert!(c.discover().iter().any(|p| p.to_string_lossy().ends_with("s1.jsonl")));
}

#[test]
fn codex_turn_context取模型_token_usage_record入账_rate_limits交给_set_limits() {
    let home = temp_home();
    let c = by_name(home.path(), "codex");
    let f = home.path().join(".codex/sessions/2026/03/01/rollout.jsonl");
    write_jsonl(&f, &[
        json!({ "timestamp": iso(0), "type": "turn_context", "payload": { "model": "gpt-6.1-sol" } }),
        json!({ "timestamp": iso(1000), "type": "response_item", "payload": { "role": "user", "content": "x" } }),
        json!({ "timestamp": iso(4000), "type": "token_usage_record", "ordinal": 1, "payload": { "response_id": "r1", "session_id": "cs1", "usage": { "input_tokens": 1000, "cached_input_tokens": 800, "output_tokens": 200, "reasoning_output_tokens": 50 } } }),
        json!({ "timestamp": iso(5000), "type": "event_msg", "payload": { "type": "token_count", "rate_limits": { "plan_type": "plus", "primary": { "used_percent": 12, "window_minutes": 300, "resets_at": 1 } } } }),
    ]);
    let (s, _) = collect(c.as_ref(), &f, common::now());
    assert_eq!(s.evs.len(), 1);
    let e = &s.evs[0];
    assert_eq!((e.model.as_str(), e.sid.as_str(), e.key.as_str()), ("gpt-6.1-sol", "cs1", "r1"));
    assert_eq!([e.input, e.cr, e.out, e.rs], [200, 800, 200, 50]); // in = 总输入 - 缓存命中
    assert_eq!(e.dur, 3000); // 从最近一次“用户输入”起算
    assert_eq!(s.limits.len(), 1);
    assert_eq!(s.limits[0].0, "codex");
    assert_eq!(s.limits[0].2["plan_type"], "plus");
    assert_eq!(s.limits[0].2["primary"]["used_percent"], 12);
}

#[test]
fn workbuddy_provider_data_raw_usage_归一化() {
    let home = temp_home();
    let c = by_name(home.path(), "workbuddy");
    let f = home.path().join(".workbuddy-ai/projects/p/w1.jsonl");
    write_jsonl(&f, &[
        json!({ "timestamp": T0, "sessionId": "ws1", "message": { "role": "user" } }),
        // role:user 行（上面）为请求起点，下一行才是用量
        json!({ "timestamp": T0 + 2500, "id": "o1", "sessionId": "ws1", "providerData": { "model": "deepseek-v4", "messageId": "wm1", "rawUsage": {
            "prompt_tokens": 1000, "prompt_cache_hit_tokens": 600, "completion_tokens": 100, "completion_tokens_details": { "reasoning_tokens": 30 } } } }),
    ]);
    let (s, _) = collect(c.as_ref(), &f, common::now());
    assert_eq!(s.evs.len(), 1);
    let e = &s.evs[0];
    assert_eq!((e.model.as_str(), e.key.as_str(), e.sid.as_str()), ("deepseek-v4", "wm1", "ws1"));
    assert_eq!([e.input, e.cr, e.cw5, e.out, e.rs], [400, 600, 0, 100, 30]);
}

#[test]
fn antigravity_input_tokens行入账_model_selection切换模型_不同会话事件键不撞车() {
    let home = temp_home();
    let c = by_name(home.path(), "antigravity");
    let mk = |id: &str| -> PathBuf { home.path().join(".gemini/antigravity/brain").join(id).join(".system_generated/logs/transcript_full.jsonl") };
    let body = || -> Vec<Value> { vec![
        json!({ "timestamp": iso(0), "type": "USER", "content": "Model Selection` from Gemini 3 Pro to Gemini 3 Flash." }),
        json!({ "timestamp": iso(3000), "step_index": 7, "input_tokens": 5000, "cache_read_tokens": 4000, "output_tokens": 300 }),
        // 后续步骤的开始 = 上一次模型调用的结束
        json!({ "timestamp": iso(13000), "step_index": 8, "type": "GENERIC" }),
    ] };
    write_jsonl(&mk("conv-a"), &body());
    write_jsonl(&mk("conv-b"), &body());
    let a = collect(c.as_ref(), &mk("conv-a"), common::now()).0.evs;
    let b = collect(c.as_ref(), &mk("conv-b"), common::now()).0.evs;
    assert_eq!(a.len(), 1);
    assert_eq!(a[0].model, "Gemini 3 Flash");
    assert_eq!([a[0].input, a[0].cr, a[0].out], [1000, 4000, 300]);
    assert_eq!(a[0].sid, "conv-a");
    assert_eq!(a[0].key, "conv-a:7");
    // 回归：created_at 是步骤开始时间，事件时间取下一行（结束），耗时 = 结束 - 开始 = 10 秒，
    // 否则长步骤落盘时事件已过旧、实时速度显示不出来，且耗时被误判成 1 秒（速度虚高）
    assert_eq!((a[0].ts, a[0].dur), (T0 + 13000, 10000));
    // 回归：两个会话同一 step_index 的键必须不同，否则会被引擎去重吞掉
    assert_ne!(a[0].key, b[0].key);
    let mut ids: Vec<String> = c.discover().iter()
        .map(|p| p.parent().unwrap().parent().unwrap().parent().unwrap().file_name().unwrap().to_string_lossy().into_owned())
        .collect();
    ids.sort();
    assert_eq!(ids, ["conv-a", "conv-b"]);
}
