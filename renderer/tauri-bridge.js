// Tauri 版的前端桥：把 Electron preload.js 暴露的 window.widget 接口换成 Tauri 的 invoke / listen。
// 在 Electron 里 window.widget 已由 preload 注入，这里什么都不做。
(() => {
  if (window.widget || !window.__TAURI__) return;
  const { invoke } = window.__TAURI__.core;
  const { listen } = window.__TAURI__.event;
  const pending = [];
  const on = (ev, fn) => { pending.push(listen(ev, (e) => fn(e.payload))); };
  window.widget = {
    onSnap: (fn) => on('snap', fn),
    // fx：后端有系统毛玻璃时，给 body 加 fx 类（样式见 style.css），再把模式交给原来的处理函数
    onMode: (fn) => on('mode', (m) => { document.body.classList.toggle('fx', !!m.fx); fn(m); }),
    resize: (h) => invoke('resize', { h }),
    pillSize: (h) => invoke('pillsize', { h }),
    hover: (inside) => invoke('hover', { inside }),
    icons: () => invoke('icons'),
    getSettings: () => invoke('settings_get'),
    setSettings: (patch) => invoke('settings_set', { patch }),
    setClaudeHook: (on) => invoke('claudehook_set', { on }),
    lock: (v) => invoke('lock', { v }),
    pin: () => invoke('pin'),
    menu: () => invoke('menu'),
    close: () => invoke('close'),
  };
  // 无边框窗口没有系统标题栏：在顶部栏（按钮以外的区域）按下左键就拖动窗口
  document.addEventListener('mousedown', (e) => {
    if (e.button !== 0 || !e.target.closest('#top') || e.target.closest('button')) return;
    invoke('start_drag');
  });
  // 事件监听都注册好之后再通知后端“可以显示了”，避免丢掉第一帧模式 / 快照
  window.addEventListener('load', () => { Promise.all(pending).then(() => invoke('ready')); });
})();
