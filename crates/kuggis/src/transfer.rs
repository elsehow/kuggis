//! Framed binary streams and atomic publication. A short read or checksum error
//! never publishes the destination. No shell or remote path handling lives here.
use crate::{Error, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    fs::{self, File, OpenOptions},
    io::{BufRead, Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

pub const MAX_BYTES: u64 = 16 * 1024 * 1024 * 1024;

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Header {
    pub size: u64,
    pub sha256: String,
}

pub fn digest(reader: &mut impl Read) -> Result<String> {
    let mut hash = Sha256::new();
    let mut buf = [0; 64 * 1024];
    loop {
        let n = reader.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hash.update(&buf[..n]);
    }
    Ok(format!("{:x}", hash.finalize()))
}

pub fn send(file: &mut File, output: &mut impl Write) -> Result<()> {
    if !file.metadata()?.is_file() {
        return Err(Error("transfer source must be a regular file".into()));
    }
    let size = file.metadata()?.len();
    if size > MAX_BYTES {
        return Err(Error("file exceeds the 16 GiB transfer limit".into()));
    }
    file.seek(SeekFrom::Start(0))?;
    let header = Header {
        size,
        sha256: digest(file)?,
    };
    file.seek(SeekFrom::Start(0))?;
    serde_json::to_writer(&mut *output, &header)?;
    output.write_all(b"\n")?;
    let copied = std::io::copy(&mut file.take(size), output)?;
    if copied != size {
        return Err(Error("transfer source changed or was truncated".into()));
    }
    Ok(())
}

pub fn receive(reader: &mut impl BufRead, file: &mut File) -> Result<()> {
    let mut bytes = Vec::new();
    reader.take(1025).read_until(b'\n', &mut bytes)?;
    if bytes.len() > 1024 || bytes.last() != Some(&b'\n') {
        return Err(Error("invalid file transfer header".into()));
    }
    let header: Header = serde_json::from_slice(&bytes)?;
    if header.size > MAX_BYTES
        || header.sha256.len() != 64
        || !header
            .sha256
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        return Err(Error("invalid transfer length or checksum".into()));
    }
    let mut hash = Sha256::new();
    let mut left = header.size;
    let mut buffer = [0; 64 * 1024];
    while left != 0 {
        let length = buffer.len().min(left as usize);
        let n = reader.read(&mut buffer[..length])?;
        if n == 0 {
            return Err(Error("incomplete file transfer".into()));
        }
        hash.update(&buffer[..n]);
        file.write_all(&buffer[..n])?;
        left -= n as u64;
    }
    if reader.read(&mut [0])? != 0 || format!("{:x}", hash.finalize()) != header.sha256 {
        return Err(Error("file transfer checksum or framing mismatch".into()));
    }
    file.sync_all()?;
    Ok(())
}

/// Hold until both the data checksum and the SSH exit status have succeeded.
pub struct PendingFile {
    path: PathBuf,
    destination: PathBuf,
    pub file: File,
}

impl PendingFile {
    pub fn new(destination: &Path) -> Result<Self> {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let parent = destination
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."));
        let path = parent.join(format!(
            ".kuggis-transfer-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let file = options.open(&path)?;
        Ok(Self {
            path,
            destination: destination.to_owned(),
            file,
        })
    }

    pub fn commit(self, replace: bool) -> Result<()> {
        self.file.sync_all()?;
        if replace {
            fs::rename(&self.path, &self.destination)?;
        } else {
            fs::hard_link(&self.path, &self.destination)?;
        }
        Ok(())
    }
}

impl Drop for PendingFile {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}
