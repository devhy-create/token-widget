// 发布构建隐藏控制台窗口；开发构建保留控制台方便看日志
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod commands;
#[cfg(debug_assertions)]
mod debug;
mod fx;
mod icons;
mod menu;
mod settings;
mod state;
mod window;

use agent_core::util::{default_data_dir, now_ms};
use agent_core::{Engine, EngineOptions};
use state::{Cfg, Mode, Rect, Sh, Shared, WinState};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tauri::{Manager, RunEvent, WebviewUrl, WebviewWindowBuilder, WindowEvent};

/// Claude Code 的 statusLine 钩子入口：`agent-monitor --statusline`，读 stdin、写额度文件、输出一行状态
fn run_statusline() {
    use std::io::{Read, Write};
    let mut input = String::new();
    let _ = std::io::stdin().read_to_string(&mut input);
    let dir = std::env::var_os("TOKEN_WIDGET_DIR").map(std::path::PathBuf::from).unwrap_or_else(default_data_dir);
    let line = agent_core::statusline::run(&input, &dir);
    let _ = std::io::stdout().write_all(line.as_bytes());
}

/// 引擎线程：可见时 2 秒扫描一轮，隐藏时 15 秒；文件之间释放引擎锁，扫描途中界面照常刷新
fn spawn_engine_thread(sh: &Sh) {
    let sh = sh.clone();
    std::thread::spawn(move || {
        let mut last_save = std::time::Instant::now();
        loop {
            let t0 = std::time::Instant::now();
            let now = now_ms();
            let jobs = sh.engine().begin_tick(now);
            for j in &jobs {
                sh.engine().scan_job(j);
            }
            sh.engine().end_tick(now);
            if last_save.elapsed() > Duration::from_secs(30) {
                let _ = sh.engine().save();
                last_save = std::time::Instant::now();
            }
            let every = if window::visible(&sh) { Duration::from_secs(2) } else { Duration::from_secs(15) };
            // 分段睡眠：窗口刚显示时能尽快转入 2 秒节奏
            while t0.elapsed() < every {
                std::thread::sleep(Duration::from_millis(250));
                if window::visible(&sh) && t0.elapsed() > Duration::from_secs(2) {
                    break;
                }
            }
        }
    });
}

/// 推送线程：可见时每秒推一帧快照；隐藏时不推
fn spawn_push_thread(sh: &Sh) {
    let sh = sh.clone();
    std::thread::spawn(move || loop {
        std::thread::sleep(Duration::from_secs(1));
        window::push(&sh);
    });
}

fn create_window(sh: &Sh) -> tauri::Result<()> {
    let app = &sh.app;
    let cfg = sh.cfg().clone();
    // 毛玻璃开关要在算窗口尺寸之前定好（开启时窗口没有透明边距）。环境变量 TW_BLUR=1/0 可强制开 / 关（测试用）
    fx::init(match std::env::var("TW_BLUR").as_deref() {
        Ok("1") => true,
        Ok("0") => false,
        _ => cfg.blur,
    });
    let exp_h = sh.ws().exp_h;
    let (w, h) = window::win_size(window::CARD_W, exp_h);
    let theme = match cfg.theme.as_str() {
        "light" => Some(tauri::Theme::Light),
        "dark" => Some(tauri::Theme::Dark),
        _ => None,
    };
    let builder = WebviewWindowBuilder::new(app, "main", WebviewUrl::App("index.html".into()));
    // 省资源（沿用 Electron 版的做法）：关硬件加速、限制 JS 堆、关掉用不到的 Chromium 特性。
    // 注意 additional_browser_args 会替换 Tauri 的默认参数，所以默认的 disable-features 要一并带上。
    #[cfg(windows)]
    let builder = builder.additional_browser_args(
        "--disable-gpu --disable-features=msWebOOUI,msPdfOOUI,msSmartScreenProtection,CalculateNativeWinOcclusion,MediaRouter --js-flags=--max-old-space-size=128",
    );
    let win = builder
        .title("Agent 监控")
        .inner_size(w, h)
        .decorations(false)
        .transparent(true)
        .shadow(false)
        .resizable(false)
        .maximizable(false)
        .skip_taskbar(true)
        .always_on_top(cfg.on_top)
        .visible(false)
        .theme(theme)
        .build()?;
    // 位置：保存的坐标仍在屏幕内就用，否则放主屏右上角
    let saved = match (cfg.x, cfg.y) {
        (Some(x), Some(y)) if window::on_screen(&win, &Rect { x, y, w, h }) => Some((x, y)),
        _ => None,
    };
    fx::install_subclass(&win);
    fx::apply(&win);
    let (x, y) = saved.unwrap_or_else(|| window::default_pos(&win, (w, h)));
    let _ = win.set_position(tauri::LogicalPosition::new(x, y));

    let shc = sh.clone();
    win.on_window_event(move |ev| match ev {
        // 只记录展开态的位置，收缩态的位置是推算出来的；动画结束后一小段时间内的移动事件也忽略（它们是动画自己造成的）
        WindowEvent::Moved(_) => {
            let settled = {
                let ws = shc.ws();
                ws.mode == Mode::Expanded && ws.anim_to.is_none() && ws.anim_end.is_none_or(|t| t.elapsed() > Duration::from_millis(300))
            };
            if settled {
                if let Some(w) = window::win(&shc) {
                    let b = window::bounds(&w);
                    {
                        let mut c = shc.cfg();
                        c.x = Some(b.x);
                        c.y = Some(b.y);
                    }
                    shc.save_cfg();
                }
            }
        }
        // 关闭按钮只隐藏到托盘
        WindowEvent::CloseRequested { api, .. } => {
            api.prevent_close();
            window::hide_window(&shc);
        }
        // 分辨率变化 / 拔插显示器：确保面板仍在可见范围内
        WindowEvent::ScaleFactorChanged { .. } => {
            let shc2 = shc.clone();
            std::thread::spawn(move || {
                std::thread::sleep(Duration::from_millis(300));
                window::rescue(&shc2);
            });
        }
        _ => {}
    });
    Ok(())
}

fn main() {
    if std::env::args().any(|a| a == "--statusline") {
        run_statusline();
        return;
    }

    let app = tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            if let Some(sh) = app.try_state::<Sh>() {
                window::reveal(sh.inner());
            }
        }))
        .plugin(tauri_plugin_autostart::init(tauri_plugin_autostart::MacosLauncher::LaunchAgent, None))
        .plugin(tauri_plugin_opener::init())
        .on_menu_event(|app, ev| menu::on_menu_event(app, ev.id().as_ref()))
        .invoke_handler(tauri::generate_handler![
            commands::ready, commands::resize, commands::pillsize, commands::hover, commands::icons,
            commands::settings_get, commands::settings_set, commands::claudehook_set, commands::lock,
            commands::pin, commands::menu, commands::close, commands::start_drag,
        ])
        .setup(|app| {
            let dir = default_data_dir();
            let engine = Engine::new(EngineOptions { dir: Some(dir.clone()), home: None, fresh: false })?;
            let cfg_file = dir.join("widget.json");
            let sh: Sh = Arc::new(Shared {
                app: app.handle().clone(),
                engine: Mutex::new(engine),
                cfg: Mutex::new(Cfg::load(&cfg_file)),
                cfg_file,
                ws: Mutex::new(WinState::default()),
            });
            app.manage(sh.clone());
            create_window(&sh)?;
            menu::create_tray(&sh)?;
            spawn_engine_thread(&sh);
            spawn_push_thread(&sh);
            #[cfg(debug_assertions)]
            debug::start(&sh);
            Ok(())
        })
        .build(tauri::generate_context!())
        .expect("初始化 Tauri 失败");

    app.run(|app, event| {
        // 退出前把还在合并中的消息入账并落盘
        if let RunEvent::Exit = event {
            if let Some(sh) = app.try_state::<Sh>() {
                sh.engine().shutdown();
            }
        }
    });
}
