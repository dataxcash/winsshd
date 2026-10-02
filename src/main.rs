mod config;
mod hostkey;
mod sftp;
mod server;
mod service;
mod shell;
mod users;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use std::path::PathBuf;
use tokio_util::sync::CancellationToken;

pub const SERVICE_NAME: &str = "WinSSHD";

#[derive(Parser)]
#[command(name = "winsshd", version, about = "极简 Rust SSH/SFTP 服务端")]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// 前台运行 (服务模式由 SCM 传入 --service)
    Run {
        #[arg(short, long)]
        config: Option<PathBuf>,
        #[arg(long, hide = true)]
        service: bool,
    },
    /// 注册 Windows 服务 (开机自启)
    Install,
    /// 卸载服务
    Uninstall,
    /// 启动服务
    Start,
    /// 停止服务
    Stop,
    /// 用户管理
    User {
        #[command(subcommand)]
        cmd: UserCmd,
    },
    /// 为用户添加公钥
    KeyAdd {
        name: String,
        /// 公钥文件 (单行 openssh 公钥)
        key_file: PathBuf,
    },
}

#[derive(Subcommand)]
enum UserCmd {
    /// 添加用户 (--password 缺省时交互输入)
    Add {
        name: String,
        #[arg(long)]
        password: Option<String>,
    },
    /// 删除用户
    Del { name: String },
    /// 列出用户
    List,
}

fn find_config(explicit: Option<&PathBuf>) -> PathBuf {
    if let Some(p) = explicit {
        return p.clone();
    }
    let mut candidates = Vec::new();
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            candidates.push(dir.join("config.toml"));
        }
    }
    candidates.push(PathBuf::from("C:/ProgramData/winsshd/config.toml"));
    candidates.push(PathBuf::from("config.toml"));
    candidates
        .into_iter()
        .find(|p| p.exists())
        .unwrap_or_else(|| PathBuf::from("config.toml"))
}

fn init_logging(cfg: &config::Config) {
    let level = match cfg.log_level.to_lowercase().as_str() {
        "debug" => tracing::Level::DEBUG,
        "warn" => tracing::Level::WARN,
        "error" => tracing::Level::ERROR,
        "trace" => tracing::Level::TRACE,
        _ => tracing::Level::INFO,
    };
    let builder = tracing_subscriber::fmt()
        .with_max_level(level)
        .with_ansi(false);
    if let Some(parent) = cfg.log_file.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    match std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&cfg.log_file)
    {
        Ok(f) => {
            builder
                .with_writer(std::sync::Mutex::new(f))
                .init();
        }
        Err(_) => builder.init(),
    }
}

/// 启动 SSH 监听循环 (阻塞直到 shutdown 或出错)
async fn server_start(config_path: Option<PathBuf>, shutdown: CancellationToken) -> Result<()> {
    let cfg_path = find_config(config_path.as_ref());
    let cfg = config::Config::load(&cfg_path)
        .with_context(|| format!("加载配置 {}", cfg_path.display()))?;
    init_logging(&cfg);
    tracing::info!("winsshd 启动: listen={} shell={}", cfg.listen, cfg.shell);

    let host_key = hostkey::load_or_generate(&cfg.hostkey_path)?;
    let users = std::sync::Arc::new(users::Users::new(cfg.auth.users_file.clone()));

    let app = server::App {
        cfg: std::sync::Arc::new(cfg.clone()),
        users,
        host_key: host_key.clone(),
        shutdown: shutdown.clone(),
    };

    let ssh_config = std::sync::Arc::new(russh::server::Config {
        keys: vec![host_key],
        inactivity_timeout: Some(std::time::Duration::from_secs(7200)),
        auth_rejection_time: std::time::Duration::from_secs(2),
        auth_rejection_time_initial: Some(std::time::Duration::ZERO),
        ..Default::default()
    });

    let listener = tokio::net::TcpListener::bind(&cfg.listen)
        .await
        .with_context(|| format!("监听 {}", cfg.listen))?;

    let mut server_app = app.clone();
    let server = russh::server::Server::run_on_socket(&mut server_app, ssh_config, &listener);
    tokio::pin!(server);
    tokio::select! {
        _ = shutdown.cancelled() => { tracing::info!("停机信号, 关闭监听"); }
        res = &mut server => { res?; }
    }
    Ok(())
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.cmd {
        Cmd::Run { config, service } => {
            #[cfg(windows)]
            if service {
                return service::dispatch();
            }
            #[cfg(not(windows))]
            let _ = service;
            let shutdown = CancellationToken::new();
            let shutdown2 = shutdown.clone();
            tokio::spawn(async move {
                let _ = tokio::signal::ctrl_c().await;
                tracing::info!("收到 Ctrl+C");
                shutdown2.cancel();
            });
            server_start(config, shutdown).await
        }
        Cmd::Install => service::install(),
        Cmd::Uninstall => service::uninstall(),
        Cmd::Start => service::start(),
        Cmd::Stop => service::stop(),
        Cmd::User { cmd } => user_cmd(cmd),
        Cmd::KeyAdd { name, key_file } => {
            let cfg = config::Config::load(&find_config(None))?;
            let line = std::fs::read_to_string(&key_file)
                .with_context(|| format!("读取 {}", key_file.display()))?;
            let line = line
                .lines()
                .find(|l| !l.trim().is_empty())
                .context("公钥文件为空")?;
            users::Users::new(cfg.auth.users_file.clone()).add_key(&name, line)?;
            println!("已为 {name} 添加公钥: {line}");
            Ok(())
        }
    }
}

fn user_cmd(cmd: UserCmd) -> Result<()> {
    let cfg = config::Config::load(&find_config(None))?;
    let users = users::Users::new(cfg.auth.users_file.clone());
    match cmd {
        UserCmd::Add { name, password } => {
            let pw = match password {
                Some(p) => p,
                None => users::prompt_password(true)?,
            };
            users.add(&name, &pw)?;
            println!("用户 {name} 已创建 (口令 bcrypt 存储)");
        }
        UserCmd::Del { name } => {
            users.del(&name)?;
            println!("用户 {name} 已删除");
        }
        UserCmd::List => {
            for u in users.list()? {
                let pw = if u.password_bcrypt.is_some() { "✓" } else { "✗" };
                println!("{:<16} 口令:{pw} 公钥:{}", u.name, u.keys.len());
            }
        }
    }
    Ok(())
}
