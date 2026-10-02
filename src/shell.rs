use std::sync::Arc;

use russh::server::Msg;
use russh::{Channel, ChannelMsg};
use tokio::sync::mpsc;

use crate::server::ChannelCtx;

const IO_BUF: usize = 64 * 1024;

/// 交互式 shell: ConPTY(Windows) / unix pty, 双向泵
/// 数据路径: 客户端→wait() Data→pty 写线程;  pty 输出线程→wait 循环→data_bytes→客户端
pub async fn run_interactive(mut ctx: ChannelCtx, _user: &str, size: (u32, u32)) {
    let cid = ctx.channel.id();
    tracing::info!("交互 shell 启动: channel={cid} shell={}", ctx.cfg.shell);

    let system = portable_pty::native_pty_system();
    let pair = match system.openpty(portable_pty::PtySize {
        rows: size.1 as u16,
        cols: size.0 as u16,
        pixel_width: 0,
        pixel_height: 0,
    }) {
        Ok(p) => p,
        Err(e) => {
            tracing::error!("PTY 分配失败: {e}");
            let _ = ctx.channel.close().await;
            return;
        }
    };

    let mut cmd = portable_pty::CommandBuilder::new(&ctx.cfg.shell);
    cmd.env("TERM", "xterm-256color");
    let mut child = match pair.slave.spawn_command(cmd) {
        Ok(c) => c,
        Err(e) => {
            tracing::error!("shell 启动失败 ({}): {e}", ctx.cfg.shell);
            let _ = ctx.channel.close().await;
            return;
        }
    };
    let mut reader = match pair.master.try_clone_reader() {
        Ok(r) => r,
        Err(e) => {
            tracing::error!("PTY reader 获取失败: {e}");
            let _ = ctx.channel.close().await;
            return;
        }
    };
    let mut writer = match pair.master.take_writer() {
        Ok(w) => w,
        Err(e) => {
            tracing::error!("PTY writer 获取失败: {e}");
            let _ = ctx.channel.close().await;
            return;
        }
    };
    drop(pair.slave); // 已被子进程继承

    // 泵1: pty 输出 -> mpsc (阻塞读线程)
    let (out_tx, mut out_rx) = mpsc::channel::<Vec<u8>>(8);
    std::thread::spawn(move || {
        let mut buf = vec![0u8; IO_BUF];
        loop {
            match std::io::Read::read(&mut reader, &mut buf) {
                Ok(0) => break,
                Ok(n) => {
                    if out_tx.blocking_send(buf[..n].to_vec()).is_err() {
                        break;
                    }
                }
                Err(_) => break,
            }
        }
    });

    // 泵2: mpsc -> pty 输入 (阻塞写线程)
    let (in_tx, mut in_rx) = mpsc::channel::<Vec<u8>>(64);
    std::thread::spawn(move || {
        while let Some(b) = in_rx.blocking_recv() {
            if std::io::Write::write_all(&mut writer, &b).is_err() {
                break;
            }
            let _ = std::io::Write::flush(&mut writer);
        }
    });

    // 注册 resize 句柄 + 杀手
    let mut killer = child.clone_killer();
    let master = Arc::new(tokio::sync::Mutex::new(pair.master));
    ctx.masters.lock().await.insert(cid, master.clone());

    // 子进程退出监听
    let (done_tx, done_rx) = tokio::sync::oneshot::channel::<u32>();
    tokio::task::spawn_blocking(move || {
        let code = child.wait().ok().map(|s| s.exit_code()).unwrap_or(0);
        let _ = done_tx.send(code);
    });
    let mut done_rx = done_rx;
    

    let shutdown = ctx.shutdown.clone();
    let mut reason;
    loop {
        tokio::select! {
            _ = shutdown.cancelled() => {
                tracing::info!("停机信号, 终止 shell");
                reason = "shutdown";
                break;
            }
            code = &mut done_rx => {
                let code = match code { Ok(c) => c, Err(_) => 0 };
                tracing::info!("shell 退出: code={code}");
                let _ = ctx.channel.exit_status(code).await;
                reason = "exit";
                break;
            }
            out = out_rx.recv() => {
                match out {
                    Some(b) => {
                        if ctx.channel.data_bytes(b).await.is_err() {
                            reason = "chan-write";
                            break;
                        }
                    }
                    None => { reason = "pty-eof"; break; }
                }
            }
            msg = ctx.channel.wait() => {
                match msg {
                    None => { reason = "chan-closed"; break; }
                    Some(ChannelMsg::Data { data }) => {
                        if in_tx.send(data.to_vec()).await.is_err() {
                            reason = "pty-dead";
                            break;
                        }
                    }
                    Some(ChannelMsg::WindowChange { col_width, row_height, .. }) => {
                        let _ = master
                            .lock()
                            .await
                            .resize(portable_pty::PtySize {
                                rows: row_height.max(2) as u16,
                                cols: col_width.max(2) as u16,
                                pixel_width: 0,
                                pixel_height: 0,
                            });
                    }
                    Some(ChannelMsg::Eof) => { /* 客户端停发输入, shell 继续 */ }
                    Some(ChannelMsg::Close) => { reason = "client-close"; break; }
                    _ => {}
                }
            }
        }
    }

    // 清理
    ctx.masters.lock().await.remove(&cid);
    let _ = killer.kill();
    let _ = ctx.channel.eof().await;
    let _ = ctx.channel.close().await;
    tracing::info!("交互 shell 结束: channel={cid} 原因={reason}");
    drop(in_tx); // 关闭 pty 输入泵
}

/// 单次命令执行 (exec channel)
pub async fn run_exec(channel: Channel<Msg>, shell: &str, command: &str) {
    tracing::info!("exec: {command}");
    let mut cmd = std::process::Command::new(shell);
    let shell_lower = shell.to_lowercase();
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        use std::process::Stdio;
        if shell_lower.contains("powershell") || shell_lower.contains("pwsh") {
            cmd.args(["-NoProfile", "-NonInteractive", "-Command", command]);
        } else {
            cmd.args(["/C", command]);
        }
        cmd.stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .stdin(std::process::Stdio::null());
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    #[cfg(not(windows))]
    {
        use std::process::Stdio;
        cmd.args(["-c", command]);
        cmd.stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .stdin(std::process::Stdio::null());
    }

    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => {
            let _ = channel.data_bytes(format!("exec 失败: {e}\r\n")).await;
            let _ = channel.exit_status(127).await;
            let _ = channel.close().await;
            return;
        }
    };

    let mut stdout = child.stdout.take().expect("stdout");
    let mut stderr = child.stderr.take().expect("stderr");

    // 桥1: stdout 阻塞读 -> mpsc
    let (o_tx, mut o_rx) = mpsc::channel::<Vec<u8>>(8);
    std::thread::spawn(move || {
        let mut buf = vec![0u8; IO_BUF];
        loop {
            match std::io::Read::read(&mut stdout, &mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    if o_tx.blocking_send(buf[..n].to_vec()).is_err() {
                        break;
                    }
                }
            }
        }
    });
    // 桥2: stderr 阻塞读 -> mpsc
    let (e_tx, mut e_rx) = mpsc::channel::<Vec<u8>>(8);
    std::thread::spawn(move || {
        let mut buf = vec![0u8; IO_BUF];
        loop {
            match std::io::Read::read(&mut stderr, &mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    if e_tx.blocking_send(buf[..n].to_vec()).is_err() {
                        break;
                    }
                }
            }
        }
    });

    let ch1 = &channel;
    let ch2 = &channel;
    let mut wait = tokio::task::spawn_blocking(move || child.wait());
    let mut final_code: Option<u32> = None;
    loop {
        tokio::select! {
            out = o_rx.recv() => match out {
                Some(b) => {
                    if ch1.data_bytes(b).await.is_err() { break; }
                }
                None => { /* stdout 关闭, 继续等 stderr/退出 */ }
            },
            err = e_rx.recv() => match err {
                Some(b) => {
                    if ch2.extended_data_bytes(1, b).await.is_err() { break; }
                }
                None => { /* stderr 关闭 */ }
            },
            res = &mut wait => {
                if let Ok(Ok(st)) = res {
                    final_code = Some(st.code().map(|c| c as u32).unwrap_or(0));
                }
                break;
            }
        }
    }
    let _ = channel.exit_status(final_code.unwrap_or(0)).await;
    let _ = channel.eof().await;
    let _ = channel.close().await;
}
