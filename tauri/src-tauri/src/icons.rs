//! 图标本机提取：官方图标不随仓库分发（商标 / 版权），运行时从本机已安装的应用里取，缓存到 数据目录/icons。
//! 找不到就返回 null，界面回退成首字母徽标。目前只实现 Windows 路径；其他平台直接回退。
use base64::{engine::general_purpose::STANDARD, Engine as _};
use serde_json::{json, Map, Value};
use std::io::Cursor;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

const REG_PKGS: &str = r"HKCU\Software\Classes\Local Settings\Software\Microsoft\Windows\CurrentVersion\AppModel\Repository\Packages";

/// 运行外部命令并限时等待；Windows 下不弹黑窗
fn run_capture(mut cmd: Command, secs: u64) -> Option<String> {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    let mut child = cmd.stdout(Stdio::piped()).stderr(Stdio::null()).stdin(Stdio::null()).spawn().ok()?;
    let t0 = Instant::now();
    loop {
        match child.try_wait().ok()? {
            Some(_) => break,
            None if t0.elapsed() > Duration::from_secs(secs) => {
                let _ = child.kill();
                return None;
            }
            None => std::thread::sleep(Duration::from_millis(50)),
        }
    }
    let out = child.wait_with_output().ok()?;
    Some(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// WindowsApps 目录本身不可列出，但文件按完整路径可读；包全名从注册表里查
fn appx_root(prefix: &str) -> Option<PathBuf> {
    let mut c = Command::new("reg");
    c.args(["query", REG_PKGS, "/f", &format!("{prefix}_"), "/k"]);
    let out = run_capture(c, 8)?;
    let mut names: Vec<String> = out.lines()
        .filter_map(|l| l.trim().rsplit('\\').next().map(str::to_string))
        .filter(|n| n.starts_with(&format!("{prefix}_")) && n.contains("__"))
        .collect();
    // 多个版本并存时取字典序最大的（通常是最新版）
    names.sort();
    let n = names.pop()?;
    let pf = std::env::var("ProgramFiles").unwrap_or_else(|_| r"C:\Program Files".into());
    Some(Path::new(&pf).join("WindowsApps").join(n))
}

/// 从 exe 取 128px 内嵌图标：user32 的 PrivateExtractIcons。脚本走 -EncodedCommand 内联执行
/// （不落地 .ps1，不受执行策略限制）；失败返回 None，界面回退成首字母徽标。
fn extract_exe_hires(file: &Path) -> Option<Vec<u8>> {
    let q = |s: &str| format!("'{}'", s.replace('\'', "''"));
    let out = std::env::temp_dir().join(format!("tw-icon-{}-{}.png", std::process::id(), agent_core::util::now_ms()));
    let script = format!(r#"
Add-Type -AssemblyName System.Drawing
Add-Type -TypeDefinition 'using System; using System.Runtime.InteropServices; public class TwPI {{ [DllImport("user32.dll", CharSet=CharSet.Unicode)] public static extern uint PrivateExtractIcons(string f,int i,int cx,int cy,IntPtr[] h,uint[] id,uint n,uint fl); }}'
$h = New-Object IntPtr[] 1; $id = New-Object uint32[] 1
$n = [TwPI]::PrivateExtractIcons({},0,128,128,$h,$id,1,0)
if ($n -ge 1 -and $h[0] -ne [IntPtr]::Zero) {{ [System.Drawing.Icon]::FromHandle($h[0]).ToBitmap().Save({}) }}"#,
        q(&file.to_string_lossy()), q(&out.to_string_lossy()));
    let utf16: Vec<u8> = script.encode_utf16().flat_map(|u| u.to_le_bytes()).collect();
    let mut c = Command::new("powershell");
    c.args(["-NoProfile", "-NonInteractive", "-EncodedCommand", &STANDARD.encode(utf16)]);
    run_capture(c, 15);
    let bytes = std::fs::read(&out).ok();
    let _ = std::fs::remove_file(&out);
    bytes
}

/// 图标统一缩到最长边 ≤128px（界面最大只用到 38px，留 3 倍余量给高分屏）
fn shrink_png(buf: Vec<u8>) -> Vec<u8> {
    let Ok(img) = image::load_from_memory(&buf) else { return buf };
    if img.width().max(img.height()) <= 128 {
        return buf;
    }
    let small = img.resize(128, 128, image::imageops::FilterType::Lanczos3);
    let mut out = Cursor::new(Vec::new());
    match small.write_to(&mut out, image::ImageFormat::Png) {
        Ok(()) => out.into_inner(),
        Err(_) => buf,
    }
}

fn first_existing(list: &[PathBuf]) -> Option<PathBuf> {
    list.iter().find(|f| std::fs::metadata(f).is_ok_and(|m| m.is_file())).cloned()
}

enum Src {
    Png(PathBuf),
    Exe(PathBuf),
}

/// 每个 agent 的候选来源，按顺序取第一个存在的
fn candidates() -> Vec<(&'static str, Src)> {
    let mut out = Vec::new();
    if !cfg!(windows) {
        return out;
    }
    let local = PathBuf::from(std::env::var("LOCALAPPDATA").unwrap_or_default());
    let png_in = |root: &Option<PathBuf>, names: &[&str]| -> Option<PathBuf> {
        root.as_ref().and_then(|r| first_existing(&names.iter().map(|n| r.join("assets").join(n)).collect::<Vec<_>>()))
    };
    let claude_root = appx_root("Claude");
    let codex_root = appx_root("OpenAI.Codex");
    if let Some(p) = png_in(&claude_root, &["Square44x44Logo.targetsize-96.png", "Square44x44Logo.targetsize-256.png", "Square150x150Logo.scale-200.png"]) {
        out.push(("claude", Src::Png(p)));
    }
    if let Some(p) = png_in(&codex_root, &["Square44x44Logo.targetsize-96_altform-unplated.png", "Square44x44Logo.targetsize-96.png", "Square44x44Logo.targetsize-256_altform-unplated.png"]) {
        out.push(("codex", Src::Png(p)));
    }
    if let Some(p) = first_existing(&[local.join("Programs/WorkBuddyAI/resources/app.asar.unpacked/resources/icon.png")]) {
        out.push(("workbuddy", Src::Png(p)));
    }
    if let Some(p) = first_existing(&[local.join("Programs/Antigravity/Antigravity.exe")]) {
        out.push(("antigravity", Src::Exe(p)));
    }
    out
}

fn stamp_of(file: &Path) -> Option<String> {
    let m = std::fs::metadata(file).ok()?.modified().ok()?.duration_since(std::time::UNIX_EPOCH).ok()?;
    Some(format!("{}|{}", file.to_string_lossy(), m.as_secs_f64() * 1000.0))
}

/// 解析所有 agent 的图标，返回 { id: dataURL | null }
pub fn resolve_icons(cache_dir: &Path, ids: &[&str]) -> Value {
    let mut result: Map<String, Value> = ids.iter().map(|id| (id.to_string(), Value::Null)).collect();
    // 首次运行没有缓存
    let meta_file = cache_dir.join("index.json");
    let mut meta: Map<String, Value> = std::fs::read_to_string(&meta_file).ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default();
    if std::fs::create_dir_all(cache_dir).is_err() {
        return Value::Object(result);
    }
    let cand = candidates();
    let mut dirty = false;
    for id in ids {
        let png = cache_dir.join(format!("{id}.png"));
        if let Some((_, src)) = cand.iter().find(|(i, _)| i == id) {
            let file = match src { Src::Png(f) | Src::Exe(f) => f };
            if let Some(stamp) = stamp_of(file) {
                // 来源路径或修改时间变了（应用升级）就重新提取
                if meta.get(*id).and_then(Value::as_str) != Some(&stamp) || !png.exists() {
                    let buf = match src {
                        Src::Png(f) => std::fs::read(f).ok().map(shrink_png),
                        Src::Exe(f) => extract_exe_hires(f).map(shrink_png),
                    };
                    if let Some(b) = buf.filter(|b| !b.is_empty()) {
                        if std::fs::write(&png, b).is_ok() {
                            meta.insert(id.to_string(), json!(stamp));
                            dirty = true;
                        }
                    }
                }
            }
        }
        // 应用已卸载但缓存还在：继续用缓存，不闪回字母徽标
        if let Ok(b) = std::fs::read(&png) {
            result.insert(id.to_string(), json!(format!("data:image/png;base64,{}", STANDARD.encode(b))));
        }
    }
    if dirty {
        // 写失败下次再提取
        let _ = std::fs::write(&meta_file, serde_json::to_string(&meta).unwrap_or_default());
    }
    Value::Object(result)
}
