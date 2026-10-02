use std::collections::HashMap;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use russh_sftp::protocol::{
    Data, File, FileAttributes, Handle, Name, OpenFlags, Status, StatusCode, Version,
};
use tokio::sync::Mutex;

/// 句柄表条目: 文件句柄或目录句柄
enum HandleEntry {
    File { path: PathBuf, file: std::fs::File },
    Dir { entries: Vec<File>, index: usize },
}

pub struct SftpFs {
    root: PathBuf,
    handles: Mutex<HashMap<String, HandleEntry>>,
    counter: AtomicU64,
}

fn ok(id: u32) -> Status {
    Status {
        id,
        status_code: StatusCode::Ok,
        error_message: "Ok".into(),
        language_tag: "en".into(),
    }
}

fn fail(id: u32, code: StatusCode, msg: &str) -> Status {
    Status {
        id,
        status_code: code,
        error_message: msg.into(),
        language_tag: "en".into(),
    }
}

fn sc_err(e: &std::io::Error) -> StatusCode {
    match e.kind() {
        std::io::ErrorKind::NotFound => StatusCode::NoSuchFile,
        std::io::ErrorKind::PermissionDenied => StatusCode::PermissionDenied,
        _ => StatusCode::Failure,
    }
}

impl SftpFs {
    pub fn new(root: PathBuf) -> Self {
        Self {
            root,
            handles: Mutex::new(HashMap::new()),
            counter: AtomicU64::new(1),
        }
    }

    /// 根目录沙箱路径解析: 拒绝 `..` 与绝对路径成分
    fn resolve(&self, p: &str) -> Result<PathBuf, StatusCode> {
        let rel = p.trim_start_matches('/');
        if rel.is_empty() {
            return Ok(self.root.clone());
        }
        let mut out = self.root.clone();
        for comp in Path::new(rel).components() {
            match comp {
                std::path::Component::Normal(c) => out.push(c),
                std::path::Component::CurDir => {}
                _ => return Err(StatusCode::PermissionDenied),
            }
        }
        Ok(out)
    }

    fn next_handle(&self) -> String {
        format!(
            "h{}",
            self.counter.fetch_add(1, Ordering::Relaxed)
        )
    }

    fn attrs_from(md: &std::fs::Metadata) -> FileAttributes {
        let type_bits = if md.is_dir() {
            0o040000
        } else {
            0o100000
        };
        let mode_bits = {
            #[cfg(unix)]
            {
                use std::os::unix::fs::MetadataExt;
                md.mode() & 0o7777
            }
            #[cfg(not(unix))]
            {
                if md.permissions().readonly() {
                    0o444
                } else {
                    0o666
                }
            }
        };
        let (atime, mtime) = {
            let at = md
                .accessed()
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_secs() as u32);
            let mt = md
                .modified()
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_secs() as u32);
            (at, mt)
        };
        FileAttributes {
            size: Some(md.len()),
            uid: None,
            user: None,
            gid: None,
            group: None,
            permissions: Some(type_bits | mode_bits),
            atime,
            mtime,
        }
    }

    fn longname(path: &Path, md: &std::fs::Metadata) -> String {
        format!(
            "{} {:>8} {}",
            if md.is_dir() { "drwxr-xr-x" } else { "-rw-r--r--" },
            md.len(),
            path.display()
        )
    }
}

impl russh_sftp::server::Handler for SftpFs {
    type Error = StatusCode;

    fn unimplemented(&self) -> Self::Error {
        StatusCode::OpUnsupported
    }

    async fn init(&mut self, _version: u32, _extensions: HashMap<String, String>) -> Result<Version, Self::Error> {
        tracing::info!("SFTP init");
        Ok(Version::new())
    }

    async fn realpath(&mut self, id: u32, path: String) -> Result<Name, Self::Error> {
        let resolved = self.resolve(&path)?;
        let display = if resolved == self.root {
            "/".to_string()
        } else {
            let rel = resolved
                .strip_prefix(&self.root)
                .unwrap_or(&resolved)
                .to_string_lossy()
                .replace('\\', "/");
            format!("/{rel}")
        };
        Ok(Name {
            id,
            files: vec![File::dummy(display)],
        })
    }

    async fn opendir(&mut self, id: u32, path: String) -> Result<Handle, Self::Error> {
        let dir = self.resolve(&path)?;
        let rd = std::fs::read_dir(&dir).map_err(|e| sc_err(&e))?;
        let mut entries = Vec::new();
        for entry in rd.flatten() {
            let path = entry.path();
            match entry.metadata() {
                Ok(md) => entries.push(File {
                    filename: entry.file_name().to_string_lossy().to_string(),
                    longname: Self::longname(&path, &md),
                    attrs: Self::attrs_from(&md),
                }),
                Err(_) => continue,
            }
        }
        let handle = self.next_handle();
        self.handles
            .lock()
            .await
            .insert(handle.clone(), HandleEntry::Dir { entries, index: 0 });
        Ok(Handle { id, handle })
    }

    async fn readdir(&mut self, id: u32, handle: String) -> Result<Name, Self::Error> {
        let mut hs = self.handles.lock().await;
        let Some(HandleEntry::Dir { entries, index }) = hs.get_mut(&handle) else {
            return Err(StatusCode::NoSuchFile);
        };
        if *index >= entries.len() {
            return Err(StatusCode::Eof);
        }
        let batch: Vec<File> = entries[*index..].iter().take(100).cloned().collect();
        *index += batch.len();
        drop(hs);
        Ok(Name { id, files: batch })
    }

    async fn stat(&mut self, id: u32, path: String) -> Result<russh_sftp::protocol::Attrs, Self::Error> {
        let p = self.resolve(&path)?;
        let md = std::fs::metadata(&p).map_err(|e| sc_err(&e))?;
        Ok(russh_sftp::protocol::Attrs { id, attrs: Self::attrs_from(&md) })
    }

    async fn lstat(&mut self, id: u32, path: String) -> Result<russh_sftp::protocol::Attrs, Self::Error> {
        let p = self.resolve(&path)?;
        let md = std::fs::symlink_metadata(&p).map_err(|e| sc_err(&e))?;
        Ok(russh_sftp::protocol::Attrs { id, attrs: Self::attrs_from(&md) })
    }

    async fn fstat(&mut self, id: u32, handle: String) -> Result<russh_sftp::protocol::Attrs, Self::Error> {
        let hs = self.handles.lock().await;
        let Some(HandleEntry::File { file, .. }) = hs.get(&handle) else {
            return Err(StatusCode::NoSuchFile);
        };
        let md = file.metadata().map_err(|e| sc_err(&e))?;
        Ok(russh_sftp::protocol::Attrs { id, attrs: Self::attrs_from(&md) })
    }

    async fn setstat(&mut self, id: u32, path: String, attrs: FileAttributes) -> Result<Status, Self::Error> {
        let p = self.resolve(&path)?;
        if let Some(size) = attrs.size {
            let f = std::fs::OpenOptions::new()
                .write(true)
                .open(&p)
                .map_err(|e| sc_err(&e))?;
            f.set_len(size).map_err(|e| sc_err(&e))?;
        }
        Ok(ok(id))
    }

    async fn fsetstat(&mut self, id: u32, handle: String, attrs: FileAttributes) -> Result<Status, Self::Error> {
        let hs = self.handles.lock().await;
        let Some(HandleEntry::File { file, .. }) = hs.get(&handle) else {
            return Err(StatusCode::NoSuchFile);
        };
        if let Some(size) = attrs.size {
            file.set_len(size).map_err(|e| sc_err(&e))?;
        }
        Ok(ok(id))
    }

    async fn open(
        &mut self,
        id: u32,
        filename: String,
        pflags: OpenFlags,
        _attrs: FileAttributes,
    ) -> Result<Handle, Self::Error> {
        let p = self.resolve(&filename)?;
        let mut opts = std::fs::OpenOptions::new();
        opts.read(pflags.contains(OpenFlags::READ));
        let write = pflags.contains(OpenFlags::WRITE)
            || pflags.contains(OpenFlags::APPEND)
            || pflags.contains(OpenFlags::CREATE)
            || pflags.contains(OpenFlags::TRUNCATE);
        opts.write(write);
        opts.append(pflags.contains(OpenFlags::APPEND));
        opts.create(pflags.contains(OpenFlags::CREATE));
        opts.truncate(pflags.contains(OpenFlags::TRUNCATE) && !pflags.contains(OpenFlags::APPEND));
        opts.create_new(pflags.contains(OpenFlags::EXCLUDE));
        let file = opts.open(&p).map_err(|e| sc_err(&e))?;
        let handle = self.next_handle();
        self.handles.lock().await.insert(
            handle.clone(),
            HandleEntry::File { path: p, file },
        );
        Ok(Handle { id, handle })
    }

    async fn read(&mut self, id: u32, handle: String, offset: u64, len: u32) -> Result<Data, Self::Error> {
        let mut hs = self.handles.lock().await;
        let Some(HandleEntry::File { file, .. }) = hs.get_mut(&handle) else {
            return Err(StatusCode::NoSuchFile);
        };
        file.seek(SeekFrom::Start(offset)).map_err(|e| sc_err(&e))?;
        let mut buf = vec![0u8; len as usize];
        let mut total = 0;
        while total < len as usize {
            let n = file.read(&mut buf[total..]).map_err(|e| sc_err(&e))?;
            if n == 0 {
                break;
            }
            total += n;
        }
        buf.truncate(total);
        if total == 0 {
            return Err(StatusCode::Eof);
        }
        Ok(Data { id, data: buf })
    }

    async fn write(&mut self, id: u32, handle: String, offset: u64, data: Vec<u8>) -> Result<Status, Self::Error> {
        let mut hs = self.handles.lock().await;
        let Some(HandleEntry::File { file, .. }) = hs.get_mut(&handle) else {
            return Err(StatusCode::NoSuchFile);
        };
        file.seek(SeekFrom::Start(offset)).map_err(|e| sc_err(&e))?;
        file.write_all(&data).map_err(|e| sc_err(&e))?;
        Ok(ok(id))
    }

    async fn close(&mut self, id: u32, handle: String) -> Result<Status, Self::Error> {
        self.handles.lock().await.remove(&handle);
        Ok(ok(id))
    }

    async fn mkdir(&mut self, id: u32, path: String, _attrs: FileAttributes) -> Result<Status, Self::Error> {
        let p = self.resolve(&path)?;
        std::fs::create_dir(&p).map_err(|e| sc_err(&e))?;
        Ok(ok(id))
    }

    async fn rmdir(&mut self, id: u32, path: String) -> Result<Status, Self::Error> {
        let p = self.resolve(&path)?;
        if p == self.root {
            return Err(StatusCode::PermissionDenied);
        }
        std::fs::remove_dir(&p).map_err(|e| sc_err(&e))?;
        Ok(ok(id))
    }

    async fn remove(&mut self, id: u32, filename: String) -> Result<Status, Self::Error> {
        let p = self.resolve(&filename)?;
        std::fs::remove_file(&p).map_err(|e| sc_err(&e))?;
        Ok(ok(id))
    }

    async fn rename(&mut self, id: u32, oldpath: String, newpath: String) -> Result<Status, Self::Error> {
        let from = self.resolve(&oldpath)?;
        let to = self.resolve(&newpath)?;
        if from == self.root || to == self.root {
            return Err(StatusCode::PermissionDenied);
        }
        // POSIX rename 语义: 目标存在则替换
        std::fs::rename(&from, &to).or_else(|e| {
            if e.kind() == std::io::ErrorKind::AlreadyExists {
                std::fs::remove_file(&to).ok();
                std::fs::rename(&from, &to)
            } else {
                Err(e)
            }
        }).map_err(|e| sc_err(&e))?;
        Ok(ok(id))
    }

    async fn readlink(&mut self, id: u32, path: String) -> Result<Name, Self::Error> {
        let p = self.resolve(&path)?;
        let target = std::fs::read_link(&p).map_err(|e| sc_err(&e))?;
        Ok(Name {
            id,
            files: vec![File::dummy(target.to_string_lossy().to_string())],
        })
    }

    async fn symlink(&mut self, id: u32, linkpath: String, targetpath: String) -> Result<Status, Self::Error> {
        let link = self.resolve(&linkpath)?;
        #[cfg(unix)]
        let r = std::os::unix::fs::symlink(&targetpath, &link);
        #[cfg(windows)]
        let r = std::os::windows::fs::symlink_file(&targetpath, &link);
        r.map_err(|e| sc_err(&e))?;
        Ok(ok(id))
    }
}
