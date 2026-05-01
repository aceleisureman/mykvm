# MyKVM

MyKVM 是一个跨平台软件 KVM 原型，用于在同一个可信局域网内，让多台机器共享一套键盘、鼠标和文本剪贴板。

项目基于 Tauri 2、Rust、React 和 TypeScript 构建。第一个版本面向 Windows、macOS 和 Linux 桌面端。

[English README](./README.md)

## 功能

- 支持 Server / Client 两种工作模式。
- 支持局域网设备发现。
- 支持通过主机名或 IP 手动连接设备。
- 支持本机显示器检测和多显示器布局编辑。
- 通过单个 UDP 传输端口共享键盘和鼠标输入。
- 通过同一个传输端口同步文本剪贴板。
- 支持浅色、深色和跟随系统主题。
- 支持英文和简体中文界面。
- 支持托盘隐藏和恢复主窗口。

## 当前状态

MyKVM `v0.1.0` 是第一个实验版本，适合在本地可信网络中测试和迭代，但还没有面向不可信网络做生产级加固。

- 许可证：MIT
- 默认传输：UDP `47833`
- 剪贴板单次载荷上限：256 KB
- 安全模型：可信局域网原型
- 暂未包含：设备配对、身份认证、加密和生产级传输加固

请不要把传输端口暴露到公网或不可信网络。

## 协议

MyKVM 使用一个可配置的 UDP 传输端口，并通过轻量协议标记区分不同类型的数据。

| 默认端口 | 协议标记 | 用途 |
| --- | --- | --- |
| UDP `47833` | `mykvm.discovery.v1` | 局域网发现、设备探测/回应、主机信息和屏幕元数据 |
| UDP `47833` | `mykvm.input.v1` | 鼠标移动、鼠标按键、滚轮和键盘事件 |
| UDP `47833` | `mykvm.clipboard.v1` | 文本剪贴板同步 |

端口可以在设置中固定。自动模式会优先使用 UDP `47833`，被占用时尝试附近端口，必要时交给系统分配随机 UDP 端口。设备会广播当前使用的 `transportPort`，因此局域网发现和手动添加都能连接到实际端口。

## 环境要求

- Node.js 22+
- Rust stable
- 平台桌面工具链：
  - Windows：Microsoft C++ Build Tools
  - macOS：Xcode Command Line Tools
  - Linux：WebKitGTK 和 appindicator 开发包

## 开发

安装依赖：

```bash
npm install
```

运行 Web UI：

```bash
npm run dev
```

运行 Tauri 桌面端：

```bash
npm run tauri:dev
```

构建但不打安装包：

```bash
npm run tauri:build
```

构建桌面安装包：

```bash
npm run tauri:bundle
```

## 平台辅助脚本

Windows：

```powershell
powershell -ExecutionPolicy Bypass -File .\scripts\check-dev-env.ps1
powershell -ExecutionPolicy Bypass -File .\scripts\run-tauri-dev.ps1
```

macOS 和 Linux：

```bash
sh scripts/check-dev-env.sh
sh scripts/run-tauri-dev.sh
```

macOS 的输入捕获和注入需要在系统设置中授权 Accessibility 和 Input Monitoring。

## 验证

提交 PR 或发布版本前建议运行：

```bash
npm run build
npm run lint
cargo check --manifest-path src-tauri/Cargo.toml
```

## 发布

Git 本身只负责保存源码历史和推送提交。真正的打包、编译和生成安装包由 GitHub Actions 在 GitHub runner 上完成。

Release 工作流会监听 `main` 分支推送：

- `feat:` 发布下一个 minor 版本，例如 `v0.1.0` 到 `v0.2.0`。
- `fix:` 发布下一个 patch 版本，例如 `v0.1.0` 到 `v0.1.1`。
- 其他前缀只运行常规检查，不发布版本。
- 如果仓库还没有任何 release tag，第一个 `feat:` 或 `fix:` 推送会发布 `v0.1.0`。

示例：

```bash
git commit -m "feat: initial desktop release"
git push origin main
```

工作流会自动创建 git tag，构建 macOS、Windows 和 Linux 安装包，然后发布到 GitHub Release。

## 项目结构

| 路径 | 用途 |
| --- | --- |
| `src/App.tsx` | React 桌面控制台主界面 |
| `src/desktopApi.ts` | 前端到 Tauri 命令的桥接层 |
| `src/layout.ts` | 显示器布局变换和邻接逻辑 |
| `src/runtime.ts` | 运行时状态类型 |
| `src-tauri/src/lib.rs` | Tauri 命令、设备发现、剪贴板、应用状态和性能采样 |
| `src-tauri/src/input.rs` | 输入捕获、传输和注入运行时 |
| `scripts/` | 开发和构建辅助脚本 |

## 贡献

欢迎提交 issue 和 pull request。改动请保持聚焦；如果影响协议行为，请在文档中说明；如果触及共享运行时代码，请同时验证 Web 构建和 Tauri 后端。

提交前缀和版本规则见 [CONTRIBUTING.md](./CONTRIBUTING.md)。

## 许可证

MIT。见 [LICENSE](./LICENSE)。
