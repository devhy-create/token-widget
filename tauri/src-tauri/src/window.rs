//! 窗口行为：透明置顶小窗、收缩成胶囊 / 贴边、展开、缓动动画、越界拉回。
//! 全部用逻辑像素计算。
use crate::state::{Dock, Mode, Rect, Sh};
use serde_json::json;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant};
use tauri::{Emitter, LogicalPosition, LogicalSize, Manager, WebviewWindow};

/// 窗口是透明的：卡片四周留出 M 像素给 CSS 阴影，所以窗口尺寸 = 卡片尺寸 + 2M。
/// 必须与 renderer/style.css 里的 #card{inset:20px} 一致。
pub const M: f64 = 20.0;
pub const CARD_W: f64 = 380.0;
/// 胶囊宽度：要容纳 4 个 agent 速度 + 「今日 $xxx.xx」，金额位数多时也不能被截断
const PILL_W: f64 = 372.0;
const PILL_H: f64 = 30.0;
/// 贴左 / 右边时的竖向胶囊宽度（高度由渲染端按显示的 agent 数量上报）
pub const VPILL_W: f64 = 26.0;
/// 卡片离屏幕边缘多近就吸附
const DOCK_SNAP: f64 = 60.0;
/// 吸附后与屏幕边缘留的缝
const DOCK_GAP: f64 = 2.0;
/// 鼠标移出后多久收缩
const COLLAPSE_DELAY: u64 = 900;
const ANIM_MS: f64 = 280.0;

pub fn win(sh: &Sh) -> Option<WebviewWindow> {
    sh.app.get_webview_window("main")
}

// ---------------------------------------------------------------- 文本缩放
// Windows「设置 → 辅助功能 → 文本大小」（100%~225%）会被 WebView2 乘进整个页面的缩放：
// 文本大小 110% 时 CSS 1px = 1.1 逻辑像素，页面里的一切（图标、间距）都放大 1.1 倍。
// 卡片宽度、阴影边距、渲染端上报的高度都是 CSS 像素，换算成窗口逻辑像素都要乘这个比例，
// 否则窗口装不下页面，右侧和底部会被切掉（另一台电脑 TextScaleFactor=110 时实测如此）。
static TEXT_SCALE: AtomicU64 = AtomicU64::new(0x3FF0_0000_0000_0000); // 1.0 的位表示

/// 当前文本缩放比例（1.0 = 100%）
pub fn text_scale() -> f64 {
    f64::from_bits(TEXT_SCALE.load(Ordering::Relaxed))
}

/// 测试开关：环境变量 TW_TEXT_SCALE=1.1 模拟文本大小 110%（窗口按比例放大，页面也用 set_zoom 放大，模拟 WebView2 的行为）
pub fn simulated_text_scale() -> Option<f64> {
    std::env::var("TW_TEXT_SCALE").ok()?.trim().parse::<f64>().ok().filter(|v| (1.0..=3.0).contains(v))
}

#[cfg(windows)]
fn system_text_scale() -> Option<f64> {
    use windows_sys::Win32::System::Registry::{RegGetValueW, HKEY_CURRENT_USER, RRF_RT_REG_DWORD};
    let key: Vec<u16> = "Software\\Microsoft\\Accessibility\0".encode_utf16().collect();
    let name: Vec<u16> = "TextScaleFactor\0".encode_utf16().collect();
    let (mut v, mut len) = (0u32, 4u32);
    // 没调过文本大小时这个值不存在，等于 100%
    let r = unsafe {
        RegGetValueW(HKEY_CURRENT_USER, key.as_ptr(), name.as_ptr(), RRF_RT_REG_DWORD, std::ptr::null_mut(), &mut v as *mut u32 as *mut _, &mut len)
    };
    (r == 0 && (100..=225).contains(&v)).then(|| v as f64 / 100.0)
}

#[cfg(not(windows))]
fn system_text_scale() -> Option<f64> {
    None
}

/// 重新读取文本缩放比例，变了返回 true
pub fn refresh_text_scale() -> bool {
    let t = simulated_text_scale().or_else(system_text_scale).unwrap_or(1.0);
    TEXT_SCALE.swap(t.to_bits(), Ordering::Relaxed) != t.to_bits()
}

/// 用户在系统设置里改了文本大小：按新比例重排窗口（推送线程每秒检查一次，读注册表很快）
pub fn check_text_scale(sh: &Sh) {
    if !refresh_text_scale() {
        return;
    }
    let Some(w) = win(sh) else { return };
    let (mode, exp_h, anchor_right) = {
        let mut ws = sh.ws();
        ws.anim_gen += 1; // 打断正在进行的动画，按新比例重算
        ws.anim_to = None;
        (ws.mode, ws.exp_h, ws.anchor_right)
    };
    if mode == Mode::Collapsed {
        // 收缩态：按新比例重新收一次（重新挑贴靠的边、算胶囊位置）
        sh.ws().mode = Mode::Expanded;
        collapse(sh);
        return;
    }
    let b = bounds(&w);
    let wa = work_area_of(&w, &b);
    let s = win_size(CARD_W, exp_h.min(max_card_h(&wa)));
    let x = if anchor_right { b.x + b.w - s.0 } else { b.x };
    set_bounds(&w, &Rect { x, y: b.y, w: s.0, h: s.1 });
}

/// 卡片内容（CSS 像素）在 cw × ch 时的窗口逻辑尺寸。向上取整，避免小数像素让页面差一点装不下
pub fn win_size(cw: f64, ch: f64) -> (f64, f64) {
    let t = text_scale();
    (((cw + 2.0 * css_margin()) * t).ceil(), ((ch + 2.0 * css_margin()) * t).ceil())
}

/// 展开态卡片最高多少 CSS 像素：窗口不超过工作区高度减 8
pub fn max_card_h(wa: &Rect) -> f64 {
    (wa.h - 8.0) / text_scale() - 2.0 * css_margin()
}

pub fn bounds(w: &WebviewWindow) -> Rect {
    let sf = w.scale_factor().unwrap_or(1.0);
    let p = w.outer_position().map(|p| (p.x as f64 / sf, p.y as f64 / sf)).unwrap_or((0.0, 0.0));
    let s = w.outer_size().map(|s| (s.width as f64 / sf, s.height as f64 / sf)).unwrap_or((0.0, 0.0));
    Rect { x: p.0, y: p.1, w: s.0, h: s.1 }
}

pub fn set_bounds(w: &WebviewWindow, r: &Rect) {
    let _ = w.set_size(LogicalSize::new(r.w, r.h));
    let _ = w.set_position(LogicalPosition::new(r.x, r.y));
}

/// 透明边距（给阴影留的，CSS 像素）。毛玻璃开启时模糊铺满整个窗口、形状靠区域裁切，没有阴影可画，边距为 0
fn css_margin() -> f64 {
    if crate::fx::enabled() { 0.0 } else { M }
}

/// 透明边距换算成窗口逻辑像素（随文本缩放放大）；窗口定位、贴边、判断光标是否在卡片内都用它
pub fn margin() -> f64 {
    css_margin() * text_scale()
}

/// 运行时开 / 关毛玻璃：窗口边距随之在 0 与 M 之间变化，要保持卡片位置不动、重算窗口矩形
pub fn set_blur(sh: &Sh, on: bool) {
    let Some(w) = win(sh) else { return };
    if (on && crate::fx::supported()) == crate::fx::enabled() {
        return;
    }
    let (mut anim_to, pill_pos) = {
        let mut ws = sh.ws();
        ws.anim_gen += 1; // 打断正在进行的动画
        (ws.anim_to.take(), ws.pill_pos)
    };
    let b = anim_to.take().unwrap_or_else(|| bounds(&w));
    let old_m = margin();
    crate::fx::init(on);
    let m = margin();
    let d = old_m - m;
    // 卡片矩形 = 旧窗口去掉旧边距；新窗口 = 卡片加上新边距
    let nb = Rect { x: b.x + d, y: b.y + d, w: b.w - 2.0 * d, h: b.h - 2.0 * d };
    sh.ws().pill_pos = (pill_pos.0 + d, pill_pos.1 + d);
    crate::fx::apply(&w);
    set_bounds(&w, &nb);
}

pub fn visible(sh: &Sh) -> bool {
    win(sh).is_some_and(|w| w.is_visible().unwrap_or(false) && !w.is_minimized().unwrap_or(false))
}

/// 所有显示器的工作区（逻辑像素）
fn work_areas(w: &WebviewWindow) -> Vec<Rect> {
    w.available_monitors().unwrap_or_default().iter().map(|m| {
        let sf = m.scale_factor();
        let a = m.work_area();
        Rect { x: a.position.x as f64 / sf, y: a.position.y as f64 / sf, w: a.size.width as f64 / sf, h: a.size.height as f64 / sf }
    }).collect()
}

fn overlap(a: &Rect, b: &Rect) -> f64 {
    let ox = (a.x + a.w).min(b.x + b.w) - a.x.max(b.x);
    let oy = (a.y + a.h).min(b.y + b.h) - a.y.max(b.y);
    if ox > 0.0 && oy > 0.0 { ox * oy } else { 0.0 }
}

/// 与窗口重叠最多的那块显示器的工作区
pub fn work_area_of(w: &WebviewWindow, b: &Rect) -> Rect {
    let areas = work_areas(w);
    areas.iter().copied().max_by(|a, c| overlap(a, b).total_cmp(&overlap(c, b)))
        .or_else(|| primary_work_area(w))
        .unwrap_or(Rect { x: 0.0, y: 0.0, w: 1920.0, h: 1040.0 })
}

fn primary_work_area(w: &WebviewWindow) -> Option<Rect> {
    let m = w.primary_monitor().ok().flatten()?;
    let sf = m.scale_factor();
    let a = m.work_area();
    Some(Rect { x: a.position.x as f64 / sf, y: a.position.y as f64 / sf, w: a.size.width as f64 / sf, h: a.size.height as f64 / sf })
}

/// 默认位置：主屏右上角
pub fn default_pos(w: &WebviewWindow, s: (f64, f64)) -> (f64, f64) {
    let wa = primary_work_area(w).unwrap_or(Rect { x: 0.0, y: 0.0, w: 1920.0, h: 1040.0 });
    (wa.x + wa.w - s.0 - 4.0, wa.y + 4.0)
}

/// 卡片（去掉透明阴影边距）是否至少有一部分落在某个显示器的工作区内。
/// 分辨率 / 显示器变化后，保存的坐标可能已经在屏幕外，用户就找不到面板了。
pub fn on_screen(w: &WebviewWindow, b: &Rect) -> bool {
    let need_x = 60.0; // 至少露出 60px 宽、30px 高，才算还抓得到
    let need_y = 30.0;
    work_areas(w).iter().any(|a| {
        let ox = (b.x + b.w - margin()).min(a.x + a.w) - (b.x + margin()).max(a.x);
        let oy = (b.y + b.h - margin()).min(a.y + a.h) - (b.y + margin()).max(a.y);
        ox >= need_x && oy >= need_y
    })
}

/// 面板跑出屏幕时拉回右上角，并清掉保存的坐标
pub fn rescue(sh: &Sh) {
    let Some(w) = win(sh) else { return };
    let b = bounds(&w);
    if on_screen(&w, &b) {
        return;
    }
    let (x, y) = default_pos(&w, (b.w, b.h));
    set_bounds(&w, &Rect { x, y, w: b.w, h: b.h });
    {
        let mut c = sh.cfg();
        c.x = None;
        c.y = None;
    }
    sh.save_cfg();
}

pub fn send_mode(sh: &Sh) {
    let Some(w) = win(sh) else { return };
    let (mode, dock) = {
        let ws = sh.ws();
        (ws.mode, ws.dock)
    };
    let c = sh.cfg().clone();
    let _ = w.emit("mode", json!({
        "mode": if mode == Mode::Collapsed { "collapsed" } else { "expanded" },
        "dock": if mode == Mode::Collapsed { dock.map(Dock::as_str) } else { None },
        "pinned": c.pinned, "autoHide": c.auto_hide, "hidden": c.hidden,
        // 告诉界面当前有没有系统毛玻璃：有的话卡片底色要调成半透明，否则看不出模糊
        "fx": crate::fx::enabled(),
    }));
}

/// 推送一帧快照给界面（窗口不可见时不推）
pub fn push(sh: &Sh) {
    if !visible(sh) {
        return;
    }
    let snap = sh.engine().snapshot_now();
    if let Some(w) = win(sh) {
        let _ = w.emit("snap", snap);
    }
}

/// 窗口边界缓动：曲线接近 iOS 的减速弹出（先快后慢）。动画期间约 60fps 设置位置，结束即停，不常驻耗电。
fn ease_out(t: f64) -> f64 {
    1.0 - (1.0 - t).powi(4)
}

pub fn tween(sh: &Sh, to: Rect) {
    let Some(w) = win(sh) else { return };
    let from = bounds(&w);
    if from == to {
        return;
    }
    let gen = {
        let mut ws = sh.ws();
        ws.anim_gen += 1;
        ws.anim_to = Some(to);
        ws.anim_gen
    };
    if !visible(sh) {
        set_bounds(&w, &to);
        end_anim(sh, gen);
        return;
    }
    let sh = sh.clone();
    std::thread::spawn(move || {
        let t0 = Instant::now();
        // 调试：统计每帧 set_bounds 的耗时与帧间隔（TW_PERF=1），用来评估毛玻璃下动画是否流畅
        #[cfg(debug_assertions)]
        let (mut frames, mut sum_us, mut max_us, mut last, mut max_gap_ms) = (0u32, 0u128, 0u128, Instant::now(), 0u128);
        loop {
            std::thread::sleep(Duration::from_millis(15));
            if sh.ws().anim_gen != gen {
                return; // 被新动画取代
            }
            let p = (t0.elapsed().as_secs_f64() * 1000.0 / ANIM_MS).min(1.0);
            let e = ease_out(p);
            let lerp = |a: f64, b: f64| (a + (b - a) * e).round();
            #[cfg(debug_assertions)]
            let tf = Instant::now();
            set_bounds(&w, &Rect { x: lerp(from.x, to.x), y: lerp(from.y, to.y), w: lerp(from.w, to.w), h: lerp(from.h, to.h) });
            #[cfg(debug_assertions)]
            {
                let us = tf.elapsed().as_micros();
                frames += 1;
                sum_us += us;
                max_us = max_us.max(us);
                max_gap_ms = max_gap_ms.max(last.elapsed().as_millis());
                last = Instant::now();
                if p >= 1.0 && std::env::var("TW_PERF").is_ok() {
                    eprintln!("[perf] fx={} 帧数={} set_bounds 平均={}us 最大={}us 最大帧间隔={}ms", crate::fx::enabled(), frames, sum_us / frames as u128, max_us, max_gap_ms);
                }
            }
            if p >= 1.0 {
                end_anim(&sh, gen);
                return;
            }
        }
    });
}

fn end_anim(sh: &Sh, gen: u64) {
    let mut ws = sh.ws();
    if ws.anim_gen == gen {
        ws.anim_to = None;
        ws.anim_end = Some(Instant::now());
    }
}

/// 该贴哪条边：靠近顶部一律先贴顶（右上角是默认位置，保持原来的“贴顶”行为）；
/// 否则在左 / 右 / 底里取离卡片最近、且不超过 DOCK_SNAP 的那条；都不近就不贴边。
fn pick_dock(b: &Rect, wa: &Rect) -> Option<Dock> {
    let (l, t, r, bt) = (b.x + margin(), b.y + margin(), b.x + b.w - margin(), b.y + b.h - margin());
    if t - wa.y < DOCK_SNAP {
        return Some(Dock::Top);
    }
    let d = [(Dock::Left, l - wa.x), (Dock::Right, wa.x + wa.w - r), (Dock::Bottom, wa.y + wa.h - bt)];
    let mut best: Option<(Dock, f64)> = None;
    for (edge, v) in d {
        if v < DOCK_SNAP && best.is_none_or(|(_, bv)| v < bv) {
            best = Some((edge, v));
        }
    }
    best.map(|(e, _)| e)
}

/// 收缩成小胶囊：靠上 / 下边 → 横向胶囊贴边；靠左 / 右边 → 竖向胶囊贴边；不靠边 → 原地收成横向胶囊。
/// 展开时保持贴边的那一侧不动（右侧贴边、偏右的窗口都向左展开）。
pub fn collapse(sh: &Sh) {
    let Some(w) = win(sh) else { return };
    if sh.ws().mode == Mode::Collapsed {
        return;
    }
    let b = bounds(&w);
    let wa = work_area_of(&w, &b);
    let dock = pick_dock(&b, &wa);
    let vertical = matches!(dock, Some(Dock::Left | Dock::Right));
    let anchor_right = match dock {
        Some(Dock::Right) => true,
        Some(Dock::Left) => false,
        _ => b.x + b.w / 2.0 > wa.x + wa.w / 2.0,
    };
    let v_pill_h = sh.ws().v_pill_h;
    let s = if vertical { win_size(VPILL_W, v_pill_h) } else { win_size(PILL_W, PILL_H) };
    let mut x = if anchor_right { b.x + b.w - s.0 } else { b.x };
    let mut y = b.y;
    match dock {
        Some(Dock::Top) => y = wa.y - margin() + DOCK_GAP,
        Some(Dock::Bottom) => y = wa.y + wa.h - s.1 + margin() - DOCK_GAP,
        Some(Dock::Left) => x = wa.x - margin() + DOCK_GAP,
        Some(Dock::Right) => x = wa.x + wa.w - s.0 + margin() - DOCK_GAP,
        None => {}
    }
    // 竖向胶囊可能比原来的卡片短 / 长，纵向要夹在工作区内
    if vertical {
        y = y.min(wa.y + wa.h - s.1 + margin() - DOCK_GAP).max(wa.y - margin() + DOCK_GAP);
    }
    {
        let mut ws = sh.ws();
        ws.mode = Mode::Collapsed;
        ws.dock = dock;
        ws.anchor_right = anchor_right;
        ws.pill_pos = (x, y);
    }
    send_mode(sh);
    tween(sh, Rect { x, y, w: s.0, h: s.1 });
}

pub fn expand(sh: &Sh) {
    let Some(w) = win(sh) else { return };
    let (mode, exp_h, anchor_right) = {
        let ws = sh.ws();
        (ws.mode, ws.exp_h, ws.anchor_right)
    };
    if mode == Mode::Expanded {
        return;
    }
    let b = bounds(&w);
    let wa = work_area_of(&w, &b);
    let s = win_size(CARD_W, exp_h.min(max_card_h(&wa)));
    let mut x = if anchor_right { b.x + b.w - s.0 } else { b.x };
    x = x.min(wa.x + wa.w - s.0 + margin()).max(wa.x - margin());
    let y = b.y.min(wa.y + wa.h - s.1 + margin() - 4.0).max(wa.y - margin() + 4.0);
    {
        let mut ws = sh.ws();
        ws.mode = Mode::Expanded;
        ws.dock = None;
    }
    send_mode(sh);
    tween(sh, Rect { x, y, w: s.0, h: s.1 });
}

/// 光标是否在“卡片”内（透明的阴影边距不算）
fn cursor_inside(sh: &Sh) -> bool {
    cursor_inside_opt(sh).unwrap_or(false)
}

/// 同上；取不到窗口或光标位置时返回 None（定时检查据此跳过，不误收缩）
fn cursor_inside_opt(sh: &Sh) -> Option<bool> {
    let w = win(sh)?;
    let sf = w.scale_factor().unwrap_or(1.0);
    let p = w.cursor_position().ok()?;
    let (px, py) = (p.x / sf, p.y / sf);
    let b = bounds(&w);
    Some(px >= b.x + margin() && px < b.x + b.w - margin() && py >= b.y + margin() && py < b.y + b.h - margin())
}

/// 上一次定时检查时光标是否在卡片外
static WAS_OUTSIDE: AtomicBool = AtomicBool::new(false);

/// 兜底的自动收缩：由推送线程每秒调用一次。
/// 平时靠界面上报的鼠标进出事件收缩；但光标被程序“瞬移”时（如 PowerToys Mouse Without Borders
/// 切到另一台电脑时把光标藏到屏幕边缘、切回时再放回来）网页收不到离开事件，窗口会一直展开。
/// 这里直接看光标实际位置：展开状态下连续两次（约 1~2 秒）都在卡片外就收缩。
pub fn check_hover(sh: &Sh) {
    // 先单独取 mode：sh.ws() 的锁守卫是临时值，和 can_auto_hide（内部也锁 ws）写在同一个表达式里会自锁
    let expanded = sh.ws().mode == Mode::Expanded;
    let active = expanded && visible(sh) && can_auto_hide(sh);
    #[cfg(debug_assertions)]
    let active = active && !crate::debug::NO_HOVER.load(Ordering::Relaxed);
    let outside = active && cursor_inside_opt(sh) == Some(false);
    if outside && WAS_OUTSIDE.load(Ordering::Relaxed) {
        WAS_OUTSIDE.store(false, Ordering::Relaxed);
        collapse(sh);
    } else {
        WAS_OUTSIDE.store(outside, Ordering::Relaxed);
    }
}

pub fn can_auto_hide(sh: &Sh) -> bool {
    let c = sh.cfg();
    c.auto_hide && !c.pinned && !sh.ws().locked
}

/// 取消已排队的延迟收缩
pub fn cancel_collapse(sh: &Sh) {
    sh.ws().collapse_gen += 1;
}

/// 鼠标移开后排一个延迟收缩；期间再次进入或取消则作废
pub fn arm_collapse(sh: &Sh) {
    // 调试：忽略鼠标时也不自动收缩，收缩 / 展开全由脚本步骤触发
    #[cfg(debug_assertions)]
    if crate::debug::NO_HOVER.load(std::sync::atomic::Ordering::Relaxed) {
        return;
    }
    let gen = {
        let mut ws = sh.ws();
        ws.collapse_gen += 1;
        ws.collapse_gen
    };
    if !can_auto_hide(sh) || sh.ws().mode != Mode::Expanded {
        return;
    }
    let sh = sh.clone();
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(COLLAPSE_DELAY));
        if sh.ws().collapse_gen == gen && !cursor_inside(&sh) {
            collapse(&sh);
        }
    });
}

pub fn show_window(sh: &Sh) {
    if let Some(w) = win(sh) {
        let _ = w.show();
        let _ = w.unminimize();
        crate::fx::restyle(&w);
    }
    push(sh);
    crate::menu::refresh_tray(sh);
}

pub fn hide_window(sh: &Sh) {
    if let Some(w) = win(sh) {
        let _ = w.hide();
    }
    crate::menu::refresh_tray(sh);
}

/// 第二次启动 / 托盘点击：显示并展开
pub fn reveal(sh: &Sh) {
    show_window(sh);
    expand(sh);
    arm_collapse(sh);
}
