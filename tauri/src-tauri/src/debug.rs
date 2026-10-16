//! 仅调试构建可用的无人值守验证脚本。
//! TW_DEBUG_STEPS：用 `;;` 分隔的步骤，每步之间间隔 1.5 秒，页面加载后开始执行：
//!   collapse | expand | dock:left|right|bottom|top | js:<要在页面里执行的脚本>
//! TW_DEBUG_NOHOVER=1：忽略界面上报的鼠标进出（避免真实鼠标干扰自动化验证）。
use crate::state::{Rect, Sh};
use crate::window::{self, margin};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

pub static NO_HOVER: AtomicBool = AtomicBool::new(false);

pub fn start(sh: &Sh) {
    if std::env::var("TW_DEBUG_NOHOVER").is_ok_and(|v| v == "1") {
        NO_HOVER.store(true, Ordering::Relaxed);
    }
    let Ok(steps) = std::env::var("TW_DEBUG_STEPS") else { return };
    let sh = sh.clone();
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_secs(5));
        for step in steps.split(";;").map(str::trim).filter(|s| !s.is_empty()) {
            run_step(&sh, step);
            std::thread::sleep(Duration::from_millis(1500));
        }
    });
}

fn run_step(sh: &Sh, step: &str) {
    let Some(w) = window::win(sh) else { return };
    if step == "collapse" {
        window::collapse(sh);
    } else if step == "expand" {
        window::expand(sh);
    } else if let Some(edge) = step.strip_prefix("dock:") {
        // 把窗口挪到该边附近，随后的 collapse 会吸附上去
        let b = window::bounds(&w);
        let wa = window::work_area_of(&w, &b);
        let (x, y) = match edge {
            "left" => (wa.x - margin() + 30.0, 200.0),
            "right" => (wa.x + wa.w - b.w + margin() - 30.0, 200.0),
            "bottom" => (wa.x + ((wa.w - b.w) / 2.0).round(), wa.y + wa.h - b.h + margin() - 30.0),
            _ => (wa.x + 300.0, wa.y - margin() + 4.0),
        };
        window::set_bounds(&w, &Rect { x, y, w: b.w, h: b.h });
    } else if let Some(js) = step.strip_prefix("js:") {
        let _ = w.eval(js);
    }
}
