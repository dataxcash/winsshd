use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;

use anyhow::Result;
use russh::{Channel, ChannelId};
use russh::server::{Auth, Msg, Server as _, Session};
use russh::keys::PrivateKey;
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;

use crate::config::Config;
use crate::users::Users;
use crate::shell;
use crate::sftp::SftpFs;

/// 全局应用状态 (跨连接共享)
#[derive(Clone)]
pub struct App {
    pub cfg: Arc<Config>,
    pub users: Arc<Users>,
    pub host_key: PrivateKey,
    pub shutdown: CancellationToken,
}

impl russh::server::Server for App {
    type Handler = Conn;

    fn new_client(&mut self, addr: Option<SocketAddr>) -> Self::Handler {
        tracing::info!("新连接: {:?}", addr);
        Conn {
            cfg: self.cfg.clone(),
            users: self.users.clone(),
            shutdown: self.shutdown.clone(),
            authed_user: None,
            channels: Arc::new(Mutex::new(HashMap::new())),
            pty_sizes: Arc::new(Mutex::new(HashMap::new())),
            pty_masters: Arc::new(Mutex::new(HashMap::new())),
        }
    }
}

/// 单连接状态
pub struct Conn {
    cfg: Arc<Config>,
    users: Arc<Users>,
    shutdown: CancellationToken,
    authed_user: Option<String>,
    channels: Arc<Mutex<HashMap<ChannelId, Channel<Msg>>>>,
    /// channel -> (cols, rows)  由 pty_request 记录
    pty_sizes: Arc<Mutex<HashMap<ChannelId, (u32, u32)>>>,
    /// channel -> pty master (window change resize 用)
    pty_masters: Arc<Mutex<HashMap<ChannelId, Arc<tokio::sync::Mutex<Box<dyn portable_pty::MasterPty + Send>>>>>>,
}

impl russh::server::Server for Conn {
    type Handler = Conn;
    fn new_client(&mut self, _: Option<SocketAddr>) -> Self::Handler {
        Conn {
            cfg: self.cfg.clone(),
            users: self.users.clone(),
            shutdown: self.shutdown.clone(),
            authed_user: None,
            channels: self.channels.clone(),
            pty_sizes: self.pty_sizes.clone(),
            pty_masters: self.pty_masters.clone(),
        }
    }
    fn handle_session_error(&mut self, error: <Self::Handler as russh::server::Handler>::Error) {
        tracing::warn!("会话错误: {error:#}");
    }
}

impl russh::server::Handler for Conn {
    type Error = anyhow::Error;

    async fn auth_password(&mut self, user: &str, password: &str) -> Result<Auth, Self::Error> {
        let ok = self.cfg.auth.password && self.users.verify_password(user, password)?;
        if !ok {
            tracing::warn!("密码认证失败: user={user}");
            return Ok(Auth::Reject {
                proceed_with_methods: None,
                partial_success: false,
            });
        }
        tracing::info!("密码认证成功: user={user}");
        self.authed_user = Some(user.into());
        Ok(Auth::Accept)
    }

    async fn auth_publickey(
        &mut self,
        user: &str,
        key: &russh::keys::ssh_key::PublicKey,
    ) -> Result<Auth, Self::Error> {
        let openssh = key.to_string(); // "algo base64 [comment]"
        let ok = self.cfg.auth.publickey && self.users.verify_pubkey(user, &openssh)?;
        if !ok {
            tracing::warn!("公钥认证失败: user={user} key={openssh}");
            return Ok(Auth::Reject {
                proceed_with_methods: None,
                partial_success: false,
            });
        }
        tracing::info!("公钥认证成功: user={user}");
        self.authed_user = Some(user.into());
        Ok(Auth::Accept)
    }

    async fn channel_open_session(
        &mut self,
        channel: Channel<Msg>,
        reply: russh::server::ChannelOpenHandle,
        _session: &mut Session,
    ) -> Result<(), Self::Error> {
        if self.authed_user.is_none() {
            return Err(anyhow::anyhow!("未认证的通道请求"));
        }
        self.channels.lock().await.insert(channel.id(), channel);
        reply.accept().await;
        Ok(())
    }

    async fn pty_request(
        &mut self,
        channel: ChannelId,
        _term: &str,
        col_width: u32,
        row_height: u32,
        _pix_width: u32,
        _pix_height: u32,
        _modes: &[(russh::Pty, u32)],
        session: &mut Session,
    ) -> Result<(), Self::Error> {
        self.pty_sizes
            .lock()
            .await
            .insert(channel, (col_width.max(2), row_height.max(2)));
        session.channel_success(channel)?;
        Ok(())
    }

    async fn shell_request(
        &mut self,
        channel: ChannelId,
        session: &mut Session,
    ) -> Result<(), Self::Error> {
        let Some(user) = self.authed_user.clone() else {
            session.channel_failure(channel)?;
            return Ok(());
        };
        let Some(channel_obj) = self.channels.lock().await.remove(&channel) else {
            session.channel_failure(channel)?;
            return Ok(());
        };
        let size = self
            .pty_sizes
            .lock()
            .await
            .get(&channel)
            .copied()
            .unwrap_or((120, 40));
        session.channel_success(channel)?;

        let ctx = ChannelCtx {
            cfg: self.cfg.clone(),
            shutdown: self.shutdown.clone(),
            channel: channel_obj,
            masters: self.pty_masters.clone(),
        };
        tokio::spawn(async move {
            shell::run_interactive(ctx, &user, size).await;
        });
        Ok(())
    }

    async fn exec_request(
        &mut self,
        channel: ChannelId,
        command: &[u8],
        session: &mut Session,
    ) -> Result<(), Self::Error> {
        let Some(channel_obj) = self.channels.lock().await.remove(&channel) else {
            session.channel_failure(channel)?;
            return Ok(());
        };
        session.channel_success(channel)?;
        let cmd = String::from_utf8_lossy(command).to_string();
        let shell = self.cfg.shell.clone();
        tokio::spawn(async move {
            shell::run_exec(channel_obj, &shell, &cmd).await;
        });
        Ok(())
    }

    async fn subsystem_request(
        &mut self,
        channel: ChannelId,
        name: &str,
        session: &mut Session,
    ) -> Result<(), Self::Error> {
        if name == "sftp" && self.cfg.sftp_enabled {
            let Some(channel_obj) = self.channels.lock().await.remove(&channel) else {
                session.channel_failure(channel)?;
                return Ok(());
            };
            session.channel_success(channel)?;
            let root = self.cfg.sftp_root.clone();
            tokio::spawn(async move {
                let fs = SftpFs::new(root);
                russh_sftp::server::run(channel_obj.into_stream(), fs).await;
                tracing::info!("SFTP 会话结束");
            });
        } else {
            tracing::info!("拒绝子系统: {name}");
            session.channel_failure(channel)?;
        }
        Ok(())
    }

    async fn window_change_request(
        &mut self,
        channel: ChannelId,
        col_width: u32,
        row_height: u32,
        _pix_width: u32,
        _pix_height: u32,
        _session: &mut Session,
    ) -> Result<(), Self::Error> {
        self.pty_sizes
            .lock()
            .await
            .insert(channel, (col_width.max(2), row_height.max(2)));
        if let Some(m) = self.pty_masters.lock().await.get(&channel) {
            let _ = m
                .lock()
                .await
                .resize(portable_pty::PtySize {
                    rows: row_height.max(2) as u16,
                    cols: col_width.max(2) as u16,
                    pixel_width: 0,
                    pixel_height: 0,
                });
        }
        Ok(())
    }

    async fn channel_eof(
        &mut self,
        _channel: ChannelId,
        _session: &mut Session,
    ) -> Result<(), Self::Error> {
        Ok(())
    }

    async fn channel_close(
        &mut self,
        channel: ChannelId,
        _session: &mut Session,
    ) -> Result<(), Self::Error> {
        self.pty_masters.lock().await.remove(&channel);
        Ok(())
    }
}

/// 传递给 shell 任务的通道上下文
pub struct ChannelCtx {
    pub cfg: Arc<Config>,
    pub shutdown: CancellationToken,
    pub channel: Channel<Msg>,
    pub masters: Arc<Mutex<HashMap<ChannelId, Arc<tokio::sync::Mutex<Box<dyn portable_pty::MasterPty + Send>>>>>>,
}
