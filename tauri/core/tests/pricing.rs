use agent_core::pricing::{Bucket, Cost, PriceTable};
use std::fs;

const ZERO: Bucket = Bucket { input: 0.0, cr: 0.0, cw5: 0.0, cw1: 0.0, out: 0.0 };

#[test]
fn 最长前缀匹配_opus_5_5_优先于_opus_5() {
    let t = PriceTable::load(None);
    assert_eq!(t.find("claude-opus-5-5-20260101").unwrap().matcher, "claude-opus-5-5");
    assert_eq!(t.find("claude-opus-5-1").unwrap().matcher, "claude-opus-5");
}

#[test]
fn 大小写不敏感_未知模型返回_none() {
    let t = PriceTable::load(None);
    assert!(t.find("Claude-Sonnet-5-5").is_some());
    assert!(t.find("totally-unknown-model").is_none());
    assert!(t.find("").is_none());
}

#[test]
fn cost_按每百万token计算_缓存写区分5分钟与1小时() {
    let t = PriceTable::load(None);
    // claude-sonnet-5：in 2、out 10、cr 0.2、cw5 2.5、cw1 4
    let b = Bucket { input: 1e6, out: 1e6, cr: 1e6, cw5: 1e6, cw1: 1e6 };
    let c = t.cost("claude-sonnet-5-5", &b);
    assert!((c.usd - (2.0 + 10.0 + 0.2 + 2.5 + 4.0)).abs() < 1e-9);
    assert!(c.known);
    assert!(!c.est);
}

#[test]
fn cost_未知模型known_false_est价格带估价标记() {
    let t = PriceTable::load(None);
    assert_eq!(t.cost("nope", &Bucket { input: 1e6, ..ZERO }), Cost { usd: 0.0, est: false, known: false });
    assert!(t.cost("gpt-6.1-sol", &Bucket { input: 1.0, ..ZERO }).est);
}

#[test]
fn 用户覆盖_同长度前缀优先用户条目_且可追加新模型() {
    let dir = tempfile::tempdir().unwrap();
    let f = dir.path().join("pricing.user.json");
    fs::write(&f, r#"{"models":[
        {"match":"claude-sonnet-5","in":100,"out":0,"cr":0,"cw5":0,"cw1":0},
        {"match":"my-model","in":1,"out":2,"cr":0,"cw5":0,"cw1":0}]}"#).unwrap();
    let t = PriceTable::load(Some(&f));
    assert_eq!(t.cost("claude-sonnet-5-5", &Bucket { input: 1e6, ..ZERO }).usd, 100.0);
    assert_eq!(t.cost("my-model-x", &Bucket { out: 1e6, ..ZERO }).usd, 2.0);
}

#[test]
fn 用户文件损坏时忽略_沿用默认表() {
    let dir = tempfile::tempdir().unwrap();
    let f = dir.path().join("bad.json");
    fs::write(&f, "{ 这不是 json").unwrap();
    let t = PriceTable::load(Some(&f));
    assert_eq!(t.find("claude-sonnet-5-5").unwrap().input, 2.0);
}
