//! Static artifacts are explicit snapshots, never a workspace mount or an
//! archive extraction. Paths are validated at both packing and upload boundaries.
use crate::{canon, Hash};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeSet,
    ffi::CString,
    fs::{self, File, OpenOptions},
    io::{self, Read, Write},
    os::{
        fd::{AsRawFd, FromRawFd},
        unix::{
            ffi::OsStrExt,
            fs::{DirBuilderExt, OpenOptionsExt},
        },
    },
    path::Path,
};

pub const MAX_FILES: usize = 512;
pub const MAX_BYTES: usize = 5 * 1024 * 1024;
// Byte arrays have at most four JSON bytes per content byte, plus paths.
pub const MAX_WIRE_BYTES: usize = 24 * 1024 * 1024;
pub const MAX_STORE_BYTES: u64 = 256 * 1024 * 1024;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Entry {
    pub path: String,
    pub bytes: Vec<u8>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Artifact {
    pub files: Vec<Entry>,
}
fn bad(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message)
}

pub fn check_path(path: &str) -> io::Result<()> {
    if path.len() > 240
        || path.split('/').any(|part| {
            part.is_empty()
                || part.starts_with('.')
                || part.len() > 100
                || !part
                    .bytes()
                    .all(|c| c.is_ascii_alphanumeric() || b"-_.".contains(&c))
        })
    {
        return Err(bad(
            "artifact paths must be relative, non-hidden ASCII paths without traversal",
        ));
    }
    Ok(())
}
impl Artifact {
    pub fn validate(&self) -> io::Result<()> {
        if self.files.is_empty() || self.files.len() > MAX_FILES {
            return Err(bad("artifact file count exceeds limit"));
        }
        let mut paths = BTreeSet::new();
        let mut total = 0usize;
        let mut previous: Option<&str> = None;
        for file in &self.files {
            check_path(&file.path)?;
            if previous.is_some_and(|p| p >= file.path.as_str()) {
                return Err(bad("artifact paths must be unique and sorted"));
            }
            previous = Some(&file.path);
            total = total
                .checked_add(file.bytes.len())
                .ok_or_else(|| bad("artifact too large"))?;
            if total > MAX_BYTES {
                return Err(bad("artifact exceeds 5 MiB"));
            }
            paths.insert(file.path.as_str());
        }
        if !paths.contains("index.html") {
            return Err(bad("artifact must contain index.html"));
        }
        for path in &paths {
            for (at, _) in path.match_indices('/') {
                if paths.contains(&path[..at]) {
                    return Err(bad("artifact file/directory conflict"));
                }
            }
        }
        Ok(())
    }
    pub fn hash(&self) -> io::Result<Hash> {
        self.validate()?;
        Ok(Hash::of(&canon::bytes(self)))
    }
    /// Run as the agent, not the privileged signer. Directory FDs pin each
    /// traversed directory; openat + O_NOFOLLOW rejects symlink-swap races.
    pub fn snapshot(directory: &Path) -> io::Result<Self> {
        let root = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW)
            .open(directory)?;
        let mut files = vec![];
        let mut total = 0;
        let mut visited = 0;
        walk(&root, "", &mut files, &mut total, &mut visited)?;
        files.sort_by(|a, b| a.path.cmp(&b.path));
        let artifact = Self { files };
        artifact.validate()?;
        Ok(artifact)
    }
}
fn walk(
    dir: &File,
    prefix: &str,
    files: &mut Vec<Entry>,
    total: &mut usize,
    visited: &mut usize,
) -> io::Result<()> {
    // /proc refers to OUR already-open descriptor, not an agent-selected link.
    for entry in fs::read_dir(format!("/proc/self/fd/{}", dir.as_raw_fd()))? {
        *visited += 1;
        if *visited > 1024 {
            return Err(bad("artifact tree has too many entries"));
        }
        let name = entry?.file_name();
        let text = name
            .to_str()
            .ok_or_else(|| bad("non-ASCII artifact name"))?;
        let path = format!("{prefix}{text}");
        check_path(&path)?;
        let c_name = CString::new(name.as_bytes()).map_err(|_| bad("invalid name"))?;
        // O_NONBLOCK ensures a hostile FIFO cannot hang the packer.
        let fd = unsafe {
            libc::openat(
                dir.as_raw_fd(),
                c_name.as_ptr(),
                libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC,
            )
        };
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        let file = unsafe { File::from_raw_fd(fd) };
        let metadata = file.metadata()?;
        if metadata.is_dir() {
            walk(&file, &format!("{path}/"), files, total, visited)?;
        } else if metadata.is_file() {
            if files.len() >= MAX_FILES {
                return Err(bad("artifact has too many files"));
            }
            let mut bytes = vec![];
            file.take((MAX_BYTES - *total + 1) as u64)
                .read_to_end(&mut bytes)?;
            *total += bytes.len();
            if *total > MAX_BYTES {
                return Err(bad("artifact exceeds 5 MiB"));
            }
            files.push(Entry { path, bytes });
        } else {
            return Err(bad("only regular files and directories can be published"));
        }
    }
    Ok(())
}

/// Called only under the registry lock in an authenticated commit. Stores a
/// content-addressed JSON snapshot, not executable files or filesystem paths.
pub(super) fn store(directory: &Path, artifact: &Artifact) -> io::Result<Hash> {
    let hash = artifact.hash()?;
    let directory = directory.join("artifacts");
    match fs::DirBuilder::new().mode(0o700).create(&directory) {
        Ok(()) => File::open(directory.parent().unwrap())?.sync_all()?,
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => (),
        Err(e) => return Err(e),
    }
    let path = directory.join(format!("{hash}.json"));
    if path.exists() {
        if load(directory.parent().unwrap(), hash)?.hash()? != hash {
            return Err(bad("artifact digest mismatch"));
        }
        return Ok(hash);
    }
    let bytes = canon::bytes(artifact);
    let mut used = 0u64;
    for entry in fs::read_dir(&directory)? {
        used = used.saturating_add(entry?.metadata()?.len());
    }
    if used.saturating_add(bytes.len() as u64) > MAX_STORE_BYTES {
        return Err(bad("publisher storage quota exceeded"));
    }
    let tmp = directory.join(format!(".{}", crate::RequestId::generate()));
    let result = (|| {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&tmp)?;
        file.write_all(&bytes)?;
        file.sync_all()?;
        fs::rename(&tmp, path)?;
        File::open(&directory)?.sync_all()?;
        Ok(hash)
    })();
    if result.is_err() {
        let _ = fs::remove_file(tmp);
    }
    result
}
pub(super) fn load(directory: &Path, hash: Hash) -> io::Result<Artifact> {
    let file = File::open(directory.join("artifacts").join(format!("{hash}.json")))?;
    let mut bytes = vec![];
    file.take(MAX_WIRE_BYTES as u64 + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() > MAX_WIRE_BYTES {
        return Err(bad("stored artifact too large"));
    }
    let artifact: Artifact = serde_json::from_slice(&bytes).map_err(|e| bad(&e.to_string()))?;
    if artifact.hash()? != hash {
        return Err(bad("stored artifact digest mismatch"));
    }
    Ok(artifact)
}
