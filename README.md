# winsshd

A minimal, memory-efficient SSH + SFTP server for Windows, written in Rust.

**Single ~2.6 MB executable. Runs as a native Windows service. No external dependencies.**

文档 | Docs: **[简体中文](README.zh-CN.md)**

---

## Features

- **SSH server** (russh — the same SSH library used by Warpgate)
- **Two authentication methods**: public key and username/password (bcrypt-hashed)
- **Interactive shell** — PowerShell / cmd / any executable, via Windows ConPTY
- **Single command execution** (`ssh host "command"`)
- **SFTP file transfer** with a configurable root directory
- **Native Windows service** — `install` / `start` / `stop` / `uninstall` built in, auto-start on boot. No NSSM needed.
- **Tiny footprint** — ~2.6 MB binary, single-digit MB RAM at idle
- Cross-compiled from Linux; no Windows build machine required

## Security model — read this before deploying

**Every SSH session runs as `LocalSystem` (the highest Windows privilege level).**

winsshd does **not** use Windows accounts for authentication. Users are stored in
its own `users.toml` file (bcrypt-hashed passwords, OpenSSH public keys). These
"virtual users" only decide *who may log in* — they do **not** constrain *what the
session can do*. Once authenticated, the shell inherits the service's identity,
which is LocalSystem.

This is a deliberate trade-off for simplicity:

| Use this if… | Avoid this if… |
|---|---|
| You (or your ops team) administer your own machines | You need per-user privilege separation |
| The box is on an internal network | The port is exposed to the public internet |
| A single set of admin credentials per box is acceptable | You need per-user audit trails |

Per-user Windows account impersonation (via `LogonUser`) is planned — see
[Limitations & roadmap](#limitations--roadmap).

Other notes:

- Passwords are stored bcrypt-hashed; keys are stored as OpenSSH public key lines.
- All authentication attempts are logged (success and failure).
- The host key (Ed25519) is generated automatically on first run.
- Currently only Ed25519 host keys are supported.

## Quick start

On the Windows machine (PowerShell **as Administrator**):

```powershell
# 1. Extract the zip to a directory, e.g. C:\Program Files\winsshd, then:
.\winsshd.exe user add alice            # prompt for password
.\winsshd.exe key add alice .\alice.pub # optional: add a public key
Copy-Item config.example.toml config.toml

# 2. Open the firewall (skip if you use another port / manage it elsewhere)
netsh advfirewall firewall add rule name="WinSSHD" dir=in action=allow protocol=TCP localport=2222

# 3. Install as a service (auto-start on boot) and launch
.\winsshd.exe install
.\winsshd.exe start
```

From your workstation:

```bash
ssh -p 2222 alice@<windows-host>       # interactive shell
ssh -p 2222 alice@<windows-host> "dir" # single command
sftp -P 2222 alice@<windows-host>      # file transfer
```

## Command reference

| Command | Description |
|---|---|
| `winsshd run [-c config.toml]` | Run in foreground (service mode uses `run --service`) |
| `winsshd install` | Register the Windows service (auto-start) |
| `winsshd uninstall` | Remove the service |
| `winsshd start` / `stop` | Start / stop the service |
| `winsshd user add <name> [--password <pw>]` | Create a user (interactive prompt if `--password` omitted) |
| `winsshd user list` | List users (key counts) |
| `winsshd user del <name>` | Delete a user |
| `winsshd key-add <name> <pubkey-file>` | Add an OpenSSH public key line for a user |

## Configuration

`config.toml` is looked up next to the executable, then in
`C:\ProgramData\winsshd\`, then the current directory. See
[config.example.toml](config.example.toml):

```toml
listen       = "0.0.0.0:2222"   # bind address
shell        = "powershell.exe" # login shell
sftp_enabled = true
sftp_root    = "C:/"            # SFTP-visible root (sandboxed)
hostkey_path = "host_ed25519"   # auto-generated on first run
log_file     = "winsshd.log"
log_level    = "info"

[auth]
password   = true
publickey  = true
users_file = "users.toml"
```

Relative paths are resolved against the config file's directory.

## Building

Cross-compile from Linux (no Windows machine needed):

```bash
rustup target add x86_64-pc-windows-gnu
apt install mingw-w64
cargo build --release --target x86_64-pc-windows-gnu
# → target/x86_64-pc-windows-gnu/release/winsshd.exe
```

Or just push to `main` — GitHub Actions builds and runs the full behavioral
test suite (exec / shell / sftp / password auth / service lifecycle) on a real
Windows runner. See [.github/workflows/ci.yml](.github/workflows/ci.yml).

## Limitations & roadmap

- [ ] Per-user Windows account impersonation (`LogonUser`) — sessions currently run as LocalSystem
- [ ] RSA / ECDSA host keys (Ed25519 only for now)
- [ ] Keyboard-interactive auth
- [ ] Port forwarding

## License

Apache-2.0 — see [LICENSE](LICENSE).
