# winsshd

用 Rust 编写的极简、低内存占用 Windows SSH + SFTP 服务端。

**单个约 2.6 MB 的可执行文件，以原生 Windows 服务运行，无外部依赖。**

English | 英文文档: **[README.md](README.md)**

---

## 功能特性

- **SSH 服务端**（基于 russh —— Warpgate 同款 SSH 库）
- **双认证方式**：公钥 + 用户名/口令（bcrypt 哈希存储）
- **交互式 shell** —— PowerShell / cmd / 任意可执行程序，经 Windows ConPTY
- **单命令执行**（`ssh 主机 "命令"`）
- **SFTP 文件传输**，根目录可配置（沙箱隔离）
- **原生 Windows 服务** —— 内置 `install` / `start` / `stop` / `uninstall`，开机自启。**不需要 NSSM**
- **体积极小** —— 约 2.6 MB 二进制，空闲内存个位数 MB
- Linux 上交叉编译即可产出，不需要 Windows 构建机

## 安全模型 —— 部署前必读

**每一个 SSH 会话都以 LocalSystem（Windows 最高权限）运行。**

winsshd **不使用 Windows 账号做认证**。用户存放在自己的 `users.toml`
文件里（口令 bcrypt 哈希、OpenSSH 公钥行）。这些"虚拟账号"只决定
**谁有资格登录**，**不约束登录后能做什么**——认证通过后，shell 直接
继承服务的身份，也就是 LocalSystem。

这是为极简而做的刻意取舍：

| 适合 | 不适合 |
|---|---|
| 你（或你的运维团队）管理自己的机器 | 需要按用户区分权限 |
| 机器在内网 | 端口暴露公网 |
| 一台机器一套管理员口令可以接受 | 需要按用户的审计日志 |

按 Windows 账号做权限模拟（`LogonUser`）已在计划中，见
[限制与路线图](#限制与路线图)。

其他说明：

- 口令以 bcrypt 哈希存储；公钥以 OpenSSH 公钥行原文存储
- 所有认证尝试（成功与失败）均记录日志
- 主机密钥（Ed25519）首次运行自动生成
- 目前仅支持 Ed25519 主机密钥

## 快速开始

在 Windows 目标机上（**管理员** PowerShell）：

```powershell
# 1. 解压到目录（例如 C:\Program Files\winsshd），然后：
.\winsshd.exe user add alice            # 创建用户，交互输入口令
.\winsshd.exe key add alice .\alice.pub # 可选：添加公钥
Copy-Item config.example.toml config.toml

# 2. 防火墙放行（换了端口或自行管理防火墙可跳过）
netsh advfirewall firewall add rule name="WinSSHD" dir=in action=allow protocol=TCP localport=2222

# 3. 安装为服务（开机自启）并启动
.\winsshd.exe install
.\winsshd.exe start
```

在你的工作电脑上：

```bash
ssh -p 2222 alice@<windows主机>       # 交互 shell
ssh -p 2222 alice@<windows主机> "dir" # 单命令
sftp -P 2222 alice@<windows主机>      # 传文件
```

## 命令一览

| 命令 | 说明 |
|---|---|
| `winsshd run [-c config.toml]` | 前台运行（服务模式为 `run --service`） |
| `winsshd install` | 注册 Windows 服务（开机自启） |
| `winsshd uninstall` | 卸载服务 |
| `winsshd start` / `stop` | 启动 / 停止服务 |
| `winsshd user add <名称> [--password <口令>]` | 创建用户（省略 `--password` 则交互输入） |
| `winsshd user list` | 列出用户（含公钥数） |
| `winsshd user del <名称>` | 删除用户 |
| `winsshd key-add <名称> <公钥文件>` | 为用户添加 OpenSSH 公钥 |

## 配置说明

`config.toml` 的查找顺序：可执行文件所在目录 → `C:\ProgramData\winsshd\` → 当前目录。
字段说明见 [config.example.toml](config.example.toml)：

```toml
listen       = "0.0.0.0:2222"   # 监听地址
shell        = "powershell.exe" # 登录 shell
sftp_enabled = true
sftp_root    = "C:/"            # SFTP 可见根目录（沙箱）
hostkey_path = "host_ed25519"   # 首次运行自动生成
log_file     = "winsshd.log"
log_level    = "info"

[auth]
password   = true
publickey  = true
users_file = "users.toml"
```

相对路径以配置文件所在目录为基准解析。

## 构建

在 Linux 上交叉编译（不需要 Windows 机器）：

```bash
rustup target add x86_64-pc-windows-gnu
apt install mingw-w64
cargo build --release --target x86_64-pc-windows-gnu
# → target/x86_64-pc-windows-gnu/release/winsshd.exe
```

或者直接推送到 `main` 分支——GitHub Actions 会在**真实 Windows 环境**
自动构建并执行完整行为测试（exec / shell / sftp / 口令认证 / 服务生命周期），
见 [.github/workflows/ci.yml](.github/workflows/ci.yml)。

## 限制与路线图

- [ ] 按 Windows 账号做权限模拟（`LogonUser`）—— 当前会话以 LocalSystem 运行
- [ ] RSA / ECDSA 主机密钥（当前仅 Ed25519）
- [ ] keyboard-interactive 认证
- [ ] 端口转发

## 许可证

Apache-2.0 —— 见 [LICENSE](LICENSE)。
