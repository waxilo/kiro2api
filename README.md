# kiro2api

把 Kiro（AWS Kiro IDE / kiro-cli）账户的模型额度，转换成本机可用的 **Anthropic Messages** 与
**OpenAI Chat Completions** 两套标准接口。一个托盘常驻的桌面应用，打开就能用，关掉窗口也不影响正在使用接口的客户端。

- **一套额度，两种协议**：同一个端口按路径分流，Claude Code、Anthropic SDK、OpenAI SDK、各类
  兼容客户端可以同时接入，各自指定自己要用哪个模型。
- **模型与额度本地缓存**：重开应用立刻显示上次的值，随后在后台静默刷新成最新的，不用等联网。
- **应用内自动更新**：检查 → 下载（带进度）→ 安装 → 重启，全程不用手动找安装包。
- **macOS 与 Windows 双端**：Apple Silicon / Intel Mac、Windows x64 都有官方安装包。

---

## 下载

到 [Releases](https://github.com/waxilo/kiro2api/releases/latest) 取对应平台的安装包。
产物统一按 `<名称>_<版本>_<平台>` 命名：

| 平台 | 安装包 | 说明 |
| --- | --- | --- |
| Windows x64 | `kiro2api_<版本>_windows-x64.exe` | NSIS 安装包，也是自动更新的载体 |
| Windows x64 | `kiro2api_<版本>_windows-x64.msi` | 走组策略 / 静默部署时用 |
| macOS Apple Silicon | `kiro2api_<版本>_macos-arm64.dmg` | M 系列芯片 |
| macOS Intel | `kiro2api_<版本>_macos-x64.dmg` | Intel 芯片 |

`*.app.tar.gz` 与 `*.sig` 是应用内自动更新用的包与签名，`latest.json` 是更新清单，都不需要手动下载。

---

## 它是怎么工作的

```
Claude Code / Anthropic SDK ──┐
                              ├─▶ kiro2api（本机 127.0.0.1:5680）──▶ Kiro 上游
OpenAI SDK / 兼容客户端     ──┘      Anthropic ↔ Kiro 协议翻译
```

Kiro 的接口是私有的（AWS Event Stream 二进制流），并不能直接被通用客户端使用。kiro2api 在
本机起一个只监听回环地址的小服务，把两套公开协议翻译成 Kiro 的私有协议并把流式响应翻回来。
对外只暴露两套大家都认识的接口，对内复用你本机已经登录好的 Kiro 凭据。

> 认证方式是读取 Kiro IDE / CLI 已经写在本机的 SSO 凭据（`~/.aws/sso/cache/kiro-auth-token.json`），
> 并在凭据临近过期时自动续期。**应用自身不收集、不上传你的任何账号信息。**

---

## 使用

### 1. 准备 Kiro 凭据

在任意一台机器上登录过 Kiro IDE，或执行过 `kiro-cli login`，本机就会有凭据文件：

- Windows / macOS：`~/.aws/sso/cache/kiro-auth-token.json`

概览页「Kiro 凭据」卡片里的「来源」会显示它读到的凭据状态。若显示未找到凭据的提示，
先登录再回到应用点「刷新凭据」。

### 2. 启动代理

打开应用 → 点「启动代理」。启动后概览页会显示实际监听端口（默认 `5680`，被占用时自动向后找可用端口）。

### 3. 接入客户端

在左侧「双协议接入」分区里，两套协议各有一组一键复制按钮：**复制地址**、**复制 .env**、**复制 JSON**。

**Anthropic 协议**（Claude Code / Anthropic SDK）

```bash
ANTHROPIC_BASE_URL=http://127.0.0.1:5680
ANTHROPIC_AUTH_TOKEN=<在「双协议接入」复制的密钥>
```

**OpenAI 协议**（OpenAI SDK / LangChain / 兼容客户端）

```bash
OPENAI_BASE_URL=http://127.0.0.1:5680/openai/v1
OPENAI_API_KEY=<同一个密钥>
```

> 片段里**不含模型名**：同一台代理可以同时服务多个客户端，各自用各自需要的模型，
> 应用不替你选。要指定模型，在客户端自己的配置里加 `ANTHROPIC_MODEL` / `OPENAI_MODEL` 即可，
> 可选值见左侧「可用模型」分区；不指定则由上游按账户情况自行选择。

> ⚠️ OpenAI 客户端的 base_url **必须带 `/openai/v1` 前缀**。OpenAI SDK 会自己在 base_url
> 后面拼 `/chat/completions`，如果只填 `http://127.0.0.1:5680`，请求会落到 Anthropic 路由上。

健康检查：`curl http://127.0.0.1:5680/health`。

### 4. 可用模型与账户额度

左侧「可用模型」分区列出当前 Kiro 账户可用的模型，「账户额度」分区显示用量与剩余。

两者都是**只读的账户信息**，不看代理是否在运行：

- 模型清单直接向上游控制面查询，**不需要先启动代理**，点「从上游同步」即可刷新。
- 额度在应用启动时自动查询一次。

查询结果都会写入本地配置（`config.json`），所以重开应用时这两个分区立刻就是上次的值，
随后后台静默刷新成最新的 —— 全程没有加载态，也不会卡在网络上。

### 5. 关于密钥

代理密钥首次启动时自动生成并持久化，重启后不变，方便你把它配置到多个客户端里长期使用。
密钥只在本机内网生效，且服务只监听 `127.0.0.1`，同网段其它设备无法访问。
需要换密钥时：先停止代理 → 在「双协议接入」分区点「重置密钥」→ 重新启动（旧密钥立即失效，
客户端需要重新粘贴）。

---

## 应用内更新

应用启动后会自动检查更新（之后按需复查）；左侧「应用更新」分区里有版本对比与更新说明，
点「下载并安装」即可完成下载、安装与重启。

为了在国内网络下也能更新，内置了两个措施：

1. **本机更新清单代理**：应用启动时在 `127.0.0.1:47866` 提供一个 `/latest.json`，
   它会优先直连 GitHub 拉官方清单，直连失败时并行试多个镜像，并把清单里的下载地址改写成
   当前最快的镜像。
2. **下载停滞自动换镜像**：下载过程中若 15 秒没有任何字节增长，判定当前镜像不可用，
   中断并重新探测镜像后重试，最多 3 次。

更新包始终由应用内嵌的公钥做 minisign 验签，镜像即使篡改清单也无法注入任意代码。

---

## 开发

需要 Node.js 与 Rust 稳定版工具链。

```bash
npm install
npm run tauri dev          # 开发模式（前端热更新）
npm run tauri build        # 本地打包（需要签名私钥才会生成更新包）
node scripts/release.mjs --dry-run   # 预演发版：检查五处版本号是否都能对齐
```

代码结构：

```
src/                     前端（TypeScript + Vite，无框架）
  main.ts                入口：找容器 → 交给外壳接管
  core/                  无 DOM 依赖的地基
    types.ts             与后端共享的类型
    ipc.ts               后端命令的类型化封装
    format.ts            转义 / 数值格式化 / 错误文案
    platform.ts          平台判定与 body 上的平台类名
  state/                 状态层（唯一数据源，不含 DOM）
    app-state.ts         AppState 形状 + 全局 store 实例 + 派生函数
    store.ts             极简订阅式 store（写入即广播）
    actions.ts           动作编排：点击 → 调后端 → 写状态 → 发提示
  services/
    updater.ts           更新流程（清单探测 / 下载重试 / 安装重启）
  ui/
    shared/              通用片段：icons / render / toast / scroll-area（自绘滚动条）
    shell/               外壳：titlebar / rail（左导航）/ statusbar / shell（唯一渲染循环）
    views/               6 个分区视图，都是 (state) => HTML 的纯函数
  styles/                tokens → base → components → shell → views 五层
src-tauri/src/
  lib.rs                 应用装配：插件、命令注册、托盘、退出钩子
  main.rs                可执行入口
  prefs.rs               本地配置持久化（原子写 + 最小权限）
  paths.rs               数据目录 / 凭据目录 / 日志路径
  proc_guard.rs          子进程硬超时与隐藏控制台窗口
  protocol_guard.rs      模型输出里内部协议文本的清洗
  tray.rs                托盘常驻与窗口控制
  updater_accel.rs       本机更新清单代理与镜像加速
  window/                自绘窗口：窗口控制 + Windows 11 贴靠布局命中区
  kiro/
    server.rs            本地 HTTP 服务：路由、鉴权、并发、去重、SSE
    transform.rs         Anthropic ↔ Kiro 请求响应转换
    openai.rs            OpenAI 协议兼容层
    auth.rs              Kiro SSO 凭据读取与 OIDC 续期
    eventstream.rs       AWS Event Stream 二进制帧解析
    models.rs            模型名映射、effort 归一化、token 估算
    service.rs           Tauri 命令与代理生命周期（应用层）
```

协议内核移植自 **kiro2cli**（[`@aristra/kiro2cli`](https://www.npmjs.com/package/@aristra/kiro2cli)，MIT License）。

### 测试

```bash
cd src-tauri && cargo test        # 单元测试（不需要凭据）
```

另有两个需要**真实 Kiro 凭据**的端到端测试，默认被忽略，用于人工验证两套协议完整链路：

```bash
cd src-tauri
cargo test --lib kiro::server::tests::live -- --ignored --nocapture --test-threads=1
```

---

## 发布

推 tag 即触发构建，CI 会产出 macOS（ARM64 / x64）与 Windows x64 的安装包、更新包与签名，
并按 `<名称>_<版本>_<平台>` 命名后随 `latest.json` 更新清单一并发布。

```bash
node scripts/release.mjs        # 版本号 +1，提交、打 tag、推送
node scripts/release.mjs 0.2.0  # 指定版本
```

首次使用前需在仓库的 **Settings → Secrets and variables → Actions** 配置两项：

| Secret | 值 |
| --- | --- |
| `TAURI_SIGNING_PRIVATE_KEY` | `src-tauri/tauri.key` 文件的**完整内容** |
| `TAURI_SIGNING_PRIVATE_KEY_PASSWORD` | 生成密钥时设置的口令（本项目生成时为空，留空字符串即可） |

> ⚠️ `src-tauri/tauri.key` 是更新签名的私钥，已在 `.gitignore` 中排除，**绝不要提交或外传**。
> 它一旦丢失，线上版本将无法再为已安装的客户端推送更新（只能让用户手动重装）。

---

## 已知约束

- 需要本机存在有效的 Kiro 凭据；应用不会替你登录 Kiro。
- 目前只支持 macOS 与 Windows。协议内核本身与平台无关，移植到 Linux 主要是打包与
  窗口/托盘适配的工作量。
- 应用刻意**不修改任何外部客户端的配置文件**（例如 `~/.claude/settings.json`）：
  所有接入都通过一键复制配置片段由你自行粘贴，避免应用私自改动你的环境。

## License

MIT
