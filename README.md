# winsshd — 极简 Rust SSH/SFTP 服务端 (Windows 原生服务)

## 子命令
  winsshd run [-c config.toml]   前台/服务运行
  winsshd install                注册 Windows 服务 (自动启动)
  winsshd uninstall              卸载服务
  winsshd start / stop           启停服务
  winsshd user add <name>        添加用户 (交互输入口令, bcrypt 存储)
  winsshd user list / del <name> 用户管理
  winsshd key add <name> <pubkey-file>  为用户添加公钥

## 用户库 users.toml
  [[user]]
  name = "fila"
  password_bcrypt = "$2b$12$..."
  keys = ["ssh-ed25519 AAAA... comment"]

## 构建 (ryzenSvr 交叉编译)
  ./build.sh build --release --target x86_64-pc-windows-gnu
  产物: /data/dev/winsshd-target/x86_64-pc-windows-gnu/release/winsshd.exe
