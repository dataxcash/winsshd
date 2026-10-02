//! Windows 服务集成 (仅 Windows 生效; 其它平台为桩)
pub const SERVICE_NAME: &str = "WinSSHD";

#[cfg(windows)]
pub mod imp {
    use anyhow::Result;
    use std::ffi::{OsStr, OsString};
    use windows_service::{
        define_windows_service,
        service::{
            ServiceAccess, ServiceControl, ServiceControlAccept, ServiceErrorControl,
            ServiceExitCode, ServiceInfo, ServiceState, ServiceStartType, ServiceStatus,
            ServiceType,
        },
        service_control_handler::{self, ServiceControlHandlerResult},
        service_dispatcher, service_manager::{ServiceManager, ServiceManagerAccess},
    };

    use crate::SERVICE_NAME;

    define_windows_service!(ffi_service_main, service_main);

    pub fn dispatch() -> Result<()> {
        service_dispatcher::start(SERVICE_NAME, ffi_service_main)?;
        Ok(())
    }

    fn service_main(_args: Vec<OsString>) {
        if let Err(e) = run_as_service() {
            tracing::error!("服务运行失败: {e:#}");
        }
    }

    fn run_as_service() -> Result<()> {
        let (stop_tx, stop_rx) = std::sync::mpsc::channel::<()>();
        let handler = move |control: ServiceControl| match control {
            ServiceControl::Stop | ServiceControl::Shutdown => {
                let _ = stop_tx.send(());
                ServiceControlHandlerResult::NoError
            }
            ServiceControl::Interrogate => ServiceControlHandlerResult::NoError,
            _ => ServiceControlHandlerResult::NotImplemented,
        };
        let status = service_control_handler::register(SERVICE_NAME, handler)?;
        status.set_service_status(ServiceStatus {
            service_type: ServiceType::OWN_PROCESS,
            current_state: ServiceState::Running,
            controls_accepted: ServiceControlAccept::STOP,
            exit_code: ServiceExitCode::Win32(0),
            checkpoint: 0,
            wait_hint: std::time::Duration::from_secs(5),
            process_id: None,
        })?;

        let rt = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()?;
        rt.block_on(async move {
            let shutdown = tokio_util::sync::CancellationToken::new();
            let server = crate::server_start(None, shutdown.clone());
            tokio::select! {
                _ = shutdown_run(stop_rx) => { tracing::info!("服务收到停止指令"); }
                res = server => {
                    if let Err(e) = res { tracing::error!("服务循环异常退出: {e:#}"); }
                }
            }
            shutdown.cancel();
        });

        status.set_service_status(ServiceStatus {
            service_type: ServiceType::OWN_PROCESS,
            current_state: ServiceState::Stopped,
            controls_accepted: ServiceControlAccept::STOP,
            exit_code: ServiceExitCode::Win32(0),
            checkpoint: 0,
            wait_hint: std::time::Duration::from_secs(1),
            process_id: None,
        })?;
        Ok(())
    }

    async fn shutdown_run(rx: std::sync::mpsc::Receiver<()>) {
        // mpsc(std) -> 异步轮询 (stop 信号到达即返回)
        let (atx, mut arx) = tokio::sync::mpsc::channel::<()>(1);
        std::thread::spawn(move || {
            while rx.recv().is_ok() {
                if atx.blocking_send(()).is_err() {
                    break;
                }
            }
        });
        while arx.recv().await.is_some() {}
    }

    pub fn install() -> Result<()> {
        let exe = std::env::current_exe()?;
        let manager = ServiceManager::local_computer(None::<&OsStr>, ServiceManagerAccess::ALL_ACCESS)?;
        let info = ServiceInfo {
            name: SERVICE_NAME.into(),
            display_name: "WinSSHD — Rust SSH/SFTP Server".into(),
            service_type: ServiceType::OWN_PROCESS,
            start_type: ServiceStartType::AutoStart,
            error_control: ServiceErrorControl::Normal,
            executable_path: exe,
            launch_arguments: vec!["run".into(), "--service".into()],
            account_name: None,   // LocalSystem
            account_password: None,
            dependencies: vec![],
        };
        let service = manager.create_service(&info, ServiceAccess::ALL_ACCESS)?;
        service.set_description("极简 Rust SSH/SFTP 服务端 (russh) — 开机自启")?;
        tracing::info!("服务已注册");
        Ok(())
    }

    pub fn uninstall() -> Result<()> {
        let manager = ServiceManager::local_computer(None::<&OsStr>, ServiceManagerAccess::ALL_ACCESS)?;
        let service = manager.open_service(SERVICE_NAME, ServiceAccess::ALL_ACCESS)?;
        let _ = service.stop();
        std::thread::sleep(std::time::Duration::from_millis(800));
        service.delete()?;
        tracing::info!("服务已卸载");
        Ok(())
    }

    pub fn start() -> Result<()> {
        let manager = ServiceManager::local_computer(None::<&OsStr>, ServiceManagerAccess::CONNECT)?;
        let service = manager.open_service(SERVICE_NAME, ServiceAccess::START)?;
        service.start::<&str>(&[])?;
        tracing::info!("服务已启动");
        Ok(())
    }

    pub fn stop() -> Result<()> {
        let manager = ServiceManager::local_computer(None::<&OsStr>, ServiceManagerAccess::CONNECT)?;
        let service = manager.open_service(SERVICE_NAME, ServiceAccess::STOP)?;
        service.stop()?;
        tracing::info!("服务已停止");
        Ok(())
    }
}

#[cfg(not(windows))]
pub mod imp {
    use anyhow::bail;
    pub fn dispatch() -> anyhow::Result<()> {
        bail!("服务调度器仅 Windows 支持 (Linux 下直接前台运行)")
    }
    pub fn install() -> anyhow::Result<()> { bail!("仅 Windows 支持") }
    pub fn uninstall() -> anyhow::Result<()> { bail!("仅 Windows 支持") }
    pub fn start() -> anyhow::Result<()> { bail!("仅 Windows 支持") }
    pub fn stop() -> anyhow::Result<()> { bail!("仅 Windows 支持") }
}

pub use imp::{dispatch, install, start, stop, uninstall};
