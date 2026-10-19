//! 设置面板的后端：读写用户配置、套餐覆盖、开机自启、Claude 额度钩子。
use crate::state::Sh;
use crate::window;
use agent_core::plans;
use agent_core::util::{home_dir, js_number_or_zero};
use serde_json::{json, Map, Value};
use std::path::{Path, PathBuf};
use tauri::Theme;
use tauri_plugin_autostart::ManagerExt;

pub const AGENT_IDS: [&str; 6] = ["claude", "codex", "antigravity", "workbuddy", "zcode", "opencode"];

pub fn agent_name(id: &str) -> &'static str {
    match id {
        "claude" => "Claude",
        "codex" => "Codex",
        "antigravity" => "Antigravity",
        "workbuddy" => "WorkBuddy",
        "zcode" => "ZCode",
        "opencode" => "OpenCode",
        _ => "",
    }
}

// ---- 开机自启 ----
// 开发构建（debug）里 current_exe 是 target/debug 下的临时产物，注册进去没有意义，所以只允许发布构建使用。
pub fn can_autostart() -> bool {
    !cfg!(debug_assertions)
}

pub fn get_autostart(sh: &Sh) -> bool {
    can_autostart() && sh.app.autolaunch().is_enabled().unwrap_or(false)
}

pub fn set_autostart(sh: &Sh, on: bool) {
    if !can_autostart() {
        return;
    }
    let al = sh.app.autolaunch();
    let _ = if on { al.enable() } else { al.disable() };
}

// ---- 外观 ----
pub fn apply_theme(sh: &Sh, theme: &str) {
    let Some(w) = window::win(sh) else { return };
    // system 跟随系统；light / dark 固定
    let t = match theme {
        "light" => Some(Theme::Light),
        "dark" => Some(Theme::Dark),
        _ => None,
    };
    let _ = w.set_theme(t);
}

// ---- Claude 额度钩子：由设置面板一键启用 / 停用 ----
// 钩子就是本程序自己的 `--statusline` 子命令，不依赖 Node。命令写进 ~/.claude/settings.json 的 statusLine。
// 只在没有别人的 statusLine 时才写；每次改动前都先备份。
fn claude_settings_path() -> PathBuf {
    // 环境变量仅用于测试
    std::env::var_os("TW_CLAUDE_SETTINGS").map(PathBuf::from).unwrap_or_else(|| home_dir().join(".claude").join("settings.json"))
}

enum Read {
    Ok(Map<String, Value>),
    Unreadable,
}

fn read_claude_settings(f: &Path) -> Read {
    match std::fs::read_to_string(f) {
        Ok(s) => match serde_json::from_str::<Value>(&s) {
            Ok(Value::Object(m)) => Read::Ok(m),
            _ => Read::Unreadable, // 文件存在但解析失败：不碰
        },
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Read::Ok(Map::new()),
        Err(_) => Read::Unreadable,
    }
}

/// 是不是本小组件的钩子（本程序的 --statusline，或旧版 v0.1 写入的 node 脚本 claude-statusline.js）
fn is_ours(command: &str) -> bool {
    command.contains("claude-statusline.js") || (command.contains("--statusline") && command.contains("agent-monitor"))
}

/// 命令里引号包起来的 .js / .exe 路径是否有不存在的：旧版 v0.1 的脚本被删（如移除 Electron 版后）
/// 或程序挪了位置，钩子就一直调不起来，Claude 额度会停在旧值
fn command_target_missing(command: &str) -> bool {
    // 引号把命令切成奇偶段，奇数段是被引号包住的内容
    command.split('"').skip(1).step_by(2).any(|seg| {
        let s = seg.to_ascii_lowercase();
        (s.ends_with(".js") || s.ends_with(".exe")) && !Path::new(seg).exists()
    })
}

fn hook_state() -> Value {
    match read_claude_settings(&claude_settings_path()) {
        Read::Unreadable => json!({ "state": "unreadable" }),
        Read::Ok(m) => match m.get("statusLine").filter(|v| !v.is_null()) {
            None => json!({ "state": "none" }),
            Some(sl) => {
                let cmd = sl.get("command").and_then(Value::as_str).unwrap_or("");
                let state = if !is_ours(cmd) { "other" } else if command_target_missing(cmd) { "stale" } else { "ours" };
                json!({ "state": state })
            }
        },
    }
}

fn backup_claude_settings(f: &Path) {
    if !f.exists() {
        return;
    }
    let stamp = chrono::Local::now().format("%Y%m%d-%H%M%S");
    let mut name = f.as_os_str().to_owned();
    name.push(format!(".bak-{stamp}"));
    let _ = std::fs::copy(f, PathBuf::from(name));
}

pub fn set_claude_hook(on: bool) -> Value {
    let f = claude_settings_path();
    let st = hook_state();
    let state = st["state"].as_str().unwrap_or("");
    if state == "unreadable" || state == "other" {
        return st; // 不覆盖用户自己的 statusLine
    }
    let Read::Ok(mut json) = read_claude_settings(&f) else { return st };
    if on {
        let Ok(exe) = std::env::current_exe() else { return st };
        let exe = exe.to_string_lossy().replace('\\', "/");
        backup_claude_settings(&f);
        json.insert("statusLine".into(), json!({ "type": "command", "command": format!("\"{exe}\" --statusline") }));
    } else {
        if state != "ours" && state != "stale" {
            return st;
        }
        backup_claude_settings(&f);
        json.shift_remove("statusLine");
    }
    if let Some(dir) = f.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    if let Ok(body) = serde_json::to_string_pretty(&Value::Object(json)) {
        let _ = std::fs::write(&f, body);
    }
    hook_state()
}

// ---- 设置页数据 ----
pub fn get_settings(sh: &Sh) -> Value {
    let (plans_user, log_plans, home, found) = {
        let e = sh.engine();
        let found: Vec<bool> = AGENT_IDS.iter().map(|id| e.found(id)).collect();
        (e.plans_user_file().to_path_buf(), e.log_plans().clone(), e.home().to_path_buf(), found)
    };
    let user = plans::read_user(&plans_user);
    // 传一个不存在的路径 = 不含用户覆盖，只看自动识别结果
    let detected = plans::detect(&home, Path::new(""), &log_plans);
    let defaults = plans::defaults();
    let c = sh.cfg().clone();
    let agents: Vec<Value> = AGENT_IDS.iter().enumerate().map(|(i, id)| {
        let u = user.get(*id).filter(|u| u.get("name").and_then(Value::as_str).is_some_and(|n| !n.is_empty()));
        let presets: Vec<Value> = defaults.get(*id).and_then(Value::as_object).map(|o| {
            o.iter().map(|(key, v)| json!({
                "key": key, "name": v.get("name"), "usd": v.get("usd"), "est": v.get("est").and_then(Value::as_bool).unwrap_or(false),
            })).collect()
        }).unwrap_or_default();
        json!({
            "id": id, "name": agent_name(id),
            // 本机有没有这个 agent 的数据：没有的不在设置页列出
            "found": found[i],
            "user": u.map(|u| json!({ "name": u.get("name"), "usd": js_number_or_zero(u.get("usd")) })),
            "detected": detected.get(*id).and_then(|p| p.as_ref()).map(|p| json!({ "name": p.name, "usd": p.usd })),
            "presets": presets,
        })
    }).collect();
    json!({
        "cfg": {
            "autoHide": c.auto_hide, "onTop": c.on_top, "theme": c.theme, "agQuota": c.ag_quota,
            // canBlur：系统是否支持毛玻璃（不支持时设置页隐藏该行）
            "blur": c.blur, "canBlur": crate::fx::supported(),
            "openAtLogin": get_autostart(sh), "canAutostart": can_autostart(),
        },
        "hidden": c.hidden,
        "claudeHook": hook_state(),
        "agents": agents,
    })
}

pub fn set_settings(sh: &Sh, patch: &Value) -> Value {
    let c = patch.get("cfg").cloned().unwrap_or(Value::Null);
    let b = |k: &str| c.get(k).and_then(Value::as_bool);
    if let Some(v) = b("onTop") {
        sh.cfg().on_top = v;
        if let Some(w) = window::win(sh) {
            let _ = w.set_always_on_top(v);
        }
    }
    if let Some(v) = b("blur") {
        sh.cfg().blur = v;
        window::set_blur(sh, v);
    }
    if let Some(v) = b("agQuota") {
        sh.cfg().ag_quota = v;
    }
    if let Some(t) = c.get("theme").and_then(Value::as_str).filter(|t| ["system", "light", "dark"].contains(t)) {
        sh.cfg().theme = t.to_string();
        apply_theme(sh, t);
    }
    if let Some(v) = b("openAtLogin") {
        set_autostart(sh, v);
    }
    if let Some(v) = b("autoHide") {
        let changed = {
            let mut cfg = sh.cfg();
            let ch = cfg.auto_hide != v;
            cfg.auto_hide = v;
            ch
        };
        if changed {
            if v { window::arm_collapse(sh) } else { window::expand(sh) }
        }
    }
    if let Some(h) = patch.get("hidden").and_then(Value::as_object) {
        let mut cfg = sh.cfg();
        for (id, v) in h {
            if !AGENT_IDS.contains(&id.as_str()) {
                continue; // 只接受已知 agent，避免写入任意键
            }
            if v.as_bool().unwrap_or(false) { cfg.hidden.insert(id.clone(), true); } else { cfg.hidden.remove(id); }
        }
    }
    sh.save_cfg();
    if let Some(p) = patch.get("plans").and_then(Value::as_object) {
        let file = sh.engine().plans_user_file().to_path_buf();
        for (id, entry) in p {
            if !AGENT_IDS.contains(&id.as_str()) {
                continue;
            }
            // 写失败：界面下次打开会显示真实状态
            let _ = plans::write_user(&file, id, if entry.is_null() { None } else { Some(entry) });
        }
        sh.engine().refresh_plans_now();
    }
    window::send_mode(sh);
    window::push(sh);
    crate::menu::refresh_tray(sh);
    get_settings(sh)
}

#[cfg(test)]
mod hook_tests {
    use super::*;

    #[test]
    fn missing_target_is_detected() {
        // 旧版 v0.1 的 node 脚本已被删：钩子失效
        assert!(command_target_missing("node \"Z:/not/exist/claude-statusline.js\""));
        assert!(command_target_missing("\"Z:/not/exist/agent-monitor.exe\" --statusline"));
        // 文件存在（用测试进程自己的 exe）：正常
        let exe = std::env::current_exe().unwrap().to_string_lossy().replace('\\', "/");
        assert!(!command_target_missing(&format!("\"{exe}\" --statusline")));
        // 没有引号包住的路径：不判断
        assert!(!command_target_missing("agent-monitor --statusline"));
    }
}
