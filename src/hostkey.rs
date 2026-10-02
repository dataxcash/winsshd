use anyhow::{Context, Result};
use std::path::Path;

/// 加载或首次生成 ed25519 主机密钥
pub fn load_or_generate(path: &Path) -> Result<russh::keys::PrivateKey> {
    if path.exists() {
        let key = russh::keys::decode_secret_key(
            &std::fs::read_to_string(path)
                .with_context(|| format!("读取主机密钥 {}", path.display()))?,
            None, // 无口令
        )
        .with_context(|| format!("解析主机密钥 {}", path.display()))?;
        tracing::info!("已加载主机密钥 {}", path.display());
        return Ok(key);
    }

    let key = russh::keys::PrivateKey::random(
        &mut rand::rng(),
        russh::keys::Algorithm::Ed25519,
    )?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let pem = key
        .to_openssh(russh::keys::ssh_key::LineEnding::LF)
        .map_err(|e| anyhow::anyhow!("编码私钥: {e}"))?;
    std::fs::write(path, pem.as_bytes())
        .with_context(|| format!("写入主机密钥 {}", path.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
    }
    tracing::info!("已生成新主机密钥 {}", path.display());
    Ok(key)
}
