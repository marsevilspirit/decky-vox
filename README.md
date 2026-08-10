# Decky Vox

Decky Vox 是面向 Steam Deck / Decky Loader 的本地离线语音输入插件。

> Offline push-to-talk voice typing for Steam Deck with native text input and optional auto-send.

按住 R4 说话，松开后在 Steam Deck 本地完成中文或多语言转写，并通过
Steam 自身的控制器键盘接口将文字输入当前获得焦点的文本框。正常流程不需要
手动打开屏幕键盘，也不需要点击“粘贴”。自动按 Enter 是默认关闭的高风险选项。

## 当前状态

这是 v0.1.0 实验版。纯逻辑、IPC 和构建可以在开发机验证，但 R4/R5、麦克风、
Vulkan 和 Steam 好友聊天输入必须在真实 Steam Deck 上完成验收。请先阅读下方
“真实 Steam Deck 验收边界”，再将它用于重要对话。

## 安装

1. 在 Steam Deck 上安装并启用 [Decky Loader](https://github.com/SteamDeckHomebrew/decky-loader)。
2. 从 Releases 下载 `decky-vox.zip`。不要直接下载 GitHub 的源码 ZIP。
3. 在 Decky Loader 的开发者安装入口选择该 ZIP；安装后重新打开快捷菜单。
4. 打开 Decky Vox。首次使用选择模型并明确点击下载；默认是多语言 `small`。
5. 下载完成后确认状态为 Ready。日常录音和转写不访问云端，不需要 API Key。

默认模型约 488 MB。模型不包含在插件 ZIP 中，第一次准备模型需要网络；下载文件
使用固定 HTTPS 地址、大小和 SHA-256 校验，成功后原子安装。之后的识别完全在
Steam Deck 本地进行。

## 使用

默认设置：

| 设置 | 默认值 |
| --- | --- |
| Model | `small`（多语言） |
| Language | `auto` |
| Vulkan GPU acceleration | 开启 |
| PTT mode | 按住说话（hold） |
| Primary button | R4 |
| Optional chord button | 无 |
| Output mode | `steam_input` |
| Auto-send delay | 250 ms |
| Auto-start on boot | 开启 |

最安全的操作流程：

1. 打开 Steam 快捷菜单中的好友聊天。
2. **先让目标聊天输入框获得焦点。** Decky Vox 不会寻找、点击或验证聊天框。
3. 按住 R4，状态变为 Recording 后说话。
4. 松开 R4，等待 Transcribing 完成。
5. `steam_input` 模式只输入文本，由你检查后手动发送。

可以选择第二个按键形成组合键。hold 模式下，两键都按下才开始，任意一个松开就
停止；toggle 模式下，完整按下一次开始，再完整按下一次停止，松键只负责重新布防。
重复的按下/松开事件会被去重。监听属于插件生命周期，因此关闭插件面板后仍工作。

## 输出模式与风险

- `steam_input`：把文字输入当前焦点文本框，不按 Enter。默认且推荐。
- `steam_input_send`：输入文字，等待配置的延迟，再发送 Return（HID 40）的按下与
  释放。必须在 UI 中主动确认误发风险后才能启用。
- `clipboard`：只复制到剪贴板，用作兼容模式。

**自动发送可能把内容发送到错误的窗口或聊天对象。** 焦点改变、Steam UI 延迟、
游戏覆盖层行为和系统更新都可能影响结果。250 ms 只给聊天框处理中文文本留出时间，
不是焦点或送达保证。

`SteamClient.Input.ControllerKeyboardSendText()` 返回 `void`。因此“调用没有抛异常”
只表示 Decky Vox 已提交输入请求，不能证明聊天框真的收到中文；`Sent` 也只表示文字
注入和 Return 调用未抛异常，不代表消息已经通过网络送达。这一边界只能在真实硬件上
验证。

安全规则：

- 空白转写不会输入、复制或按 Enter。
- 转写失败、旧会话结果、core 退出或协议故障不会按 Enter。
- 原生文字注入缺失或抛异常时会尝试复制到剪贴板，但绝不继续自动发送。
- 文字注入后若 Return 接口失败，不会再次复制，以免产生重复文本。
- Return 始终成对发送；即使取消、异常或卸载也会 best-effort 释放，避免按键卡住。
- 会话开始时和真正发送前都必须仍处于 `steam_input_send`，中途关闭会阻止发送。

## 状态

后端阶段为 Stopped、Setup required、Ready、Recording、Transcribing 或 Failed。
Ready 同时要求 Rust 后端已准备且前端能够注册控制器输入。最近一次输出结果单独显示为
Input completed、Sent、Copied to clipboard、No speech recognized 或 Failed，不能将
这些结果理解为 Steam 已确认送达。

## 故障排查

### 一直显示 Setup required

确认 CPU/Vulkan voxtype 二进制和所选模型已就绪。重新点击模型下载并查看具体错误；
哈希不匹配、空间不足或下载中断时，`.part` 文件不会被当成有效模型。GPU 启动失败时
插件可以回退 CPU，并会显示实际后端，不会切换到云端。

### 按 R4 没有开始录音

确认 Enable Decky Vox 已开启、状态为 Ready，并检查游戏自己的 Steam Input 布局是否
吞掉 R4/R5。尝试单键绑定排除组合键问题。控制器输入接口和具体 R4/R5 数值可能随
Steam 客户端更新变化，开发机测试不能证明实机行为。

### 无法使用内置麦克风

检查 SteamOS 的默认录音源和权限，关闭占用麦克风的程序后重试。插件以最小权限运行，
不会为了绕过权限自动提升为 root。错误 `MICROPHONE_UNAVAILABLE` 表示录音服务未就绪。

### 显示 Input completed，但输入框没有文字

先确认目标文本框在松开 PTT 到转写完成期间一直有焦点。Steam 的原生输入调用没有回执；
可切换到 `clipboard` 验证转写本身，再手动粘贴。剪贴板只是回退，不会自动点击粘贴。

### 自动发送没有发生

只有显式选择并确认 `steam_input_send`、当前会话未取消、文字非空、原生注入未抛异常且
Return 接口可用时才发送。任何不确定情况都会 fail closed。先用 `steam_input` 验证中文
注入，再在非重要对话中测试自动发送。

### Backend unavailable / Core crashed / Protocol mismatch

重新加载插件。v1 不自动重启 core，也不会重放录音请求或旧转写结果；这是为了避免崩溃
恢复后向错误焦点注入或发送文字。如果持续出现，检查 Decky 日志及 ZIP 是否包含
`bin/decky-vox-core`、`bin/voxtype` 和 `bin/voxtype-vulkan`。

## 本地处理与日志隐私

除首次下载模型外，Decky Vox 的录音和转写路径不访问网络，也不接受 API Key。Rust
core 会清除继承来的 `VOXTYPE_*` 配置覆盖，避免宿主环境把引擎、输出方式或远端端点
改掉；voxtype 以 quiet 模式运行，防止成功转写文本写入持久日志。每次会话使用权限为
`0700` 的私有会话目录，结果读取后立即删除，启动时也会清理上次异常退出遗留的转写结果。

诊断日志仍可能包含错误信息、设备或文件路径等运行元数据。提交故障日志前请自行检查，
不要把包含隐私信息的日志直接公开。

## v1 边界

Decky Vox v1 只处理本地语音识别、PTT、当前焦点文本输入、可选 Enter、剪贴板回退、
设置和状态。它不实现云端识别、好友列表、好友选择、自动打开会话、坐标点击、OCR、
AI 润色或 Steam 好友消息网络接口。项目不会调用
`ISteamFriends::ReplyToFriendMessage`，也不会尝试判断当前聊天对象。

## 架构

```text
Steam controller events
  -> TypeScript PTT state machine
  -> thin Python Decky bridge
  -> versioned NDJSON over stdio
  -> Rust decky-vox-core
  -> pinned voxtype / whisper.cpp

transcription event
  -> TypeScript OutputCoordinator
  -> Steam native text input / clipboard / optional Return
```

Rust 是设置、模型、录音/转写会话和 voxtype 进程的业务后端。Python 只保留 Decky
Loader 所需的 lifecycle/callable/event 桥；Steam 控制器和文字接口必须留在
TypeScript 前端上下文。详细设计见
[`docs/implementation-plan.md`](docs/implementation-plan.md)。

## 开发与验证

前端、Python bridge 与主机侧 Rust 测试：

```bash
pnpm install --frozen-lockfile
pnpm run typecheck
pnpm run build
pnpm run test:ts
python3 -m unittest discover -s tests/python -p 'test_*.py'
PYTHONPYCACHEPREFIX=/tmp/decky-vox-pycache python3 -m py_compile main.py

cd backend
cargo fmt --all -- --check
cargo clippy --all-targets --all-features --locked -- -D warnings
cargo test --all-targets --locked
```

Linux amd64 与 Decky ZIP：

```bash
scripts/test-backend-linux.sh
scripts/package.sh
python3 scripts/check_zip.py out/decky-vox.zip
scripts/check-linux-zip.sh out/decky-vox.zip
```

打包要求 Docker daemon 可用，并将固定版本的 Decky CLI 放在 `cli/decky`（或设置
`DECKY_CLI`）。Decky CLI 是唯一 ZIP 写入者；脚本使用目录名生成
`out/decky-vox.zip`，并检查单一顶层目录、必需文件、ELF64 x86-64 架构、执行权限、
voxtype SHA-256 及开发残留。不要把 macOS 上生成的 Mach-O 当作发布 core。
Linux 容器还会用 `readelf` / `ldd` 检查三个 ELF 的目标机器和动态依赖；通用工具链
镜像中只允许 voxtype 所需的 SteamOS 系统库 `libasound.so.2`，以及 Vulkan 版本所需的
`libvulkan.so.1` 未解析，任何其他缺失都会失败。这两个系统库仍须在真实 Steam Deck
上确认。

## 真实 Steam Deck 验收边界

以下项目必须逐项在真实 Steam Deck 上验证，容器或 macOS 单元测试不能替代：

- R4/R5 输入事件、组合键、重复事件和面板关闭后的监听。
- 内置麦克风设备、PipeWire/权限和长时间录音稳定性。
- Vulkan 模型加载与性能、CPU 回退、内存和温度表现。
- Steam 好友聊天当前焦点框的中文原生注入。
- 自动 Enter 的约 250 ms 延迟、按下/释放及不会卡键。
- SteamOS、Steam 客户端和 Decky Loader 更新后的兼容性。

还需确认游戏 Steam Input 映射是否吞掉背键、Decky CEF 剪贴板回退、插件卸载后无
core/voxtype 孤儿进程，以及 core 崩溃后旧 session 永远不会迟到输入或发送。

## 上游与许可证

Decky Vox 采用 BSD-3-Clause，并感谢：

- [Decky Loader / Decky Plugin Template](https://github.com/SteamDeckHomebrew/decky-plugin-template)
- [mimed95/decky-voxtype](https://github.com/mimed95/decky-voxtype)，审计并选择性改写其
  控制器监听、SteamOS 环境和 voxtype 生命周期思路；上游许可证为 BSD-3-Clause。
- [peteonrails/voxtype](https://github.com/peteonrails/voxtype)，固定 v0.6.5，MIT。
- [ggerganov/whisper.cpp](https://github.com/ggerganov/whisper.cpp)，MIT。

完整署名和随 ZIP 分发的许可证位于
[`defaults/THIRD_PARTY_NOTICES.md`](defaults/THIRD_PARTY_NOTICES.md) 与
[`defaults/licenses/`](defaults/licenses/)。
