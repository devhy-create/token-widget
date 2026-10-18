# Agent Monitor（Agent 监控）

[English](README.en.md) · 中文

一个 Windows 桌面悬浮小组件：在同一块屏幕上实时显示 **Claude / Codex / Antigravity / WorkBuddy / ZCode / OpenCode** 的 token 速度（分模型）、5 小时 / 每周额度、今日花费与 token 用量。苹果风格、占用低，鼠标移开自动收缩成胶囊（可贴顶 / 底 / 左 / 右），移入即展开。

![截图](docs/screenshot.png)

> 本项目与 Anthropic、OpenAI、Google、腾讯等公司没有任何隶属或合作关系；文中出现的产品名仅用于说明数据来源，相关商标归各自所有者。

## 功能

- 多个 agent 同屏：今日花费、token、当前模型；有额度数据的显示 5h / 周圆环
- 实时速度：1 个模型在生成时显示大号 tok/s，多个模型同时生成时逐个列出
- 点开某个 agent：近 14 天柱状图、本周期 / 月底推算、订阅回本倍数、分模型明细
- 自动收缩成胶囊（空闲时显示 5h 百分比，生成时显示速度），贴边后竖向 / 横向自适应
- 只显示本机检测到的 agent，可在设置里手动隐藏
- 浅色 / 深色 / 跟随系统；设置面板里改套餐、开机自启、始终置顶等

## 安装

到 [Releases](../../releases) 下载：

| 文件 | 说明 |
| --- | --- |
| `Agent.Monitor_x.y.z_x64-setup.exe` | 安装版（约 4MB，当前用户安装，无需管理员），支持开机自启；内置 WebView2 引导程序，系统缺 WebView2 时会联网自动安装 |
| `Agent-Monitor-x.y.z-portable-x64.exe` | 单文件便携版（约 6MB），双击即用，需要系统已装 WebView2（Win11 自带） |

> 首次运行 Windows SmartScreen 可能提示“未知发布者”（项目没有代码签名证书），点“更多信息 → 仍要运行”即可。

### 从源码构建

需要 Rust 稳定版和 Node.js（Node 只用来调用 Tauri CLI 打包）：

```bash
cd tauri
cargo test -p agent_core       # 数据引擎测试
cargo run -p agent-monitor     # 开发运行
npm install && npx tauri build # 生成 NSIS 安装包：target/release/bundle/nsis/
```

## 使用与设置

- 托盘图标：左键显示 / 隐藏，右键菜单有置顶、开机自启、在资源管理器中显示价格 / 套餐配置文件、重新全量扫描、退出
- 拖动顶部标题栏移动位置（会记住）；窗口靠近屏幕边缘（< 60px）收缩时会吸附到该边
- 右上角图钉：固定展开，不自动收缩
- 齿轮：每个 agent 的套餐（自动识别 / 预设 / 自定义月费）、显示开关、外观、开机自启、自动收缩、始终置顶、读取 Antigravity 额度、Claude 额度钩子

### 各 agent 的数据来源

| Agent | 来源 | 额度 |
| --- | --- | --- |
| Claude | `~/.claude/projects/**/*.jsonl` | 需启用 statusLine 钩子（见下） |
| Codex | `~/.codex/sessions`、`archived_sessions` | 日志自带 5h / 周额度 |
| Antigravity | `~/.gemini/antigravity/brain/*/…/transcript_full.jsonl` | 可选：读取本机语言服务（仅 5h） |
| WorkBuddy | `~/.workbuddy-ai/projects/**/*.jsonl` | 本机读不到积分 |
| ZCode | `~/.zcode/cli/db/db.sqlite`（只读） | 本机无额度数据；格式依据社区文档，未在 ZCode 真机上实测 |
| OpenCode | `~/.local/share/opencode/opencode*.db`（只读，1.14+） | 本机无额度数据；格式依据社区文档，未在真机上实测 |

没有安装的 agent 不会显示：界面、设置页和托盘菜单都只列出本机检测到数据的 agent。

计划支持：DeepSeek Harness（`dsh`，数据在 `~/.dsh/sessions/`）。它还在开发者预览阶段、没有公开的日志格式文档，等有人贴出真实的 session 文件样例（请先脱敏）再做，欢迎提 issue。

## 隐私

- **所有数据只在本机处理，不上传任何内容，也没有任何遥测。**
- 只读取上表中的本地日志，缓存和配置存放在 `%APPDATA%\token-widget\`。
- **Antigravity 额度（可选，默认关闭）**：开启后会读取 Antigravity 语言服务进程的命令行参数以获得本机接口令牌，只访问 `127.0.0.1`；令牌只保存在内存，不打印、不写盘。这是未公开的内部接口，可能随版本变化，失败时静默回退。
- **Claude 额度钩子（可选）**：在设置里启用后，会修改 `~/.claude/settings.json` 的 `statusLine`，让 Claude Code 调用本程序的 `--statusline` 子命令（不需要 Node）。修改前会备份为 `settings.json.bak-<时间>`，且**不会覆盖你已有的 statusLine**。钩子只落盘额度百分比和重置时间。
- 套餐识别只提取套餐字段（读取 `~/.claude.json` 的账号类型、Codex `auth.json` 里令牌的套餐声明），不保存、不输出任何令牌或邮箱。

## 费用口径

**界面上的金额是按各家 API 公开价折算的“等值金额”，不是实际扣款。** 订阅用户并不会按这个数字付费，它只用来衡量用量与订阅是否“回本”。`≈` 表示价格为估算（部分新模型没有可核实的公开价）。价格表见 `tauri/core/data/pricing.default.json`，可在 `%APPDATA%\token-widget\pricing.user.json` 覆盖，保存即生效；套餐月费见 `tauri/core/data/plans.default.json`，可在设置面板或 `plans.user.json` 覆盖。

其它口径：

- 速度 = 近 60 秒输出 token ÷ 生成耗时（耗时取日志时间戳差；Antigravity 仅秒级精度，是估算值）；20 秒内有请求算“生成中”
- Claude 输出 = 日志 `output_tokens` + `thinking_tokens`（与官方汇总对账吻合）

## 已知限制

- **Claude 的 5h / 周额度只有命令行版 Claude Code 会触发钩子，桌面 App 不会**——这是 Claude Code 桌面版的限制。数据超过 10 分钟未更新时会提示“额度数据更新于 x 前”。
- Antigravity 只有 5 小时窗口，没有周额度。
- WorkBuddy 的积分余额只在云端，本机日志里没有；界面显示“月费 + 近 30 天折算”代替。
- 仅在 Windows 上测试。图标提取、Antigravity 额度、钩子管理里的注册表 / PowerShell 部分只在 Windows 可用，其他平台会回退。
- 官方图标不随仓库分发：运行时从本机已安装的应用提取并缓存到 `%APPDATA%\token-widget\icons`，找不到就显示首字母徽标；仅供本地使用。

## 开发

```
renderer/            界面（原生 JS，无框架）；tauri-bridge.js 把 Tauri 的 invoke / 事件包装成 window.widget
tauri/core/          数据引擎 agent_core（纯 Rust，不依赖窗口）：采集器、去重聚合、价格与套餐、Antigravity 额度、statusLine 钩子
tauri/core/data/     默认价格表与套餐表（编译时嵌入）
tauri/core/tests/    夹具测试
tauri/src-tauri/     桌面壳：透明窗、收缩 / 贴靠动画、毛玻璃、托盘、设置、图标提取
```

命令行对账（输出完整快照 JSON，`--fresh` 忽略缓存全量重扫，不覆盖正式缓存）：

```bash
cd tauri
cargo run -q --release -p agent_core --example cli -- --fresh --out snap.json
```

### 新增一个采集器

在 `tauri/core/src/collectors.rs` 里实现 `Collector`，并加进 `all_collectors()`：

```rust
pub struct Mine { home: PathBuf }

impl Collector for Mine {
    fn name(&self) -> &'static str { "myagent" }
    fn discover(&self) -> Vec<PathBuf> { /* 日志文件路径 */ }
    // st 是该文件的持久化状态（至少有 st.off 偏移）；用 sink.emit(Event { .. }) 上报事件
    fn scan(&self, file: &Path, st: &mut FileState, sink: &mut dyn Sink, now: i64) -> io::Result<ReadResult> {
        /* 用 read_lines 从 st.off 增量读取并解析 */
    }
}
```

统一事件字段：`Event {agent, ts, model, sid, key, input, cr, cw5, cw1, out, rs, dur}`——`input` 未命中缓存的输入、`cr` 缓存读、`cw5/cw1` 5 分钟 / 1 小时缓存写、`out` 输出（含思考）、`rs` 其中的思考 token、`dur` 本次生成耗时（毫秒，未知填 0）；`key` 必须稳定且全局唯一，引擎靠它去重。然后在 `plans.rs` 的 `AGENTS`、`data/plans.default.json`、`src-tauri/src/settings.rs` 的 `AGENT_IDS` 和 `renderer/app.js` 的 `AGENTS` 里注册，并在 `tauri/core/tests/` 加一个夹具测试。

### 调试开关（环境变量）

- `TOKEN_WIDGET_DIR=<目录>`：换一个数据目录（测试 / 便携使用）
- `TW_BLUR=1|0`：强制开 / 关毛玻璃
- `TW_TEXT_SCALE=1.1`：模拟 Windows“文本大小”110%
- `TW_NO_ICONS=1`：不提取本机应用图标，全部显示首字母徽标（拍公开截图用）
- `TW_PERF=1`：在 stderr 打印收缩 / 展开动画的帧耗时
- 仅调试构建：`TW_DEBUG_STEPS="collapse;;expand;;dock:left;;js:<脚本>"` 按步骤自动操作窗口，`TW_DEBUG_NOHOVER=1` 忽略真实鼠标进出

## 许可证

[MIT](LICENSE)
