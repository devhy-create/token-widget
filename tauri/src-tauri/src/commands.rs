//! 前端（renderer）通过 invoke 调用的命令，一一对应 Electron 版 preload.js 暴露的 window.widget 接口。
use crate::state::{Dock, Mode, Rect, Sh};
use crate::{icons, menu, settings, window};
use serde_json::Value;
use tauri::menu::ContextMenu;
use tauri::State;

/// 界面首次加载完成：显示窗口、同步模式、开始自动收缩计时
#[tauri::command]
pub fn ready(sh: State<'_, Sh>) {
    let sh: Sh = sh.inner().clone();
    if let Some(w) = window::win(&sh) {
        // 测试开关 TW_TEXT_SCALE：用页面缩放模拟 WebView2 在系统文本大小 > 100% 时的放大
        if let Some(t) = window::simulated_text_scale() {
            let _ = w.set_zoom(t);
        }
        let _ = w.show();
        crate::fx::restyle(&w);
    }
    window::send_mode(&sh);
    window::arm_collapse(&sh);
    window::push(&sh);
    menu::refresh_tray(&sh);
}

/// 渲染端上报卡片内容高度；展开态下窗口缓动到新高度（手风琴展开时与 CSS 动画同步）
#[tauri::command]
pub fn resize(sh: State<'_, Sh>, h: f64) {
    let sh: Sh = sh.inner().clone();
    let Some(w) = window::win(&sh) else { return };
    let wa = window::work_area_of(&w, &window::bounds(&w));
    // h 是 CSS 像素；上限按文本缩放折算，保证窗口不超出工作区
    let exp_h = h.round().min(window::max_card_h(&wa)).max(120.0);
    let (mode, anim_to) = {
        let mut ws = sh.ws();
        ws.exp_h = exp_h;
        (ws.mode, ws.anim_to)
    };
    if mode != Mode::Expanded {
        return;
    }
    let b = anim_to.unwrap_or_else(|| window::bounds(&w));
    let s = window::win_size(window::CARD_W, exp_h);
    // 贴着屏幕下沿时向上长，避免内容撑出屏幕
    let y = b.y.min(wa.y + wa.h - s.1 + window::margin() - 4.0).max(wa.y - window::margin() + 4.0);
    window::tween(&sh, Rect { x: b.x, y, w: s.0, h: s.1 });
}

/// 竖向胶囊高度随显示的 agent 数量变化：收缩态且贴左 / 右边时，平滑调整高度
#[tauri::command]
pub fn pillsize(sh: State<'_, Sh>, h: f64) {
    let sh: Sh = sh.inner().clone();
    let Some(w) = window::win(&sh) else { return };
    let (mode, dock, pos) = {
        let mut ws = sh.ws();
        ws.v_pill_h = h.round().clamp(80.0, 600.0);
        (ws.mode, ws.dock, ws.pill_pos)
    };
    if mode != Mode::Collapsed || !matches!(dock, Some(Dock::Left | Dock::Right)) {
        return;
    }
    let wa = window::work_area_of(&w, &window::bounds(&w));
    let vh = sh.ws().v_pill_h;
    let s = window::win_size(window::VPILL_W, vh);
    let y = pos.1.min(wa.y + wa.h - s.1 + window::margin() - 6.0).max(wa.y - window::margin() + 6.0);
    sh.ws().pill_pos = (pos.0, y);
    window::tween(&sh, Rect { x: pos.0, y, w: s.0, h: s.1 });
}

#[tauri::command]
pub fn hover(sh: State<'_, Sh>, inside: bool) {
    #[cfg(debug_assertions)]
    if crate::debug::NO_HOVER.load(std::sync::atomic::Ordering::Relaxed) {
        return;
    }
    let sh: Sh = sh.inner().clone();
    if inside {
        window::cancel_collapse(&sh);
        if sh.ws().mode == Mode::Collapsed {
            window::expand(&sh);
        }
    } else {
        window::arm_collapse(&sh);
    }
}

/// 图标：从本机应用提取并缓存（放在异步线程，不阻塞界面）
#[tauri::command]
pub async fn icons(sh: State<'_, Sh>) -> Result<Value, String> {
    let dir = sh.engine().dir().join("icons");
    tauri::async_runtime::spawn_blocking(move || icons::resolve_icons(&dir, &settings::AGENT_IDS))
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub fn settings_get(sh: State<'_, Sh>) -> Value {
    settings::get_settings(sh.inner())
}

#[tauri::command]
pub fn settings_set(sh: State<'_, Sh>, patch: Value) -> Value {
    settings::set_settings(sh.inner(), &patch)
}

#[tauri::command]
pub fn claudehook_set(sh: State<'_, Sh>, on: bool) -> Value {
    settings::set_claude_hook(on);
    settings::get_settings(sh.inner())
}

#[tauri::command]
pub fn lock(sh: State<'_, Sh>, v: bool) {
    let sh: Sh = sh.inner().clone();
    sh.ws().locked = v;
    if v { window::cancel_collapse(&sh) } else { window::arm_collapse(&sh) }
}

#[tauri::command]
pub fn pin(sh: State<'_, Sh>) {
    let sh: Sh = sh.inner().clone();
    let on = {
        let mut c = sh.cfg();
        c.pinned = !c.pinned;
        c.pinned
    };
    sh.save_cfg();
    window::send_mode(&sh);
    if on { window::cancel_collapse(&sh) } else { window::arm_collapse(&sh) }
    menu::refresh_tray(&sh);
}

#[tauri::command]
pub fn menu(sh: State<'_, Sh>) {
    let sh: Sh = sh.inner().clone();
    if let (Some(w), Ok(m)) = (window::win(&sh), menu::build_menu(&sh)) {
        let _ = m.popup(w.as_ref().window());
    }
}

#[tauri::command]
pub fn close(sh: State<'_, Sh>) {
    window::hide_window(sh.inner());
}

/// 按住标题栏拖动窗口（无边框窗口没有系统标题栏）
#[tauri::command]
pub fn start_drag(sh: State<'_, Sh>) {
    if let Some(w) = window::win(sh.inner()) {
        let _ = w.start_dragging();
    }
}
