//! 应用共享状态：引擎、用户配置（widget.json）、窗口收缩 / 贴靠状态。
use agent_core::Engine;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard};
use tauri::AppHandle;

pub type Sh = Arc<Shared>;

/// 用户配置，落盘到 widget.json；字段名与 Electron 版一致，两版可共用同一份文件
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Cfg {
    pub x: Option<f64>,
    pub y: Option<f64>,
    pub on_top: bool,
    pub auto_hide: bool,
    pub pinned: bool,
    /// 用户手动隐藏的 agent
    pub hidden: BTreeMap<String, bool>,
    pub ag_quota: bool,
    /// system | light | dark
    pub theme: String,
    /// 毛玻璃（Win11 系统 Acrylic，圆角只能是系统的 8px）；默认开；关闭时回到透明边距 + 阴影的外观（圆角与开启时一致，同为 8px）；仅 Tauri 版支持
    pub blur: bool,
    /// 不认识的键原样保留（别的版本写进去的字段，比如 Electron 版的 collapsed），避免保存时丢掉
    #[serde(flatten)]
    pub extra: serde_json::Map<String, serde_json::Value>,
}

impl Default for Cfg {
    fn default() -> Self {
        Cfg { x: None, y: None, on_top: true, auto_hide: true, pinned: false, hidden: BTreeMap::new(), ag_quota: false, theme: "system".into(), blur: true, extra: Default::default() }
    }
}

impl Cfg {
    pub fn load(file: &std::path::Path) -> Cfg {
        // 首次运行或文件损坏用默认值
        std::fs::read_to_string(file).ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default()
    }
}

/// 逻辑像素矩形（窗口位置 / 大小 / 工作区）
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Rect {
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    Expanded,
    Collapsed,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Dock {
    Top,
    Bottom,
    Left,
    Right,
}

impl Dock {
    pub fn as_str(self) -> &'static str {
        match self {
            Dock::Top => "top",
            Dock::Bottom => "bottom",
            Dock::Left => "left",
            Dock::Right => "right",
        }
    }
}

pub struct WinState {
    pub mode: Mode,
    /// 展开态卡片高度（由渲染端按内容上报）
    pub exp_h: f64,
    /// 收缩时贴靠的边；None 表示不靠边、原地收缩
    pub dock: Option<Dock>,
    /// 收缩动画的最终位置（高度上报可能在动画途中到达，不能用当前位置当终点）
    pub pill_pos: (f64, f64),
    /// 竖向胶囊的卡片高度
    pub v_pill_h: f64,
    /// 收缩 / 展开时保持哪一侧不动：窗口偏右就保持右边缘
    pub anchor_right: bool,
    /// 设置页打开期间锁定：不自动收缩（原生下拉框弹出会让光标“离开”窗口）
    pub locked: bool,
    /// 动画代号：每次开新动画 +1，旧动画线程发现代号变了就退出
    pub anim_gen: u64,
    /// 正在进行的动画终点
    pub anim_to: Option<Rect>,
    pub anim_end: Option<std::time::Instant>,
    /// 收缩计时器代号：用来取消已排队的“延迟收缩”
    pub collapse_gen: u64,
}

impl Default for WinState {
    fn default() -> Self {
        WinState {
            mode: Mode::Expanded, exp_h: 560.0, dock: None, pill_pos: (0.0, 0.0), v_pill_h: 210.0, anchor_right: false,
            locked: false, anim_gen: 0, anim_to: None, anim_end: None, collapse_gen: 0,
        }
    }
}

pub struct Shared {
    pub app: AppHandle,
    pub engine: Mutex<Engine>,
    pub cfg: Mutex<Cfg>,
    pub cfg_file: PathBuf,
    pub ws: Mutex<WinState>,
}

/// 锁中毒（某线程 panic）时继续使用内部数据，而不是让整个应用连带崩溃
fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

impl Shared {
    pub fn engine(&self) -> MutexGuard<'_, Engine> {
        lock(&self.engine)
    }
    pub fn cfg(&self) -> MutexGuard<'_, Cfg> {
        lock(&self.cfg)
    }
    pub fn ws(&self) -> MutexGuard<'_, WinState> {
        lock(&self.ws)
    }
    pub fn save_cfg(&self) {
        // 写失败不影响使用
        let body = serde_json::to_string(&*self.cfg());
        if let Ok(b) = body {
            let _ = std::fs::write(&self.cfg_file, b);
        }
    }
}
