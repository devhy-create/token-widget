// 生成原创应用图标 build/icon.png（512×512）。
// 用法：node_modules\electron\dist\electron.exe scripts/make-icon.js
// 用 Electron 离屏渲染一段 SVG，不依赖任何第三方图像库；不使用任何官方 agent 的图标。
const { app, BrowserWindow } = require('electron');
const fs = require('fs');
const path = require('path');

const SIZE = 512;
// 圆角方块底 + 额度圆环（约 72%）+ 三根上升的速度条，对应「额度 + token 速度」
const svg = `<svg xmlns="http://www.w3.org/2000/svg" width="${SIZE}" height="${SIZE}" viewBox="0 0 512 512">
  <defs>
    <linearGradient id="bg" x1="0" y1="0" x2="1" y2="1">
      <stop offset="0" stop-color="#5B8CFF"/><stop offset="1" stop-color="#6A3DF0"/>
    </linearGradient>
    <linearGradient id="arc" x1="0" y1="0" x2="1" y2="1">
      <stop offset="0" stop-color="#7CF0C8"/><stop offset="1" stop-color="#3DDC97"/>
    </linearGradient>
  </defs>
  <rect x="16" y="16" width="480" height="480" rx="112" fill="url(#bg)"/>
  <circle cx="256" cy="256" r="150" fill="none" stroke="#fff" stroke-opacity="0.22" stroke-width="36"/>
  <circle cx="256" cy="256" r="150" fill="none" stroke="url(#arc)" stroke-width="36" stroke-linecap="round"
          stroke-dasharray="679 943" transform="rotate(-90 256 256)"/>
  <rect x="204" y="272" width="26" height="48" rx="13" fill="#fff"/>
  <rect x="243" y="240" width="26" height="80" rx="13" fill="#fff"/>
  <rect x="282" y="206" width="26" height="114" rx="13" fill="#fff"/>
</svg>`;

app.disableHardwareAcceleration();
app.whenReady().then(async () => {
  const win = new BrowserWindow({ width: SIZE, height: SIZE, show: false, transparent: true, frame: false, webPreferences: { offscreen: true } });
  const html = `<html><body style="margin:0;background:transparent">${svg}</body></html>`;
  await win.loadURL('data:text/html;charset=utf-8,' + encodeURIComponent(html));
  await new Promise((r) => setTimeout(r, 300));
  const img = await win.webContents.capturePage({ x: 0, y: 0, width: SIZE, height: SIZE });
  const out = path.join(__dirname, '..', 'build', 'icon.png');
  fs.mkdirSync(path.dirname(out), { recursive: true });
  fs.writeFileSync(out, img.toPNG());
  console.log('已生成', out, img.getSize());
  app.quit();
});
