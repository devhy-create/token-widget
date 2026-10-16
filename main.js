// Electron 主进程：透明无边框置顶小窗 + 托盘 + 引擎驱动 + 顶部自动收缩（带缓动动画）。
// 省资源要点：关硬件加速、窗口不可见时暂停推送并降频扫描、渲染端只在数据变化时改 DOM、动画只在切换瞬间跑。
const { app, BrowserWindow, Tray, Menu, ipcMain, screen, nativeImage, nativeTheme, shell } = require('electron');
const path = require('path');
const fs = require('fs');
const { Engine } = require('./core/engine');
const { resolveIcons } = require('./core/icons');
const plansMod = require('./core/plans');
const { AntigravityQuota } = require('./core/antigravity');
const { execFile } = require('child_process');
const os = require('os');

const AGENT_IDS = ['claude', 'codex', 'antigravity', 'workbuddy'];
const AGENT_NAMES = { claude: 'Claude', codex: 'Codex', antigravity: 'Antigravity', workbuddy: 'WorkBuddy' };

// 固定数据目录：开发 / 安装版 / 便携版共用 %APPDATA%\token-widget（否则打包后 userData 会按 productName 变成 "Agent Monitor"，
// 与引擎的缓存目录分裂）。必须在单实例锁之前设置，锁文件也放在 userData 里。
app.setPath('userData', path.join(app.getPath('appData'), 'token-widget'));

app.disableHardwareAcceleration();
app.commandLine.appendSwitch('disable-features', 'CalculateNativeWinOcclusion,MediaRouter');
app.commandLine.appendSwitch('js-flags', '--max-old-space-size=128');

if (!app.requestSingleInstanceLock()) app.quit();

// 窗口是透明的：卡片四周留出 M 像素给 CSS 阴影，所以窗口尺寸 = 卡片尺寸 + 2M
const M = 20;
const CARD_W = 380;
const PILL_W = 372; // 胶囊宽度：要容纳 4 个 agent 速度 + 「今日 $xxx.xx」，金额位数多时也不能被截断
const PILL_H = 30;
const VPILL_W = 26;     // 贴左/右边时的竖向胶囊宽度（高度由渲染端按显示的 agent 数量上报）
const DOCK_SNAP = 60;   // 卡片离屏幕边缘多近就吸附
const DOCK_GAP = 2;     // 吸附后与屏幕边缘留的缝
const COLLAPSE_DELAY = 900; // 鼠标移出后多久收缩
const ANIM_MS = 280;

let win = null;
let tray = null;
let engine = null;
let timers = [];
let collapseTimer = null;
let anim = null;
let mode = 'expanded';      // expanded | collapsed
let expH = 560;             // 展开态卡片高度（由渲染端按内容上报）
let dock = null;            // 收缩时贴靠的边：top | bottom | left | right | null（不靠边，原地收缩）
let pillPos = { x: 0, y: 0 }; // 收缩动画的最终位置（高度上报可能在动画途中到达，不能用当前位置当终点）
let vPillH = 210;           // 竖向胶囊的卡片高度
let anchorRight = false;    // 收缩/展开时保持哪一侧不动：窗口偏右就保持右边缘
let cfg = { x: null, y: null, onTop: true, autoHide: true, pinned: false, hidden: {}, agQuota: false, theme: 'system' }; // hidden: 用户手动隐藏的 agent
let cfgFile;

function loadCfg() {
  try { cfg = { ...cfg, ...JSON.parse(fs.readFileSync(cfgFile, 'utf8')) }; } catch { /* 首次运行用默认值 */ }
}
function saveCfg() {
  try { fs.writeFileSync(cfgFile, JSON.stringify(cfg)); } catch { /* 写失败不影响使用 */ }
}

// ---- 开机自启 ----
// 未打包（开发模式）时 process.execPath 是裸 electron.exe，注册进去没有意义，所以只允许打包后使用。
// 便携版运行时 execPath 指向临时解压目录，真正的 exe 路径在 PORTABLE_EXECUTABLE_FILE 里。
const canAutostart = () => app.isPackaged;
const loginPath = () => process.env.PORTABLE_EXECUTABLE_FILE || process.execPath;
const getAutostart = () => canAutostart() && app.getLoginItemSettings({ path: loginPath() }).openAtLogin;
const setAutostart = (on) => { if (canAutostart()) app.setLoginItemSettings({ openAtLogin: !!on, path: loginPath() }); };

function visible() {
  return win && win.isVisible() && !win.isMinimized();
}
const workAreaOf = (b) => screen.getDisplayMatching(b).workArea;
let locked = false; // 设置页打开期间锁定：不自动收缩（原生下拉框弹出会让光标“离开”窗口）
const canAutoHide = () => cfg.autoHide && !cfg.pinned && !locked;

function sendMode() {
  if (win && !win.webContents.isDestroyed()) win.webContents.send('mode', { mode, dock: mode === 'collapsed' ? dock : null, pinned: cfg.pinned, autoHide: cfg.autoHide, hidden: cfg.hidden });
}

// 窗口边界缓动：曲线接近 iOS 的减速弹出（先快后慢）。动画期间约 60fps 调用 setBounds，结束即停，不常驻耗电。
const easeOut = (t) => 1 - Math.pow(1 - t, 4);
function tween(to, ms = ANIM_MS) {
  if (anim) { clearInterval(anim.id); anim = null; }
  const from = win.getBounds();
  const same = ['x', 'y', 'width', 'height'].every((k) => from[k] === to[k]);
  if (same) return;
  if (!visible()) { win.setBounds(to); return; }
  const t0 = Date.now();
  const id = setInterval(() => {
    const p = Math.min(1, (Date.now() - t0) / ms);
    const e = easeOut(p);
    const lerp = (a, b) => Math.round(a + (b - a) * e);
    win.setBounds({ x: lerp(from.x, to.x), y: lerp(from.y, to.y), width: lerp(from.width, to.width), height: lerp(from.height, to.height) });
    if (p >= 1) { clearInterval(id); if (anim && anim.id === id) anim = null; }
  }, 15);
  anim = { id, to }; // 记下终点：动画途中收到的高度上报要以终点为准，不能用此刻的中间位置
}

// 目标窗口矩形（以卡片尺寸换算）
const winSize = (cw, ch) => ({ width: cw + 2 * M, height: ch + 2 * M });

// 该贴哪条边：靠近顶部一律先贴顶（右上角是默认位置，保持原来的“贴顶”行为）；
// 否则在左 / 右 / 底里取离卡片最近、且不超过 DOCK_SNAP 的那条；都不近就不贴边。
function pickDock(b, wa) {
  const l = b.x + M, t = b.y + M, r = b.x + b.width - M, bt = b.y + b.height - M;
  if (t - wa.y < DOCK_SNAP) return 'top';
  const d = [['left', l - wa.x], ['right', wa.x + wa.width - r], ['bottom', wa.y + wa.height - bt]];
  let best = null;
  for (const [edge, v] of d) if (v < DOCK_SNAP && (!best || v < best[1])) best = [edge, v];
  return best && best[0];
}

// 收缩成小胶囊：靠上/下边 → 横向胶囊贴边；靠左/右边 → 竖向胶囊贴边；不靠边 → 原地收成横向胶囊。
// 展开时保持贴边的那一侧不动（右侧贴边、偏右的窗口都向左展开）。
function collapse() {
  if (mode === 'collapsed' || !win) return;
  const b = win.getBounds();
  const wa = workAreaOf(b);
  dock = pickDock(b, wa);
  const vertical = dock === 'left' || dock === 'right';
  anchorRight = dock === 'right' ? true : dock === 'left' ? false : b.x + b.width / 2 > wa.x + wa.width / 2;
  const s = vertical ? winSize(VPILL_W, vPillH) : winSize(PILL_W, PILL_H);
  let x = anchorRight ? b.x + b.width - s.width : b.x;
  let y = b.y;
  if (dock === 'top') y = wa.y - M + DOCK_GAP;
  else if (dock === 'bottom') y = wa.y + wa.height - s.height + M - DOCK_GAP;
  else if (dock === 'left') x = wa.x - M + DOCK_GAP;
  else if (dock === 'right') x = wa.x + wa.width - s.width + M - DOCK_GAP;
  // 竖向胶囊可能比原来的卡片短/长，纵向要夹在工作区内
  if (vertical) y = Math.max(wa.y - M + DOCK_GAP, Math.min(y, wa.y + wa.height - s.height + M - DOCK_GAP));
  mode = 'collapsed';
  pillPos = { x, y };
  sendMode();
  tween({ x, y, ...s });
}

function expand() {
  if (mode === 'expanded' || !win) return;
  const b = win.getBounds();
  const wa = workAreaOf(b);
  const s = winSize(CARD_W, Math.min(expH, wa.height - 2 * M - 8));
  let x = anchorRight ? b.x + b.width - s.width : b.x;
  x = Math.max(wa.x - M, Math.min(x, wa.x + wa.width - s.width + M));
  const y = Math.max(wa.y - M + 4, Math.min(b.y, wa.y + wa.height - s.height + M - 4));
  mode = 'expanded';
  dock = null;
  sendMode();
  tween({ x, y, ...s });
}

// 光标是否在“卡片”内（透明的阴影边距不算）
function cursorInside() {
  if (!win) return false;
  const p = screen.getCursorScreenPoint();
  const b = win.getBounds();
  return p.x >= b.x + M && p.x < b.x + b.width - M && p.y >= b.y + M && p.y < b.y + b.height - M;
}

function armCollapse() {
  clearTimeout(collapseTimer);
  if (!canAutoHide() || mode !== 'expanded') return;
  collapseTimer = setTimeout(() => { if (!cursorInside()) collapse(); }, COLLAPSE_DELAY);
}

// 卡片（去掉透明阴影边距）是否至少有一部分落在某个显示器的工作区内。
// 分辨率/显示器变化后，保存的坐标可能已经在屏幕外，用户就找不到面板了。
function onScreen(b) {
  const need = 60; // 至少露出 60px 宽、30px 高，才算还抓得到
  return screen.getAllDisplays().some(({ workArea: w }) => {
    const ox = Math.min(b.x + b.width - M, w.x + w.width) - Math.max(b.x + M, w.x);
    const oy = Math.min(b.y + b.height - M, w.y + w.height) - Math.max(b.y + M, w.y);
    return ox >= need && oy >= 30;
  });
}
// 默认位置：主屏右上角
const defaultPos = (s) => { const wa = screen.getPrimaryDisplay().workArea; return { x: wa.x + wa.width - s.width - 4, y: wa.y + 4 }; };
// 面板跑出屏幕时拉回右上角，并清掉保存的坐标
function rescue() {
  if (!win || onScreen(win.getBounds())) return;
  const b = win.getBounds();
  win.setBounds({ ...defaultPos(b), width: b.width, height: b.height });
  cfg.x = null; cfg.y = null; saveCfg();
}

function createWindow() {
  const s = winSize(CARD_W, expH);
  const saved = cfg.x != null && cfg.y != null && onScreen({ x: cfg.x, y: cfg.y, ...s });
  const { x, y } = saved ? { x: cfg.x, y: cfg.y } : defaultPos(s);
  win = new BrowserWindow({
    x, y, ...s,
    frame: false, transparent: true, backgroundColor: '#00000000', hasShadow: false,
    resizable: false, maximizable: false, fullscreenable: false,
    skipTaskbar: true, show: false,
    alwaysOnTop: cfg.onTop,
    webPreferences: { preload: path.join(__dirname, 'preload.js'), spellcheck: false, devTools: process.argv.includes('--dev') },
  });
  win.setMenuBarVisibility(false);
  win.loadFile(path.join(__dirname, 'renderer', 'index.html'));
  win.once('ready-to-show', () => { win.show(); sendMode(); armCollapse(); });
  win.webContents.on('did-finish-load', sendMode);
  // 只记录展开态的位置，收缩态的位置是推算出来的
  win.on('moved', () => { if (mode === 'expanded' && !anim) { const [px, py] = win.getPosition(); cfg.x = px; cfg.y = py; saveCfg(); } });
  // 分辨率变化 / 拔插显示器：确保面板仍在可见范围内
  for (const ev of ['display-metrics-changed', 'display-removed']) screen.on(ev, () => setTimeout(rescue, 300));
  win.on('show', schedule);
  win.on('hide', schedule);
  win.on('minimize', schedule);
  win.on('restore', schedule);
}

function buildMenu() {
  return Menu.buildFromTemplate([
    { label: visible() ? '隐藏小组件' : '显示小组件', click: () => (visible() ? win.hide() : win.show()) },
    { label: '鼠标移开自动收缩到顶部', type: 'checkbox', checked: cfg.autoHide, click: (i) => { cfg.autoHide = i.checked; saveCfg(); sendMode(); if (!i.checked) expand(); else armCollapse(); } },
    { label: '固定展开（不自动收缩）', type: 'checkbox', checked: cfg.pinned, click: (i) => { cfg.pinned = i.checked; saveCfg(); sendMode(); if (i.checked) expand(); else armCollapse(); } },
    { label: '始终置顶', type: 'checkbox', checked: cfg.onTop, click: (i) => { cfg.onTop = i.checked; win.setAlwaysOnTop(i.checked); saveCfg(); } },
    { label: '开机自启', type: 'checkbox', enabled: canAutostart(), checked: getAutostart(), click: (i) => setAutostart(i.checked) },
    { label: '显示的 Agent', submenu: AGENT_IDS.map((id) => ({
      label: AGENT_NAMES[id], type: 'checkbox', checked: !cfg.hidden[id],
      click: (i) => { if (i.checked) delete cfg.hidden[id]; else cfg.hidden[id] = true; saveCfg(); sendMode(); },
    })) },
    { type: 'separator' },
    { label: '打开价格配置（pricing.user.json）', click: () => {
      const f = engine.pricingUser;
      if (!fs.existsSync(f)) fs.writeFileSync(f, JSON.stringify({ models: [{ match: 'gpt-6.1', in: 2.5, out: 15, cr: 0.25 }] }, null, 2));
      shell.openPath(f);
    } },
    { label: '打开套餐配置（plans.user.json）', click: () => {
      const f = engine.plansUser;
      // 模板里只放 _示例：把它改名成 agent 名（claude / codex / antigravity / workbuddy）才会生效
      if (!fs.existsSync(f)) fs.writeFileSync(f, JSON.stringify({ '_说明': '把 _示例 里的条目改成 agent 名即可覆盖自动识别的套餐；usd 为月费（美元）', '_示例': { antigravity: { name: 'AI Pro', usd: 19.99 }, workbuddy: { name: 'Pro', usd: 20 } } }, null, 2));
      shell.openPath(f);
    } },
    { label: '重新全量扫描', click: () => { try { fs.rmSync(engine.cacheFile, { force: true }); } catch {} app.relaunch(); app.exit(0); } },
    { type: 'separator' },
    { label: '退出', click: () => app.quit() },
  ]);
}

function createTray() {
  tray = new Tray(nativeImage.createFromPath(path.join(__dirname, 'assets', 'tray.png')));
  tray.setToolTip('Token 小组件');
  tray.on('click', () => { if (visible()) win.hide(); else { win.show(); expand(); armCollapse(); } });
  tray.on('right-click', () => tray.popUpContextMenu(buildMenu()));
}

// 推送与扫描节奏：可见时 2 秒扫描、1 秒推送；隐藏时 15 秒扫描、不推送
function schedule() {
  timers.forEach(clearInterval);
  timers = [];
  const show = visible();
  timers.push(setInterval(() => engine.tick().catch(() => {}), show ? 2000 : 15000));
  if (show) timers.push(setInterval(push, 1000));
  timers.push(setInterval(() => engine.save(), 30000));
  // Antigravity 额度：用户在设置里开启后，可见时每分钟问一次本地服务
  if (show && cfg.agQuota) { timers.push(setInterval(pollAg, 60000)); pollAg(); }
  if (show) push();
}

const agq = new AntigravityQuota();
function pollAg() {
  if (!cfg.agQuota || !engine) return;
  agq.fetch().then((r) => { if (r) engine.setExternalLimits('antigravity', r); }).catch(() => {});
}

function push() {
  if (!visible() || win.webContents.isDestroyed()) return;
  win.webContents.send('snap', engine.snapshot());
}

// ---- Claude 额度钩子：由设置面板一键启用/停用 ----
// 钩子脚本会被复制到用户数据目录（路径固定，升级软件不会失效；打包后脚本在 asar 里，node 也读不了）。
// 只在 ~/.claude/settings.json 没有别的 statusLine 时才写；每次改动前都先备份。
const CLAUDE_SETTINGS = process.env.TW_CLAUDE_SETTINGS || path.join(os.homedir(), '.claude', 'settings.json'); // 环境变量仅用于测试
const hookTarget = () => path.join(engine.dir, 'claude-statusline.js');

function readClaudeSettings() {
  try { return { json: JSON.parse(fs.readFileSync(CLAUDE_SETTINGS, 'utf8')) }; } catch (e) {
    return e.code === 'ENOENT' ? { json: {} } : { error: true }; // 文件存在但解析失败：不碰
  }
}
function nodeAvailable() {
  return new Promise((resolve) => execFile('node', ['-v'], { windowsHide: true, timeout: 5000 }, (err) => resolve(!err)));
}
async function claudeHookState() {
  const { json, error } = readClaudeSettings();
  const node = await nodeAvailable();
  if (error) return { state: 'unreadable', node };
  const sl = json.statusLine;
  if (!sl) return { state: 'none', node };
  return { state: String(sl.command || '').includes('claude-statusline.js') ? 'ours' : 'other', node };
}
function backupClaudeSettings() {
  if (!fs.existsSync(CLAUDE_SETTINGS)) return;
  const d = new Date();
  const p = (n) => String(n).padStart(2, '0');
  fs.copyFileSync(CLAUDE_SETTINGS, `${CLAUDE_SETTINGS}.bak-${d.getFullYear()}${p(d.getMonth() + 1)}${p(d.getDate())}-${p(d.getHours())}${p(d.getMinutes())}${p(d.getSeconds())}`);
}
async function setClaudeHook(on) {
  const st = await claudeHookState();
  if (st.state === 'unreadable' || st.state === 'other') return st; // 不覆盖用户自己的 statusLine
  const { json } = readClaudeSettings();
  if (on) {
    if (!st.node) return st; // 没有 node，钩子跑不起来
    fs.copyFileSync(path.join(__dirname, 'tools', 'claude-statusline.js'), hookTarget());
    backupClaudeSettings();
    json.statusLine = { type: 'command', command: `node "${hookTarget().replace(/\\/g, '/')}"` };
  } else {
    if (st.state !== 'ours') return st;
    backupClaudeSettings();
    delete json.statusLine;
  }
  fs.mkdirSync(path.dirname(CLAUDE_SETTINGS), { recursive: true });
  fs.writeFileSync(CLAUDE_SETTINGS, JSON.stringify(json, null, 2));
  return claudeHookState();
}

// 从 exe 取内嵌图标。不用 createThumbnailFromPath：它对 exe 只会返回系统的通用应用图标
async function extractExeIcon(file) {
  try {
    const i = await app.getFileIcon(file, { size: 'large' });
    return i.isEmpty() ? null : i.toPNG();
  } catch { return null; }
}

// 图标统一缩到最长边 ≤128px（界面最大只用到 38px，留 3 倍余量给高分屏）
function shrinkPng(buf) {
  const img = nativeImage.createFromBuffer(buf);
  const { width, height } = img.getSize();
  if (img.isEmpty() || Math.max(width, height) <= 128) return buf;
  return img.resize(width >= height ? { width: 128 } : { height: 128 }).toPNG();
}

app.whenReady().then(() => {
  app.setAppUserModelId('com.agentmonitor.widget'); // Windows 通知/任务栏分组用的固定 id，须与 electron-builder 的 appId 一致
  cfgFile = path.join(app.getPath('userData'), 'widget.json');
  engine = new Engine({ dir: path.join(app.getPath('appData'), 'token-widget') });
  loadCfg();
  nativeTheme.themeSource = ['light', 'dark'].includes(cfg.theme) ? cfg.theme : 'system';
  createWindow();
  createTray();
  schedule();
  engine.tick().catch(() => {});

  // 调试：--shot <文件> [--collapsed] [--open <agent>] 在界面稳定后截图并退出（用于无人值守验证）
  const si = process.argv.indexOf('--shot');
  if (si > 0) {
    cfg.pinned = !process.argv.includes('--collapsed');
    cfg.autoHide = false; // 截图模式不让启动后的自动收缩抢跑；需要收缩时由 --collapsed 手动触发
    // 调试：--hide <agent> 临时隐藏某个 agent（不写盘）
    const th = process.argv.indexOf('--theme');
    if (th > 0) nativeTheme.themeSource = process.argv[th + 1];
    const hi = process.argv.indexOf('--hide');
    if (hi > 0) cfg.hidden = { ...cfg.hidden, [process.argv[hi + 1]]: true };
    clearTimeout(collapseTimer);
    const open = process.argv.indexOf('--open');
    setTimeout(async () => {
      // 给透明窗口垫一个浅灰底色，截图才看得清阴影和圆角
      await win.webContents.executeJavaScript("document.documentElement.style.background='#cfd6de'");
      if (open > 0) {
        await win.webContents.executeJavaScript(`document.querySelector('[data-ag="${process.argv[open + 1]}"] .row').click()`);
        await new Promise((r) => setTimeout(r, 900));
      }
      // 调试：--settings 打开设置页再截图
      if (process.argv.includes('--settings')) {
        await win.webContents.executeJavaScript("document.querySelector('#btn-set').click()");
        await new Promise((r) => setTimeout(r, 900));
      }
      // 调试：--eval <js> 在渲染端执行一段脚本（用来模拟点击/输入），之后等 0.8 秒再截图
      const ev = process.argv.indexOf('--eval');
      if (ev > 0) {
        await win.webContents.executeJavaScript(process.argv[ev + 1]);
        await new Promise((r) => setTimeout(r, 800));
      }
      // 调试：--dock <left|right|bottom> 把窗口挪到该边附近，收缩时验证吸附
      const dk = process.argv.indexOf('--dock');
      if (dk > 0) {
        const wa = screen.getPrimaryDisplay().workArea;
        const b = win.getBounds();
        const e = process.argv[dk + 1];
        const pos = e === 'left' ? { x: wa.x - M + 30, y: 200 } : e === 'right' ? { x: wa.x + wa.width - b.width + M - 30, y: 200 } : e === 'bottom' ? { x: wa.x + Math.round((wa.width - b.width) / 2), y: wa.y + wa.height - b.height + M - 30 } : e === 'top' ? { x: wa.x + 300, y: wa.y - M + 4 } : {};
        win.setBounds({ ...b, ...pos });
        await new Promise((r) => setTimeout(r, 400)); // 等窗口真正挪到位再收缩，否则读到旧坐标
      }
      if (process.argv.includes('--collapsed')) { collapse(); await new Promise((r) => setTimeout(r, 900)); }
      // 调试：--expand-after 收缩后再展开一次，验证各边展开后的位置
      if (process.argv.includes('--expand-after')) { expand(); await new Promise((r) => setTimeout(r, 700)); }
      console.log('dock', dock, 'mode', mode, 'bounds', JSON.stringify(win.getBounds()), 'workAreas', JSON.stringify(screen.getAllDisplays().map((d) => d.workArea)));
      const img = await win.webContents.capturePage();
      fs.writeFileSync(process.argv[si + 1], img.toPNG());
      app.quit();
    }, 9000);
  }

  // 渲染端上报卡片内容高度；展开态下窗口缓动到新高度（手风琴展开时与 CSS 动画同步）
  ipcMain.on('resize', (_e, h) => {
    if (!win) return;
    const wa = workAreaOf(win.getBounds());
    expH = Math.max(120, Math.min(Math.round(h), wa.height - 2 * M - 8));
    if (mode !== 'expanded') return;
    const b = anim ? anim.to : win.getBounds();
    const s = winSize(CARD_W, expH);
    // 贴着屏幕下沿时向上长，避免内容撑出屏幕
    const y = Math.max(wa.y - M + 4, Math.min(b.y, wa.y + wa.height - s.height + M - 4));
    tween({ x: b.x, y, ...s });
  });
  // 竖向胶囊高度随显示的 agent 数量变化：收缩态且贴左/右边时，平滑调整高度
  ipcMain.on('pillsize', (_e, h) => {
    vPillH = Math.max(80, Math.min(Math.round(h), 600));
    if (mode !== 'collapsed' || (dock !== 'left' && dock !== 'right') || !win) return;
    const wa = workAreaOf(win.getBounds());
    const s = winSize(VPILL_W, vPillH);
    const y = Math.max(wa.y - M + DOCK_GAP, Math.min(pillPos.y, wa.y + wa.height - s.height + M - DOCK_GAP));
    pillPos = { x: pillPos.x, y };
    tween({ x: pillPos.x, y, ...s });
  });
  ipcMain.on('hover', (_e, inside) => {
    if (inside) { clearTimeout(collapseTimer); if (mode === 'collapsed') expand(); }
    else armCollapse();
  });
  // 图标：首次调用时从本机应用提取并缓存，之后直接复用同一个结果
  let iconsP = null;
  // 调试：--no-icons 模拟本机没装任何官方应用，用来验证首字母徽标回退
  ipcMain.handle('icons', () => process.argv.includes('--no-icons') ? {} : (iconsP ||= resolveIcons(path.join(app.getPath('userData'), 'icons'), AGENT_IDS, extractExeIcon, shrinkPng)));
  // ---- 设置面板 ----
  const getSettings = async () => {
    const user = plansMod.readUser(engine.plansUser);
    const detected = plansMod.detect('', engine.logPlans); // 传空路径 = 不含用户覆盖，只看自动识别结果
    return {
      cfg: { autoHide: cfg.autoHide, onTop: cfg.onTop, theme: cfg.theme, agQuota: cfg.agQuota, openAtLogin: getAutostart(), canAutostart: canAutostart() },
      hidden: cfg.hidden,
      claudeHook: await claudeHookState(),
      agents: AGENT_IDS.map((id) => ({
        id, name: AGENT_NAMES[id],
        user: user[id] && user[id].name ? { name: user[id].name, usd: Number(user[id].usd) || 0 } : null,
        detected: detected[id] ? { name: detected[id].name, usd: detected[id].usd } : null,
        presets: Object.entries(plansMod.defaults[id] || {}).map(([key, v]) => ({ key, name: v.name, usd: v.usd, est: !!v.est })),
      })),
    };
  };
  ipcMain.handle('settings:get', getSettings);
  ipcMain.handle('settings:set', (_e, patch = {}) => {
    const c = patch.cfg || {};
    if (typeof c.onTop === 'boolean') { cfg.onTop = c.onTop; win.setAlwaysOnTop(c.onTop); }
    if (typeof c.agQuota === 'boolean' && c.agQuota !== cfg.agQuota) { cfg.agQuota = c.agQuota; schedule(); }
    // 外观：system 跟随系统；light / dark 固定。nativeTheme 会同步改 prefers-color-scheme 和原生控件
    if (['system', 'light', 'dark'].includes(c.theme)) { cfg.theme = c.theme; nativeTheme.themeSource = c.theme; }
    if (typeof c.openAtLogin === 'boolean') setAutostart(c.openAtLogin);
    if (typeof c.autoHide === 'boolean' && c.autoHide !== cfg.autoHide) { cfg.autoHide = c.autoHide; if (!c.autoHide) expand(); else armCollapse(); }
    for (const [id, v] of Object.entries(patch.hidden || {})) {
      if (!AGENT_IDS.includes(id)) continue; // 只接受已知 agent，避免写入任意键
      if (v) cfg.hidden[id] = true; else delete cfg.hidden[id];
    }
    saveCfg();
    for (const [id, entry] of Object.entries(patch.plans || {})) {
      if (!AGENT_IDS.includes(id)) continue;
      try { plansMod.writeUser(engine.plansUser, id, entry); } catch { /* 写失败：界面下次打开会显示真实状态 */ }
    }
    if (patch.plans) { engine.plansAt = 0; engine._refreshPlans(Date.now()); }
    sendMode();
    push();
    return getSettings();
  });
  ipcMain.handle('claudehook:set', async (_e, on) => { await setClaudeHook(!!on); return getSettings(); });
  ipcMain.on('lock', (_e, v) => { locked = !!v; if (locked) clearTimeout(collapseTimer); else armCollapse(); });
  ipcMain.on('pin', () => { cfg.pinned = !cfg.pinned; saveCfg(); sendMode(); if (cfg.pinned) clearTimeout(collapseTimer); else armCollapse(); });
  ipcMain.on('menu', () => buildMenu().popup({ window: win }));
  ipcMain.on('close', () => win.hide());
});

app.on('before-quit', () => { try { engine.shutdown(); } catch {} });
app.on('window-all-closed', () => {});
app.on('second-instance', () => { if (win) { win.show(); expand(); armCollapse(); } });
