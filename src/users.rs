use anyhow::{Context, Result};
use base64::Engine as _;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct User {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub password_bcrypt: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub keys: Vec<String>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct UserStore {
    #[serde(default, rename = "user")]
    pub users: Vec<User>,
}

pub struct Users {
    path: PathBuf,
}

impl Users {
    pub fn new(path: PathBuf) -> Self {
        Self { path }
    }

    fn load(&self) -> Result<UserStore> {
        if !self.path.exists() {
            return Ok(UserStore::default());
        }
        let raw = std::fs::read_to_string(&self.path)
            .with_context(|| format!("读取 {}", self.path.display()))?;
        let mut store: UserStore = toml::from_str(&raw)
            .with_context(|| format!("解析 {}", self.path.display()))?;
        // 兼容: 旧字段名/空用户清理
        store.users.retain(|u| !u.name.is_empty());
        Ok(store)
    }

    fn save(&self, store: &UserStore) -> Result<()> {
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&self.path, toml::to_string_pretty(store)?)
            .with_context(|| format!("写入 {}", self.path.display()))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&self.path, Path::new(&self.path).metadata()?.permissions());
        }
        Ok(())
    }

    pub fn verify_password(&self, name: &str, password: &str) -> Result<bool> {
        let store = self.load()?;
        let Some(u) = store.users.iter().find(|u| u.name == name) else {
            return Ok(false); // 用户不存在: 仍做一次假校验以抹平时序 (防用户枚举)
        };
        let _ = bcrypt_burn(); // 时序抹平
        match &u.password_bcrypt {
            Some(h) => Ok(bcrypt::verify(password, h).unwrap_or(false)),
            None => Ok(false),
        }
    }

    pub fn verify_pubkey(&self, name: &str, key_openssh: &str) -> Result<bool> {
        let store = self.load()?;
        let Some(u) = store.users.iter().find(|u| u.name == name) else {
            return Ok(false);
        };
        let norm = normalize_key(key_openssh);
        Ok(u.keys.iter().any(|k| normalize_key(k) == norm))
    }

    pub fn user_exists(&self, name: &str) -> Result<bool> {
        Ok(self.load()?.users.iter().any(|u| u.name == name))
    }

    pub fn add(&self, name: &str, password: &str) -> Result<()> {
        let mut store = self.load()?;
        if store.users.iter().any(|u| u.name == name) {
            anyhow::bail!("用户 {} 已存在", name);
        }
        let hash = bcrypt::hash(password, 12)?;
        store.users.push(User {
            name: name.into(),
            password_bcrypt: Some(hash),
            keys: vec![],
        });
        self.save(&store)
    }

    pub fn set_password(&self, name: &str, password: &str) -> Result<()> {
        let mut store = self.load()?;
        let u = store
            .users
            .iter_mut()
            .find(|u| u.name == name)
            .ok_or_else(|| anyhow::anyhow!("用户 {} 不存在", name))?;
        u.password_bcrypt = Some(bcrypt::hash(password, 12)?);
        self.save(&store)
    }

    pub fn del(&self, name: &str) -> Result<()> {
        let mut store = self.load()?;
        let before = store.users.len();
        store.users.retain(|u| u.name != name);
        if store.users.len() == before {
            anyhow::bail!("用户 {} 不存在", name);
        }
        self.save(&store)
    }

    pub fn add_key(&self, name: &str, key_line: &str) -> Result<()> {
        let line = key_line.trim();
        let mut parts = line.split_whitespace();
        let (Some(algo), Some(blob)) = (parts.next(), parts.next()) else {
            anyhow::bail!("不是有效的 openssh 公钥行 (缺少 算法 base64 两段): {line}");
        };
        if !algo.starts_with("ssh-") && !algo.starts_with("ecdsa-") && algo != "sk-ssh-ed25519@openssh.com" {
            anyhow::bail!("疑似私钥或非公钥行 (algo={algo}) —— 请传入 .pub 公钥文件");
        }
        if base64::engine::general_purpose::STANDARD.decode(blob).is_err() {
            anyhow::bail!("公钥 base64 段解码失败");
        }
        let mut store = self.load()?;
        let u = store
            .users
            .iter_mut()
            .find(|u| u.name == name)
            .ok_or_else(|| anyhow::anyhow!("用户 {} 不存在", name))?;
        u.keys.push(line.to_string());
        self.save(&store)
    }

    pub fn list(&self) -> Result<Vec<User>> {
        Ok(self.load()?.users)
    }
}

fn normalize_key(s: &str) -> String {
    s.split_whitespace().take(2).collect::<Vec<_>>().join(" ")
}

// 口令校验失败的时序抹平: 跑一次等价 bcrypt
fn bcrypt_burn() {
    let _ = bcrypt::hash("timing-equalizer", 12);
}

pub fn prompt_password(confirm: bool) -> Result<String> {
    let pw = rpassword::prompt_password("设置口令: ")?;
    if !confirm {
        return Ok(pw);
    }
    let pw2 = rpassword::prompt_password("再次确认: ")?;
    if pw != pw2 {
        anyhow::bail!("两次输入不一致");
    }
    if pw.len() < 6 {
        anyhow::bail!("口令至少 6 位");
    }
    Ok(pw)
}
