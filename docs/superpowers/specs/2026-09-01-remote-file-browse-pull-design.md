# 远程文件浏览 + 拉取 设计文档

- 日期:2026-09-01
- 状态:已批准(用户确认方案 A)
- 分支:`devin/rebuild-upstream`

## 1. 背景与目标

mykvm 现有文件传输是**单向推送**:控制端通过拖拽把本地文件发给客户端,经 QUIC `start/chunk/finish` 三阶段分块传输,接收方落盘到 `~/Downloads/MyKVM Transfers`。

本功能补上**反向拉取**:控制端可浏览客户端(被控机)的目录,选择文件远程拉取到控制端本地。目标是"浏览 + 选择拉取",类似远程文件管理器。

## 2. 已确认的需求决策

| 问题 | 决策 |
|---|---|
| 交互模式 | 浏览 + 选择拉取(控制端浏览客户端目录,选择文件拉取) |
| 保存位置 | 系统下载目录(复用现有接收根目录 `~/Downloads/MyKVM Transfers`) |
| 安全模型 | 配对即信任(复用现有 cluster_id + pair_secret 校验) |
| 浏览范围 | 客户端任意目录 |
| UI 入口 | 新增独立 tab"文件浏览" |
| 实现方案 | 方案 A:新增轻量 `RemoteFilePacket` 指令协议,拉取复用现有文件传输管线 |

## 3. 架构

```
控制端 (controller)                         客户端 (client)
─────────────────                          ─────────────────
前端"文件浏览"tab
  │ listRemoteDirectory(deviceId, path)
  ▼
[command] list_remote_directory ──QUIC──▶ [handle] list → 枚举目录
  ▲                                        → listResult(entries)
  │ 匹配 requestId,返回 entries            │
  └────────────────────────────────────────┘
  │ pullRemoteFiles(deviceId, paths)
  ▼
[command] pull_remote_files ──QUIC──▶ [handle] pull → 逐文件
                                              send_transfer_file
                                        (start/chunk/finish 反向推回)
  ▲                                        │
  │ 现有 handle_file_transfer_packet       ▼
  │ 落盘到 ~/Downloads/MyKVM Transfers ◀──┘
```

核心思想:**数据传输完全复用现有 `FileTransferPacket` 管线**,只是方向对调(客户端当发送方、控制端当接收方)。`RemoteFilePacket` 只承载目录浏览与拉取指令,不承载文件数据。

## 4. 协议设计

### 4.1 RemoteFilePacket(新增)

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RemoteFilePacket {
    protocol: String,        // "mykvm.remote-file.v1"
    kind: String,            // "list" | "listResult" | "pull" | "pullResult"
    request_id: String,      // 请求/响应关联 ID
    origin_id: String,       // 发送方设备 ID
    target_id: String,       // 目标设备 ID
    cluster_id: String,      // 授权:集群 ID
    pair_secret: String,     // 授权:配对密钥
    path: String,            // list:目录路径;pull:可为空
    file_paths: Vec<String>, // pull:待拉取文件路径列表
    entries: Vec<RemoteDirEntry>, // listResult:目录条目
    file_count: u64,         // pullResult:成功推回文件数
    byte_count: u64,         // pullResult:成功推回字节数
    error: Option<String>,   // listResult/pullResult:错误信息
}
```

### 4.2 RemoteDirEntry(新增)

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RemoteDirEntry {
    name: String,
    path: String,
    is_dir: bool,
    size: u64,
    modified_ms: u64,
}
```

### 4.3 常量

```rust
const REMOTE_FILE_PROTOCOL: &str = "mykvm.remote-file.v1";
```

## 5. 数据流

### 5.1 目录浏览
1. 控制端 `list_remote_directory(device_id, path)`:
   - 校验配对状态(复用 `file_transfer_target_for_device` 逻辑,或新建等价目标解析)
   - 生成 `request_id`,构造 `RemoteFilePacket{kind:"list", path}` 经 QUIC 发送
   - 注册 pending 请求(request_id → oneshot channel),等待 `listResult`(带超时,如 10s)
2. 客户端收到 `list`:
   - 校验 `cluster_id`/`pair_secret`/`target_id` 是否本地
   - `spawn_blocking` 枚举目录(`fs::read_dir`),收集 `RemoteDirEntry`
   - 回 `listResult{request_id, entries, error?}`
3. 控制端收到 `listResult`:
   - 按 `request_id` 匹配 pending,resolve 返回给 command

### 5.2 拉取
1. 控制端 `pull_remote_files(device_id, paths)`:
   - 校验配对、目标在线、路径非空
   - 发 `RemoteFilePacket{kind:"pull", file_paths}`
   - **同步等待全部文件反向传输完成**,返回 `FileTransferSummary{target_name, file_count, byte_count}`(与现有 `send_files_to_device` 语义一致,前端 await 后显示结果)
2. 客户端收到 `pull`:
   - 校验授权
   - 对每个路径:解析为客户端本地绝对路径;存在且可读 → 构造 `TransferFile` 调用现有 `send_transfer_file(&quic_transport, &local_peer.id, &target, &file)` 反向推回
   - 单个文件失败:记录日志并继续下一个(部分成功,失败数计入返回结果)
3. 控制端:
   - 现有 `handle_file_transfer_packet` 接收并落盘到 `~/Downloads/MyKVM Transfers`(无需改动)
   - `file_transfer_packet_targets_local` 校验 target_id == 控制端本地,天然支持反向
4. 客户端全部文件推完后,回 `RemoteFilePacket{kind:"pullResult", request_id, file_count, byte_count}`
5. 控制端按 `request_id` 匹配 pending,resolve 返回 `FileTransferSummary` 给前端(与现有 `send_files_to_device` 返回结构一致)

## 6. 授权与安全

- **复用配对校验**:`RemoteFilePacket` 与 `FileTransferPacket` 使用相同的 `cluster_id`/`pair_secret` 校验逻辑(提取公共函数或复制等价逻辑)
- **target 校验**:list/pull 的 `target_id` 必须指向接收端本地设备
- **路径防护**:客户端只响应"存在且可读"的路径;不做目录白名单(用户选择任意目录)
- **不引入新权限**:配对即信任,与现有文件传输一致

## 7. 后端改动清单(src-tauri/src/lib.rs)

1. 新增常量 `REMOTE_FILE_PROTOCOL`
2. 新增 struct `RemoteFilePacket`、`RemoteDirEntry`
3. 新增 pending 请求注册表:`remote_file_pending: Arc<Mutex<HashMap<String, tokio::sync::oneshot::Sender<Result<RemoteFileResponse, String>>>>>`(挂到 AppRuntime,`RemoteFileResponse` 统一承载 listResult 的 entries 与 pullResult 的 file_count/byte_count)
4. 新增 `handle_remote_file_packet` 挂到现有 stream 分发链(在 clipboard 之后)
   - `list` → `spawn_blocking` 枚举 → 回 `listResult`
   - `pull` → 逐文件 `send_transfer_file` 反向推回 → 回 `pullResult`(带 request_id)
   - `listResult` / `pullResult` → 按 request_id 匹配 pending 并 resolve
5. 新增 command `list_remote_directory(device_id, path)`(await 超时)
6. 新增 command `pull_remote_files(device_id, paths)`
7. invoke_handler 注册两个新 command
8. 从 stream 分发链正确转发 `RemoteFilePacket`(新增与 `handle_clipboard_packet` 平行的分支)

## 8. 前端改动清单

### src/desktopApi.ts
- `export interface RemoteDirEntry { name; path; isDir; size; modifiedMs }`
- `export async function listRemoteDirectory(deviceId: string, path: string): Promise<RemoteDirEntry[]>`
- `export async function pullRemoteFiles(deviceId: string, paths: string[]): Promise<void>`

### src/App.tsx
- WORKSPACE_TABS / CLIENT_TABS 增加 `{ id: "files" }`(与 logs/sync 并列)
- 新增 state:`remoteBrowseDeviceId`、`remoteBrowsePath`、`remoteBrowseEntries`、`remoteBrowseLoading`、`remoteBrowseError`、`remotePullMessage`
- 新增 useEffect:切到 files tab 且选了设备 → 加载根目录
- 新增 files tab JSX:设备下拉 → 面包屑导航 → 条目列表(文件夹可进入,文件可拉取)→ 拉取结果提示
- 复用现有 `.log-panel`/`.log-container` 等样式 + 少量新增样式(面包屑、条目行)

### src/i18n.ts
- cn/en 各新增 `files` 文案块:title、subtitle、selectDevice、loading、error、empty、open、pull、pulling、pulled、refresh、goUp

### src/App.css
- 新增文件浏览器样式:面包屑、条目行(名称/大小/时间)、选中态、拉取按钮

## 9. 错误处理

| 场景 | 行为 |
|---|---|
| 未配对 / 配对信息缺失 | command 返回 `Err("当前设备尚未完成配对...")` |
| 目标设备不在线 / 版本过旧 | 复用 `file_transfer_target_for_device` 的报错文案 |
| list 超时(10s) | command 返回 `Err("远程目录列表超时")` |
| 目录不存在 / 不可读 | listResult 携带 `error`,前端显示 |
| pull 中某文件不存在/不可读 | 跳过并记录日志,继续其余文件(部分成功) |
| 反向传输中途失败 | 现有 send 失败路径报错,前端显示拉取结果 |

## 10. 测试

1. `RemoteFilePacket` 编解码往返测试
2. 目录枚举测试:空目录 / 含文件与子目录 / 不存在的路径
3. 反向文件传输落盘测试(仿 `file_transfer_writes_chunked_file_to_receive_root`):构造 pull 场景,客户端 send、控制端接收落盘
4. 授权拒绝测试:错误 cluster_id/pair_secret 被拒
5. 错误路径:目录不存在 → error 字段;文件不可读 → 跳过

## 11. 范围外(明确不做)

- 断点续传/传输进度百分比(现有 FileTransferPacket 无此能力,保持一致性)
- 目录整包递归拉取(仅逐文件拉取;文件夹本身不可拉取,只能进入后选文件)
- 客户端侧 UI(客户端只是被浏览方,不展示任何界面)
- 大文件并发传输(串行逐文件,避免 QUIC 流争用)
- 文件删除/移动/重命名等远程管理操作
