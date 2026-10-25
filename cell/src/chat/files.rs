//! Read-only files from the chat process's own filesystem and OS identity.
//! The operator only relays bytes; it never resolves guest paths on the host.
use super::{error, ApiError, ApiResult, Config, ExecutionMode, Store};
use axum::{
    body::Body,
    extract::{Query, State},
    http::{header, StatusCode},
    response::Response,
    Json,
};
use serde::{Deserialize, Serialize};
use std::{
    ffi::{CStr, CString, OsString},
    fs::{self, File},
    io,
    os::{
        fd::{AsRawFd, FromRawFd, IntoRawFd},
        unix::{ffi::{OsStrExt, OsStringExt}, fs::OpenOptionsExt},
    },
    path::{Component, Path, PathBuf},
    process::Stdio,
    sync::Arc,
    time::UNIX_EPOCH,
};
use tokio::io::AsyncReadExt;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct FileQuery {
    path: Option<String>,
}

#[derive(Serialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub(crate) struct FileEntry {
    name: String,
    path: String,
    kind: FileKind,
    symlink: bool,
    size: Option<u64>,
    modified: Option<u64>,
}

#[derive(Serialize, ts_rs::TS, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum FileKind {
    File,
    Directory,
    Other,
    Unavailable,
}

#[derive(Serialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub(crate) struct FileListing {
    path: String,
    parent: Option<String>,
    root: String,
    workspace: String,
    kind: FileKind,
    size: Option<u64>,
    entries: Vec<FileEntry>,
    truncated: bool,
}

fn io_error(e: io::Error) -> ApiError {
    error(match e.kind() {
        io::ErrorKind::NotFound => StatusCode::NOT_FOUND,
        io::ErrorKind::PermissionDenied => StatusCode::FORBIDDEN,
        _ => StatusCode::BAD_REQUEST,
    }, e.to_string())
}

fn root(config: &Config) -> &Path {
    match &config.mode {
        ExecutionMode::Local { .. } => &config.workspace,
        ExecutionMode::Cell { .. } => Path::new("/"),
    }
}

fn resolve(config: &Config, value: Option<&str>) -> ApiResult<PathBuf> {
    let path = value.map(Path::new).unwrap_or(&config.workspace);
    if !path.is_absolute() || path.as_os_str().as_bytes().contains(&0) {
        return Err(super::bad("Use an absolute path in this cell"));
    }
    let path = path.canonicalize().map_err(io_error)?;
    if !path.starts_with(root(config)) {
        return Err(error(StatusCode::FORBIDDEN, "Local files must stay inside the workspace".into()));
    }
    Ok(path)
}

// Reopen a canonical path one component at a time. If a directory is replaced
// by a symlink after canonicalization, fail instead of following that link.
fn open(config: &Config, path: &Path) -> ApiResult<File> {
    let metadata = fs::metadata(path).map_err(io_error)?;
    if !metadata.is_file() && !metadata.is_dir() {
        return Err(super::bad("Only regular files and directories can be opened"));
    }
    let mut file = fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(root(config)).map_err(io_error)?;
    let components: Vec<_> = path.strip_prefix(root(config)).map_err(|_| super::bad("Invalid path"))?.components().collect();
    for (index, component) in components.iter().enumerate() {
        let Component::Normal(name) = component else {
            return Err(super::bad("Invalid path"));
        };
        let name = CString::new(name.as_bytes()).map_err(|_| super::bad("Invalid path"))?;
        let mut flags = libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC;
        if index + 1 < components.len() {
            flags |= libc::O_DIRECTORY;
        }
        let fd = unsafe { libc::openat(file.as_raw_fd(), name.as_ptr(), flags) };
        if fd < 0 {
            return Err(io_error(io::Error::last_os_error()));
        }
        file = unsafe { File::from_raw_fd(fd) };
    }
    Ok(file)
}

fn kind(metadata: &fs::Metadata) -> FileKind {
    if metadata.is_dir() {
        FileKind::Directory
    } else if metadata.is_file() {
        FileKind::File
    } else {
        FileKind::Other
    }
}

// read_dir("/proc/self/fd/N") works on Linux but not macOS. fdopendir
// enumerates the pinned directory on both, without re-resolving its path.
fn names(directory: &File) -> io::Result<(Vec<OsString>, bool)> {
    struct Directory(*mut libc::DIR);
    impl Drop for Directory {
        fn drop(&mut self) {
            unsafe { libc::closedir(self.0); }
        }
    }
    let fd = directory.try_clone()?.into_raw_fd();
    let dir = unsafe { libc::fdopendir(fd) };
    if dir.is_null() {
        let error = io::Error::last_os_error();
        unsafe { libc::close(fd); }
        return Err(error);
    }
    let dir = Directory(dir);
    let mut names = vec![];
    loop {
        #[cfg(target_os = "macos")]
        let errno = unsafe { libc::__error() };
        #[cfg(not(target_os = "macos"))]
        let errno = unsafe { libc::__errno_location() };
        unsafe { *errno = 0; }
        let entry = unsafe { libc::readdir(dir.0) };
        if entry.is_null() {
            let code = unsafe { *errno };
            if code != 0 {
                return Err(io::Error::from_raw_os_error(code));
            }
            return Ok((names, false));
        }
        let name = unsafe { CStr::from_ptr((*entry).d_name.as_ptr()) }.to_bytes();
        if name == b"." || name == b".." {
            continue;
        }
        if names.len() == 10_000 {
            return Ok((names, true));
        }
        names.push(OsString::from_vec(name.to_vec()));
    }
}

fn listing(config: &Config, value: Option<&str>) -> ApiResult<FileListing> {
    let path = resolve(config, value)?;
    let directory = open(config, &path)?;
    let metadata = directory.metadata().map_err(io_error)?;
    let mut entries = vec![];
    let mut truncated = false;
    if metadata.is_dir() {
        let (names, more) = names(&directory).map_err(io_error)?;
        truncated = more;
        for name in names {
            // Do not manufacture lossy names which could select a different file.
            let Some(name) = name.to_str().map(str::to_owned) else {
                continue;
            };
            let entry_path = path.join(&name);
            let link_metadata = fs::symlink_metadata(&entry_path).ok();
            let symlink = link_metadata.as_ref().is_some_and(|m| m.is_symlink());
            let metadata = if link_metadata.as_ref().is_some_and(|m| !m.is_symlink() && !m.is_dir() && !m.is_file()) {
                link_metadata
            } else {
                resolve(config, entry_path.to_str())
                    .and_then(|p| open(config, &p))
                    .and_then(|f| f.metadata().map_err(io_error))
                    .ok()
            };
            entries.push(FileEntry {
                name, path: entry_path.to_string_lossy().into_owned(), symlink,
                kind: metadata.as_ref().map(kind).unwrap_or(FileKind::Unavailable),
                size: metadata.as_ref().filter(|m| m.is_file()).map(|m| m.len()),
                modified: metadata.and_then(|m| m.modified().ok()).and_then(|t| t.duration_since(UNIX_EPOCH).ok()).map(|t| t.as_secs()),
            });
        }
        entries.sort_by(|a, b| (a.kind != FileKind::Directory, &a.name).cmp(&(b.kind != FileKind::Directory, &b.name)));
    }
    Ok(FileListing {
        parent: path.parent().filter(|p| p.starts_with(root(config))).map(|p| p.to_string_lossy().into_owned()),
        path: path.to_string_lossy().into_owned(), root: root(config).to_string_lossy().into_owned(),
        workspace: config.workspace.to_string_lossy().into_owned(), kind: kind(&metadata),
        size: metadata.is_file().then(|| metadata.len()), entries, truncated,
    })
}

pub(super) async fn list(
    State(store): State<Arc<Store>>,
    Query(query): Query<FileQuery>,
) -> ApiResult<Json<FileListing>> {
    tokio::task::spawn_blocking(move || listing(&store.config, query.path.as_deref()))
        .await
        .map_err(|e| error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
        .map(Json)
}

fn disposition(name: &str) -> String {
    let encoded: String = name.bytes().map(|b| {
        if b.is_ascii_alphanumeric() || b"-._".contains(&b) { (b as char).to_string() } else { format!("%{b:02X}") }
    }).collect();
    format!("attachment; filename=\"download\"; filename*=UTF-8''{encoded}")
}

pub(super) async fn download(
    State(store): State<Arc<Store>>,
    Query(query): Query<FileQuery>,
) -> ApiResult<Response> {
    let value = query.path.ok_or_else(|| super::bad("A download path is required"))?;
    let (path, file, metadata) = tokio::task::spawn_blocking(move || {
        let path = resolve(&store.config, Some(&value))?;
        let metadata = fs::metadata(&path).map_err(io_error)?;
        if !metadata.is_file() && !metadata.is_dir() {
            return Err(super::bad("Only regular files and directories can be downloaded"));
        }
        let file = open(&store.config, &path)?;
        let metadata = file.metadata().map_err(io_error)?;
        Ok::<_, ApiError>((path, file, metadata))
    })
    .await
    .map_err(|e| error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))??;
    let name = path.file_name().and_then(|s| s.to_str()).unwrap_or("filesystem");
    let (body, filename, content_type, size) = if metadata.is_file() {
        let size = metadata.len();
        let stream = async_stream::stream! {
            let mut file = tokio::fs::File::from_std(file);
            let mut remaining = size;
            while remaining > 0 {
                let mut bytes = vec![0; remaining.min(64 * 1024) as usize];
                let count = match file.read(&mut bytes).await {
                    Ok(0) => { yield Err(io::Error::new(io::ErrorKind::UnexpectedEof, "File changed during download")); break; }
                    Ok(count) => count,
                    Err(e) => { yield Err(e); break; }
                };
                remaining -= count as u64;
                bytes.truncate(count);
                yield Ok(bytes);
            }
        };
        (Body::from_stream(stream), name.to_owned(), "application/octet-stream", Some(size))
    } else if metadata.is_dir() {
        let mut command = tokio::process::Command::new("python3");
        command.args(["-I", "-c", include_str!("archive.py"), name])
            .stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::null())
            .kill_on_drop(true);
        // Only the async-signal-safe fchdir syscall runs between fork and exec.
        // Pin the archive's working directory even if its path is renamed.
        unsafe {
            command.pre_exec(move || {
                if libc::fchdir(file.as_raw_fd()) == 0 { Ok(()) }
                else { Err(io::Error::last_os_error()) }
            });
        }
        let mut child = command.spawn().map_err(io_error)?;
        let mut output = child.stdout.take().unwrap();
        let stream = async_stream::stream! {
            // Own the child inside the response: disconnecting cancels archiving.
            let mut child = child;
            loop {
                let mut bytes = vec![0; 64 * 1024];
                let count = match output.read(&mut bytes).await {
                    Ok(0) => break,
                    Ok(count) => count,
                    Err(e) => { yield Err(e); return; }
                };
                bytes.truncate(count);
                yield Ok(bytes);
            }
            match child.wait().await {
                Ok(status) if status.success() => {},
                Ok(_) => yield Err(io::Error::other("Archive incomplete: a file changed or could not be read")),
                Err(e) => yield Err(e),
            }
        };
        (Body::from_stream(stream), format!("{name}.tar.gz"), "application/gzip", None)
    } else {
        return Err(super::bad("Only regular files and directories can be downloaded"));
    };
    let mut response = Response::builder().header(header::CONTENT_TYPE, content_type)
        .header(header::CONTENT_DISPOSITION, disposition(&filename));
    if let Some(size) = size {
        response = response.header(header::CONTENT_LENGTH, size);
    }
    Ok(response.body(body).unwrap())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::symlink;

    struct Fixture { directory: PathBuf, config: Config }

    impl Fixture {
        fn new() -> Self {
            let directory = std::env::temp_dir().join(format!("cell-files-{}", crate::RequestId::generate()));
            fs::create_dir_all(directory.join("work/sub")).unwrap();
            let directory = directory.canonicalize().unwrap();
            fs::write(directory.join("private"), "outside").unwrap();
            fs::write(directory.join("work/sub/results.csv"), "inside").unwrap();
            let config = Config {
                mode: ExecutionMode::Local { listen: "127.0.0.1:0".parse().unwrap() },
                workspace: directory.join("work"), state: directory.join("state"),
                assets: directory.join("assets"), pi: "pi".into(),
            };
            Self { directory, config }
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) { let _ = fs::remove_dir_all(&self.directory); }
    }

    #[test]
    fn local_links_and_parent_paths_cannot_escape_the_workspace() {
        let f = Fixture::new();
        symlink(f.directory.join("private"), f.config.workspace.join("outside")).unwrap();
        symlink(f.config.workspace.join("sub"), f.config.workspace.join("inside")).unwrap();
        for path in [f.directory.join("private"), f.config.workspace.join("../private"), f.config.workspace.join("outside")] {
            assert_eq!(resolve(&f.config, path.to_str()).err().unwrap().status, StatusCode::FORBIDDEN);
        }
        let path = resolve(&f.config, f.config.workspace.join("inside/results.csv").to_str())
            .unwrap_or_else(|e| panic!("{}", e.message));
        assert_eq!(path, f.config.workspace.join("sub/results.csv"));
        let list = listing(&f.config, None).unwrap_or_else(|e| panic!("{}", e.message));
        assert!(list.parent.is_none());
        assert!(list.entries.iter().any(|entry| entry.name == "outside" && entry.kind == FileKind::Unavailable));
    }

    #[test]
    fn replacement_symlink_is_not_followed_after_resolution() {
        let f = Fixture::new();
        let path = resolve(&f.config, f.config.workspace.join("sub/results.csv").to_str())
            .unwrap_or_else(|e| panic!("{}", e.message));
        fs::rename(f.config.workspace.join("sub"), f.config.workspace.join("saved")).unwrap();
        symlink(f.config.workspace.join("saved"), f.config.workspace.join("sub")).unwrap();
        assert!(open(&f.config, &path).is_err(), "even a replacement link inside the workspace must be reopened explicitly");
    }

    #[test]
    fn attachment_names_cannot_inject_headers() {
        let value = disposition("résults\r\n\"; x=y.csv");
        assert!(!value.contains('\r') && !value.contains('\n'));
        assert!(value.contains("r%C3%A9sults%0D%0A%22%3B%20x%3Dy.csv"));
    }
}
