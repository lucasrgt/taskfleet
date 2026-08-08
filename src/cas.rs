use std::{
    collections::HashSet,
    fs,
    io::Write,
    path::{Path, PathBuf},
};

#[cfg(any(target_os = "linux", target_os = "macos"))]
use std::process::Command;

use anyhow::{Context, Result, bail};
use sha2::{Digest, Sha256};

pub struct Cas {
    root: PathBuf,
}

impl Cas {
    pub fn open(root: &Path) -> Result<Self> {
        fs::create_dir_all(root.join("sha256"))?;
        Ok(Self { root: root.to_path_buf() })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn put_bytes(&self, bytes: &[u8], media_type: &str) -> Result<(String, u64, PathBuf)> {
        let digest = digest_bytes(bytes);
        let path = self.path_for(&digest)?;
        if !path.exists() {
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent)?;
            }
            let mut file = fs::File::create(&path)?;
            file.write_all(bytes)?;
        }
        let _ = media_type;
        Ok((digest, bytes.len() as u64, path))
    }

    pub fn put_path(&self, source: &Path, media_type: &str) -> Result<(String, u64, PathBuf)> {
        let bytes = fs::read(source).with_context(|| format!("read {}", source.display()))?;
        self.put_bytes(&bytes, media_type)
    }

    pub fn resolve(&self, digest: &str) -> Result<PathBuf> {
        let path = self.path_for(digest)?;
        if !path.exists() {
            bail!("artifact not found: {digest}");
        }
        Ok(path)
    }

    pub fn materialize(&self, digest: &str, destination: &Path) -> Result<PathBuf> {
        let source = self.resolve(digest)?;
        copy_prefer_cow(&source, destination)?;
        Ok(destination.to_path_buf())
    }

    pub fn get_bytes(&self, digest: &str) -> Result<Vec<u8>> {
        Ok(fs::read(self.resolve(digest)?)?)
    }

    pub fn path_for(&self, digest: &str) -> Result<PathBuf> {
        let hex = strip_sha256(digest)?;
        Ok(self.root.join("sha256").join(&hex[..2]).join(&hex[2..]))
    }

    pub fn sweep(&self, keep: &HashSet<String>) -> Result<usize> {
        let base = self.root.join("sha256");
        if !base.exists() {
            return Ok(0);
        }
        let mut removed = 0;
        for prefix in fs::read_dir(&base)? {
            let prefix = prefix?;
            if !prefix.file_type()?.is_dir() {
                continue;
            }
            for entry in fs::read_dir(prefix.path())? {
                let entry = entry?;
                if !entry.file_type()?.is_file() {
                    continue;
                }
                let digest = format!("sha256:{}{}", prefix.file_name().to_string_lossy(), entry.file_name().to_string_lossy());
                if !keep.contains(&digest) {
                    fs::remove_file(entry.path())?;
                    removed += 1;
                }
            }
        }
        Ok(removed)
    }
}

pub fn digest_bytes(bytes: &[u8]) -> String {
    let hash = Sha256::digest(bytes);
    let mut out = String::with_capacity(7 + 64);
    out.push_str("sha256:");
    for byte in hash {
        out.push_str(&format!("{byte:02x}"));
    }
    out
}

pub fn digest_str(text: &str) -> String {
    digest_bytes(text.as_bytes())
}

pub fn is_digest(value: &str) -> bool {
    strip_sha256(value).is_ok()
}

fn strip_sha256(digest: &str) -> Result<&str> {
    let hex = digest.strip_prefix("sha256:").context("digest must start with sha256:")?;
    if hex.len() != 64 || !hex.chars().all(|c| c.is_ascii_hexdigit()) {
        bail!("invalid sha256 digest");
    }
    Ok(hex)
}

pub fn copy_prefer_cow(src: &Path, dst: &Path) -> Result<()> {
    if let Some(parent) = dst.parent() {
        fs::create_dir_all(parent)?;
    }
    if try_cow_clone(src, dst) {
        return Ok(());
    }
    fs::copy(src, dst)?;
    Ok(())
}

fn try_cow_clone(src: &Path, dst: &Path) -> bool {
    if dst.exists() {
        let _ = fs::remove_file(dst);
    }
    #[cfg(target_os = "linux")]
    {
        Command::new("cp")
            .args(["--reflink=auto", "--", &src.display().to_string(), &dst.display().to_string()])
            .status()
            .map(|status| status.success() && dst.exists())
            .unwrap_or(false)
    }
    #[cfg(target_os = "macos")]
    {
        Command::new("cp")
            .args(["-c", "--", &src.display().to_string(), &dst.display().to_string()])
            .status()
            .map(|status| status.success() && dst.exists())
            .unwrap_or(false)
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        let _ = (src, dst);
        false
    }
}
