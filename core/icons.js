// 图标本机提取：官方图标不随仓库分发（商标/版权），运行时从本机已安装的应用里取，缓存到 userData/icons。
// 找不到就返回 null，界面回退成首字母徽标。目前只实现 Windows 路径；其他平台直接回退。
const fs = require('fs');
const path = require('path');
const { execFile } = require('child_process');

const REG_PKGS = 'HKCU\\Software\\Classes\\Local Settings\\Software\\Microsoft\\Windows\\CurrentVersion\\AppModel\\Repository\\Packages';

// WindowsApps 目录本身不可列出（EPERM），但文件按完整路径可读；包全名从注册表里查
function appxRoot(prefix) {
  return new Promise((resolve) => {
    execFile('reg', ['query', REG_PKGS, '/f', prefix + '_', '/k'], { windowsHide: true, timeout: 8000 }, (err, out) => {
      if (err) return resolve(null);
      const names = String(out).split(/\r?\n/).map((l) => l.trim().split('\\').pop()).filter((n) => n.startsWith(prefix + '_') && n.includes('__'));
      // 多个版本并存时取字典序最大的（通常是最新版）
      names.sort();
      const n = names[names.length - 1];
      resolve(n ? path.join(process.env.ProgramFiles || 'C:\\Program Files', 'WindowsApps', n) : null);
    });
  });
}

// 从 exe 取 128px 内嵌图标：user32 的 PrivateExtractIcons。Electron 的 getFileIcon 只有 32px，
// 而 createThumbnailFromPath 对 exe 只会给通用图标，所以优先用这个。
// 脚本走 -EncodedCommand 内联执行（不落地 .ps1，不受执行策略限制）；失败返回 null，由调用方降级。
function extractExeHiRes(file) {
  const q = (s) => "'" + String(s).replace(/'/g, "''") + "'";
  const out = path.join(require('os').tmpdir(), `tw-icon-${process.pid}-${Date.now()}.png`);
  const script = `
Add-Type -AssemblyName System.Drawing
Add-Type -TypeDefinition 'using System; using System.Runtime.InteropServices; public class TwPI { [DllImport("user32.dll", CharSet=CharSet.Unicode)] public static extern uint PrivateExtractIcons(string f,int i,int cx,int cy,IntPtr[] h,uint[] id,uint n,uint fl); }'
$h = New-Object IntPtr[] 1; $id = New-Object uint32[] 1
$n = [TwPI]::PrivateExtractIcons(${q(file)},0,128,128,$h,$id,1,0)
if ($n -ge 1 -and $h[0] -ne [IntPtr]::Zero) { [System.Drawing.Icon]::FromHandle($h[0]).ToBitmap().Save(${q(out)}) }`;
  const enc = Buffer.from(script, 'utf16le').toString('base64');
  return new Promise((resolve) => {
    execFile('powershell', ['-NoProfile', '-NonInteractive', '-EncodedCommand', enc], { windowsHide: true, timeout: 15000 }, () => {
      try { const b = fs.readFileSync(out); fs.rmSync(out, { force: true }); resolve(b); } catch { resolve(null); }
    });
  });
}

const firstExisting = (list) => list.find((f) => { try { return fs.statSync(f).isFile(); } catch { return false; } }) || null;

// 每个 agent 的候选来源：{ kind: 'png' | 'exe', file }，按顺序取第一个存在的
async function candidates() {
  if (process.platform !== 'win32') return {};
  const local = process.env.LOCALAPPDATA || '';
  const [claudeRoot, codexRoot] = await Promise.all([appxRoot('Claude'), appxRoot('OpenAI.Codex')]);
  const pngIn = (root, names) => (root ? firstExisting(names.map((n) => path.join(root, 'assets', n))) : null);
  const out = {};
  const claude = pngIn(claudeRoot, ['Square44x44Logo.targetsize-96.png', 'Square44x44Logo.targetsize-256.png', 'Square150x150Logo.scale-200.png']);
  if (claude) out.claude = { kind: 'png', file: claude };
  const codex = pngIn(codexRoot, ['Square44x44Logo.targetsize-96_altform-unplated.png', 'Square44x44Logo.targetsize-96.png', 'Square44x44Logo.targetsize-256_altform-unplated.png']);
  if (codex) out.codex = { kind: 'png', file: codex };
  const wb = firstExisting([path.join(local, 'Programs', 'WorkBuddyAI', 'resources', 'app.asar.unpacked', 'resources', 'icon.png')]);
  if (wb) out.workbuddy = { kind: 'png', file: wb };
  const ag = firstExisting([path.join(local, 'Programs', 'Antigravity', 'Antigravity.exe')]);
  if (ag) out.antigravity = { kind: 'exe', file: ag };
  return out;
}

// 解析所有 agent 的图标，返回 { id: dataURL | null }。
// extractExe(file) → Buffer|null 由主进程提供（需要 Electron 的 nativeImage）。
// shrink(buf) → Buffer：把过大的 png 缩到合适尺寸（有的应用图标 280KB+，不值得塞进界面）。
async function resolveIcons(cacheDir, ids, extractExe, shrink = (b) => b) {
  const result = {};
  for (const id of ids) result[id] = null;
  let meta = {};
  const metaFile = path.join(cacheDir, 'index.json');
  try { meta = JSON.parse(fs.readFileSync(metaFile, 'utf8')); } catch { /* 首次运行没有缓存 */ }
  try { fs.mkdirSync(cacheDir, { recursive: true }); } catch { return result; }

  const cand = await candidates();
  let dirty = false;
  for (const id of ids) {
    const png = path.join(cacheDir, id + '.png');
    const src = cand[id];
    try {
      if (src) {
        const stamp = src.file + '|' + fs.statSync(src.file).mtimeMs;
        // 来源路径或修改时间变了（应用升级）就重新提取
        if (meta[id] !== stamp || !fs.existsSync(png)) {
          const buf = src.kind === 'png' ? shrink(fs.readFileSync(src.file)) : (await extractExeHiRes(src.file)) || (await extractExe(src.file));
          if (buf && buf.length) { fs.writeFileSync(png, buf); meta[id] = stamp; dirty = true; }
        }
      }
      // 应用已卸载但缓存还在：继续用缓存，不闪回字母徽标
      if (fs.existsSync(png)) result[id] = 'data:image/png;base64,' + fs.readFileSync(png).toString('base64');
    } catch { /* 单个图标失败不影响其他 */ }
  }
  if (dirty) { try { fs.writeFileSync(metaFile, JSON.stringify(meta)); } catch { /* 写失败下次再提取 */ } }
  return result;
}

module.exports = { resolveIcons };
