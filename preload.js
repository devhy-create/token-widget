const { contextBridge, ipcRenderer } = require('electron');

contextBridge.exposeInMainWorld('widget', {
  onSnap: (fn) => ipcRenderer.on('snap', (_e, s) => fn(s)),
  onMode: (fn) => ipcRenderer.on('mode', (_e, m) => fn(m)),
  resize: (h) => ipcRenderer.send('resize', h),
  pillSize: (h) => ipcRenderer.send('pillsize', h),
  hover: (inside) => ipcRenderer.send('hover', inside),
  icons: () => ipcRenderer.invoke('icons'),
  getSettings: () => ipcRenderer.invoke('settings:get'),
  setSettings: (patch) => ipcRenderer.invoke('settings:set', patch),
  setClaudeHook: (on) => ipcRenderer.invoke('claudehook:set', on),
  lock: (v) => ipcRenderer.send('lock', v),
  pin: () => ipcRenderer.send('pin'),
  menu: () => ipcRenderer.send('menu'),
  close: () => ipcRenderer.send('close'),
});
