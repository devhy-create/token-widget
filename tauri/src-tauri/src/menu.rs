//! 托盘与右键菜单。菜单每次状态变化后重建（勾选状态、显示 / 隐藏文案跟随当前配置）。
use crate::settings::{self, agent_name, AGENT_IDS};
use crate::state::Sh;
use crate::window;
use tauri::menu::{CheckMenuItemBuilder, Menu, MenuBuilder, MenuItemBuilder, SubmenuBuilder};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Manager, Wry};
use tauri_plugin_opener::OpenerExt;

const TRAY_ID: &str = "main";

pub fn build_menu(sh: &Sh) -> tauri::Result<Menu<Wry>> {
    let app = &sh.app;
    let c = sh.cfg().clone();
    let vis = window::visible(sh);
    let mut agents = SubmenuBuilder::new(app, "显示的 Agent");
    // 只列出本机检测到的 agent；一个都没有时放一个置灰的提示
    let found: Vec<&str> = {
        let e = sh.engine();
        AGENT_IDS.iter().copied().filter(|id| e.found(id)).collect()
    };
    for id in &found {
        let it = CheckMenuItemBuilder::with_id(format!("ag:{id}"), agent_name(id)).checked(!c.hidden.contains_key(*id)).build(app)?;
        agents = agents.item(&it);
    }
    if found.is_empty() {
        agents = agents.item(&MenuItemBuilder::with_id("ag-none", "未检测到任何 Agent").enabled(false).build(app)?);
    }
    let agents = agents.build()?;
    let toggle = MenuItemBuilder::with_id("toggle", if vis { "隐藏小组件" } else { "显示小组件" }).build(app)?;
    let auto_hide = CheckMenuItemBuilder::with_id("autohide", "鼠标移开自动收缩到顶部").checked(c.auto_hide).build(app)?;
    let pinned = CheckMenuItemBuilder::with_id("pinned", "固定展开（不自动收缩）").checked(c.pinned).build(app)?;
    let on_top = CheckMenuItemBuilder::with_id("ontop", "始终置顶").checked(c.on_top).build(app)?;
    let autostart = CheckMenuItemBuilder::with_id("autostart", "开机自启").enabled(settings::can_autostart()).checked(settings::get_autostart(sh)).build(app)?;
    let pricing = MenuItemBuilder::with_id("pricing", "打开价格配置（pricing.user.json）").build(app)?;
    let plans = MenuItemBuilder::with_id("plans", "打开套餐配置（plans.user.json）").build(app)?;
    let rescan = MenuItemBuilder::with_id("rescan", "重新全量扫描").build(app)?;
    let quit = MenuItemBuilder::with_id("quit", "退出").build(app)?;
    MenuBuilder::new(app)
        .items(&[&toggle, &auto_hide, &pinned, &on_top, &autostart, &agents])
        .separator()
        .items(&[&pricing, &plans, &rescan])
        .separator()
        .item(&quit)
        .build()
}

/// 配置或可见性变化后，用最新状态重建托盘菜单
pub fn refresh_tray(sh: &Sh) {
    if let (Some(tray), Ok(menu)) = (sh.app.tray_by_id(TRAY_ID), build_menu(sh)) {
        let _ = tray.set_menu(Some(menu));
    }
}

pub fn create_tray(sh: &Sh) -> tauri::Result<()> {
    let icon = tauri::image::Image::from_bytes(include_bytes!("../../../assets/tray.png"))?;
    let menu = build_menu(sh)?;
    let shc = sh.clone();
    TrayIconBuilder::with_id(TRAY_ID)
        .icon(icon)
        .tooltip("Token 小组件")
        .menu(&menu)
        .show_menu_on_left_click(false)
        .on_tray_icon_event(move |_tray, ev| {
            // 左键：显示 / 隐藏切换；右键由系统弹出菜单
            if let TrayIconEvent::Click { button: MouseButton::Left, button_state: MouseButtonState::Up, .. } = ev {
                if window::visible(&shc) { window::hide_window(&shc) } else { window::reveal(&shc) }
            }
        })
        .build(&sh.app)?;
    Ok(())
}

/// 用户配置文件不存在时先写一份模板再打开
fn open_user_file(sh: &Sh, file: std::path::PathBuf, template: serde_json::Value) {
    if !file.exists() {
        let _ = std::fs::write(&file, serde_json::to_string_pretty(&template).unwrap_or_default());
    }
    let _ = sh.app.opener().open_path(file.to_string_lossy(), None::<&str>);
}

pub fn on_menu_event(app: &AppHandle, id: &str) {
    let Some(sh) = app.try_state::<Sh>() else { return };
    let sh: Sh = sh.inner().clone();
    match id {
        "toggle" => {
            if window::visible(&sh) { window::hide_window(&sh) } else { window::show_window(&sh) }
        }
        "autohide" => {
            let on = {
                let mut c = sh.cfg();
                c.auto_hide = !c.auto_hide;
                c.auto_hide
            };
            sh.save_cfg();
            window::send_mode(&sh);
            if on { window::arm_collapse(&sh) } else { window::expand(&sh) }
        }
        "pinned" => {
            let on = {
                let mut c = sh.cfg();
                c.pinned = !c.pinned;
                c.pinned
            };
            sh.save_cfg();
            window::send_mode(&sh);
            if on { window::expand(&sh) } else { window::arm_collapse(&sh) }
        }
        "ontop" => {
            let on = {
                let mut c = sh.cfg();
                c.on_top = !c.on_top;
                c.on_top
            };
            if let Some(w) = window::win(&sh) {
                let _ = w.set_always_on_top(on);
            }
            sh.save_cfg();
        }
        "autostart" => settings::set_autostart(&sh, !settings::get_autostart(&sh)),
        "pricing" => {
            let f = sh.engine().pricing_user_file().to_path_buf();
            open_user_file(&sh, f, serde_json::json!({ "models": [{ "match": "gpt-6.1", "in": 2.5, "out": 15, "cr": 0.25 }] }));
        }
        "plans" => {
            let f = sh.engine().plans_user_file().to_path_buf();
            // 模板里只放 _示例：把它改名成 agent 名（claude / codex / antigravity / workbuddy）才会生效
            open_user_file(&sh, f, serde_json::json!({
                "_说明": "把 _示例 里的条目改成 agent 名即可覆盖自动识别的套餐；usd 为月费（美元）",
                "_示例": { "antigravity": { "name": "AI Pro", "usd": 19.99 }, "workbuddy": { "name": "Pro", "usd": 20 } },
            }));
        }
        "rescan" => {
            let f = sh.engine().cache_file().to_path_buf();
            let _ = std::fs::remove_file(f);
            app.restart();
        }
        "quit" => app.exit(0),
        other if other.starts_with("ag:") => {
            let ag = &other[3..];
            {
                let mut c = sh.cfg();
                if c.hidden.contains_key(ag) { c.hidden.remove(ag); } else { c.hidden.insert(ag.to_string(), true); }
            }
            sh.save_cfg();
            window::send_mode(&sh);
        }
        _ => {}
    }
    refresh_tray(&sh);
}
