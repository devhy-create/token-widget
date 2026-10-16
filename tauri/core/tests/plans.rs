mod common;
use agent_core::plans::{detect, read_user, write_user};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use serde_json::json;
use std::collections::BTreeMap;
use std::fs;

fn logs(codex: Option<&str>) -> BTreeMap<String, String> {
    codex.map(|c| ("codex".to_string(), c.to_string())).into_iter().collect()
}

#[test]
fn claude_从_claude_json_的_oauth_account_识别_max_5x() {
    let home = common::temp_home();
    let user = home.path().join("plans.user.json");
    fs::write(home.path().join(".claude.json"), json!({ "oauthAccount": { "organizationType": "claude_max", "organizationRateLimitTier": "default_claude_max_5x" } }).to_string()).unwrap();
    let p = detect(home.path(), &user, &logs(None));
    let c = p["claude"].as_ref().unwrap();
    assert_eq!(c.key, "max_5x");
    assert_eq!(c.name, "Max 5x");
    assert_eq!(c.usd, Some(100.0));
    assert_eq!(c.source, "detected");
    assert!(p["antigravity"].is_none());
}

#[test]
fn codex_优先用日志里的_plan_type_价格表里没有的只显示名字() {
    let home = common::temp_home();
    let user = home.path().join("plans.user.json");
    assert_eq!(detect(home.path(), &user, &logs(Some("Plus")))["codex"].as_ref().unwrap().name, "Plus");
    let p = detect(home.path(), &user, &logs(Some("weird_plan")));
    let p = p["codex"].as_ref().unwrap();
    assert_eq!(p.name, "weird_plan");
    assert_eq!(p.usd, None);
}

#[test]
fn codex_日志没有时解码_auth_json_的_id_token_套餐声明() {
    let home = common::temp_home();
    let user = home.path().join("plans.user.json");
    let payload = URL_SAFE_NO_PAD.encode(json!({ "https://api.openai.com/auth": { "chatgpt_plan_type": "pro" } }).to_string());
    fs::create_dir_all(home.path().join(".codex")).unwrap();
    fs::write(home.path().join(".codex").join("auth.json"), json!({ "tokens": { "id_token": format!("x.{payload}.y") } }).to_string()).unwrap();
    assert_eq!(detect(home.path(), &user, &logs(None))["codex"].as_ref().unwrap().key, "pro");
}

#[test]
fn 用户文件覆盖自动识别_usd_非法按0处理() {
    let home = common::temp_home();
    let user = home.path().join("plans.user.json");
    fs::write(home.path().join(".claude.json"), json!({ "oauthAccount": { "organizationType": "claude_pro" } }).to_string()).unwrap();
    write_user(&user, "claude", Some(&json!({ "name": "我的套餐", "usd": "abc" }))).unwrap();
    let p = detect(home.path(), &user, &logs(None));
    let c = p["claude"].as_ref().unwrap();
    assert_eq!(c.source, "user");
    assert_eq!(c.name, "我的套餐");
    assert_eq!(c.usd, Some(0.0));
}

#[test]
fn write_user_读取_保留其它键_传none恢复自动识别_名称截断24字() {
    let home = common::temp_home();
    let user = home.path().join("plans.user.json");
    fs::write(&user, json!({ "other": { "keep": 1 } }).to_string()).unwrap();
    write_user(&user, "workbuddy", Some(&json!({ "name": "x".repeat(50), "usd": 20 }))).unwrap();
    let u = read_user(&user);
    assert_eq!(u["other"], json!({ "keep": 1 }));
    assert_eq!(u["workbuddy"]["name"].as_str().unwrap().chars().count(), 24);
    assert_eq!(u["workbuddy"]["usd"], json!(20.0));
    write_user(&user, "workbuddy", None).unwrap();
    let u = read_user(&user);
    assert!(u.get("workbuddy").is_none());
    assert_eq!(u["other"], json!({ "keep": 1 }));
}

#[test]
fn read_user_文件不存在返回空对象() {
    let home = common::temp_home();
    assert_eq!(read_user(&home.path().join("nope.json")), json!({}));
}
