use serde::Deserialize;
use std::path::PathBuf;

#[derive(Debug, Clone, Deserialize)]
pub struct Config {
    pub listen: String,
    pub shell: String,
    pub sftp_enabled: bool,
    pub sftp_root: PathBuf,
    pub hostkey_path: PathBuf,
    pub log_file: PathBuf,
    pub log_level: String,
    pub auth: AuthConfig,
}

#[derive(Debug, Clone, Deserialize)]
pub struct AuthConfig {
    pub password: bool,
    pub publickey: bool,
    pub users_file: PathBuf,
}

impl Config {
    /// 解析相对路径为相对于配置文件所在目录
    pub fn resolve_paths(&mut self, base_dir: &std::path::Path) {
        let fix = |p: &mut PathBuf| {
            if p.is_relative() {
                *p = base_dir.join(&*p);
            }
        };
        fix(&mut self.hostkey_path);
        fix(&mut self.log_file);
        fix(&mut self.auth.users_file);
        // sftp_root 保持原样 (Windows 盘符路径本就是绝对的)
    }

    pub fn load(path: &std::path::Path) -> anyhow::Result<Self> {
        let raw = std::fs::read_to_string(path)
            .map_err(|e| anyhow::anyhow!("读取配置 {}: {}", path.display(), e))?;
        let mut cfg: Config = toml::from_str(&raw)
            .map_err(|e| anyhow::anyhow!("配置解析失败 {}: {}", path.display(), e))?;
        let base = path.parent().unwrap_or_else(|| std::path::Path::new("."));
        cfg.resolve_paths(base);
        Ok(cfg)
    }
}
