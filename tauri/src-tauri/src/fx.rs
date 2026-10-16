//! 毛玻璃：让系统把窗口后面的桌面模糊掉（网页里的 CSS backdrop-filter 只能模糊页面自己的内容，做不到）。
//! Windows 方案（Win11 22H2+）：
//!   1. DwmSetWindowAttribute(DWMWA_SYSTEMBACKDROP_TYPE = Transient) —— 由 DWM 管理的 Acrylic，拖动 / 改尺寸时没有卡顿；
//!   2. 圆角用系统原生的 DWMWCP_ROUND（8px，抗锯齿，且会正确裁切模糊）。
//!      实测弃用的方案：SetWindowRgn 裁圆角 —— 区域只裁得住网页内容，裁不住系统模糊，圆角外会露出一块方形的灰色模糊（像阴影），
//!      而且区域是 1 位的，边缘锯齿明显；SetWindowCompositionAttribute 的 Acrylic 同样不被区域裁切；
//!   3. 模糊铺满整个窗口，所以窗口必须“卡片 = 窗口”（边距 0），阴影没有了，改用 CSS 内描边。
//!
//! 其它平台暂无实现（macOS 将用原生 vibrancy，见 README 备注）。
use std::sync::atomic::{AtomicBool, Ordering};

/// 当前是否启用毛玻璃（运行时可切换）
static ACTIVE: AtomicBool = AtomicBool::new(false);

pub fn enabled() -> bool {
    cfg!(windows) && ACTIVE.load(Ordering::Relaxed)
}

/// 当前系统是否支持（Win11 22H2 / build 22621 以上才有 DWM 的系统背景材质）
pub fn supported() -> bool {
    cfg!(windows) && windows_build() >= 22621
}

#[cfg(windows)]
fn windows_build() -> u32 {
    // RtlGetVersion 不受清单 / 兼容性垫片影响，能拿到真实版本号
    #[repr(C)]
    struct OsVersionInfo {
        size: u32,
        major: u32,
        minor: u32,
        build: u32,
        platform: u32,
        csd: [u16; 128],
    }
    #[link(name = "ntdll")]
    extern "system" {
        fn RtlGetVersion(info: *mut OsVersionInfo) -> i32;
    }
    let mut v = OsVersionInfo { size: std::mem::size_of::<OsVersionInfo>() as u32, major: 0, minor: 0, build: 0, platform: 0, csd: [0; 128] };
    // 返回 0 表示成功
    if unsafe { RtlGetVersion(&mut v) } == 0 { v.build } else { 0 }
}

#[cfg(not(windows))]
fn windows_build() -> u32 {
    0
}

/// 记下开关状态（不支持的系统上恒为关）。窗口创建前先调用，窗口尺寸要据此算边距。
pub fn init(on: bool) {
    ACTIVE.store(on && supported(), Ordering::Relaxed);
}

/// 把当前开关状态应用到窗口：设置系统 Acrylic 材质 + 原生圆角，并调整窗口样式。
/// 开关变化会让窗口边距变化，调用方需要先 init 再重算窗口尺寸，最后调这里。
/// 实测不需要 DwmExtendFrameIntoClientArea，模糊也能铺满无边框窗口。
#[cfg(windows)]
pub fn apply(w: &tauri::WebviewWindow) {
    use windows_sys::Win32::Graphics::Dwm::DwmSetWindowAttribute;
    let on = enabled();
    let Ok(hwnd) = w.hwnd() else { return };
    let h = hwnd.0 as _;
    // DWMWA_WINDOW_CORNER_PREFERENCE = 33：0 默认 / 2 DWMWCP_ROUND；DWMWA_SYSTEMBACKDROP_TYPE = 38：0 自动 / 3 Acrylic(Transient)
    let corner: i32 = if on { 2 } else { 0 };
    let backdrop: i32 = if on { 3 } else { 0 };
    unsafe {
        DwmSetWindowAttribute(h, 33, &corner as *const i32 as *const _, 4);
        DwmSetWindowAttribute(h, 38, &backdrop as *const i32 as *const _, 4);
        set_frame_style(h, on);
    }
}

/// 无边框窗口默认仍带着 WS_CAPTION / WS_SYSMENU 等样式（tao 只是不画边框）。毛玻璃窗口被拖动时，
/// DWM 会在模糊的窗口顶部画出系统标题栏（图标 + 标题 + 最小化 / 关闭按钮），所以开启时改成纯 WS_POPUP，关闭时还原。
#[cfg(windows)]
unsafe fn set_frame_style(h: windows_sys::Win32::Foundation::HWND, on: bool) {
    use std::sync::atomic::AtomicI32;
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        GetWindowLongW, SetWindowLongW, SetWindowPos, GWL_STYLE, SWP_FRAMECHANGED, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, SWP_NOZORDER,
        WS_CAPTION, WS_MAXIMIZEBOX, WS_MINIMIZEBOX, WS_POPUP, WS_SYSMENU, WS_THICKFRAME, WS_VISIBLE,
    };
    // 记下开启前的原样式，关闭时还原（0 表示还没记过）
    static ORIG: AtomicI32 = AtomicI32::new(0);
    let cur = GetWindowLongW(h, GWL_STYLE);
    let new = if on {
        if ORIG.load(Ordering::Relaxed) == 0 {
            ORIG.store(cur, Ordering::Relaxed);
        }
        (cur & !((WS_CAPTION | WS_SYSMENU | WS_MINIMIZEBOX | WS_MAXIMIZEBOX | WS_THICKFRAME) as i32)) | WS_POPUP as i32
    } else {
        match ORIG.load(Ordering::Relaxed) {
            0 => return,
            // 原样式是创建时（还没显示）记的，可见位要沿用当前的，否则还原后窗口会消失
            o => (o & !(WS_VISIBLE as i32)) | (cur & WS_VISIBLE as i32),
        }
    };
    if new != cur {
        SetWindowLongW(h, GWL_STYLE, new);
        SetWindowPos(h, std::ptr::null_mut(), 0, 0, 0, 0, SWP_FRAMECHANGED | SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE);
    }
}

/// 重新套用窗口样式：tao 每次 show() 都会把完整样式（含 WS_CAPTION）写回去，所以每次 show 之后都要调一次
#[cfg(windows)]
pub fn restyle(w: &tauri::WebviewWindow) {
    if let Ok(hwnd) = w.hwnd() {
        unsafe { set_frame_style(hwnd.0 as _, enabled()) };
    }
}

/// 子类化窗口过程（窗口创建后调用一次，必须在创建窗口的线程）：毛玻璃开启时拦掉非客户区的重绘。
/// 实测问题：毛玻璃窗口失去激活（点了别的窗口）后，系统会重画“标题栏”，在模糊窗口顶部闪出一条白色标题条。
/// 做法是自绘边框的经典写法：WM_NCACTIVATE 把 lParam 改成 -1（告诉系统不要重画非客户区），WM_NCPAINT 直接吞掉。
#[cfg(windows)]
pub fn install_subclass(w: &tauri::WebviewWindow) {
    use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
    use windows_sys::Win32::UI::Shell::{DefSubclassProc, SetWindowSubclass};
    unsafe extern "system" fn proc(h: HWND, msg: u32, wp: WPARAM, lp: LPARAM, _id: usize, _data: usize) -> LRESULT {
        const WM_NCPAINT: u32 = 0x0085;
        const WM_NCACTIVATE: u32 = 0x0086;
        if enabled() {
            match msg {
                WM_NCPAINT => return 0,
                WM_NCACTIVATE => return DefSubclassProc(h, msg, wp, -1),
                _ => {}
            }
        }
        DefSubclassProc(h, msg, wp, lp)
    }
    if let Ok(hwnd) = w.hwnd() {
        // 同一个 id 重复安装只会替换，不会叠加
        unsafe { SetWindowSubclass(hwnd.0 as _, Some(proc), 0x5457, 0) };
    }
}

#[cfg(not(windows))]
pub fn apply(_w: &tauri::WebviewWindow) {}

#[cfg(not(windows))]
pub fn restyle(_w: &tauri::WebviewWindow) {}

#[cfg(not(windows))]
pub fn install_subclass(_w: &tauri::WebviewWindow) {}