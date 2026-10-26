//! Read-only access to the folder the agent shares with the user, using the
//! chat process's own OS identity. Nothing outside it is listed or downloaded,
//! so agent credentials and chat state stay private. The operator only relays
//! bytes; it never resolves guest paths on the host.
use super::{error, ApiError, ApiResult, Store};
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
        unix::{
            ffi::{OsStrExt, OsStringExt},
            fs::OpenOptionsExt,
        },
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
    kind: FileKind,
    size: Option<u64>,
    entries: Vec<FileEntry>,
    truncated: bool,
}

fn io_error(e: io::Error) -> ApiError {
    error(
        match e.kind() {
            io::ErrorKind::NotFound => StatusCode::NOT_FOUND,
            io::ErrorKind::PermissionDenied => StatusCode::FORBIDDEN,
            _ => StatusCode::BAD_REQUEST,
        },
        e.to_string(),
    )
}

/// The shared folder is pinned once at startup. Never reopen it by its pathname:
/// O_NOFOLLOW on that pathname would not protect its ancestors from replacement.
pub(super) struct Shared {
    path: PathBuf,
    directory: File,
}

impl Shared {
    pub(super) fn new(path: &Path) -> io::Result<Self> {
        let mut directory = fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open("/")?;
        // serve() canonicalizes the configured path before pinning it. Refuse
        // any symlink introduced between that step and these descriptor opens.
        for component in path.components() {
            match component {
                Component::RootDir => {}
                Component::Normal(name) => {
                    directory = open_at(&directory, name, libc::O_DIRECTORY)?;
                }
                _ => return Err(io::Error::other("Shared folder must be canonical")),
            }
        }
        if !path.is_absolute() {
            return Err(io::Error::other("Shared folder must be absolute"));
        }
        Ok(Self {
            path: path.to_owned(),
            directory,
        })
    }

    fn outside(&self) -> ApiError {
        error(
            StatusCode::FORBIDDEN,
            format!(
                "Only the shared folder {} can be browsed. Ask the agent to copy this there.",
                self.path.display()
            ),
        )
    }
}

fn shared_folder(shared: Option<&Shared>) -> ApiResult<&Shared> {
    shared.ok_or_else(|| {
        error(
            StatusCode::NOT_FOUND,
            "Files are not set up in this cell. Re-run cell setup to create its shared folder."
                .into(),
        )
    })
}

fn c_name(name: &std::ffi::OsStr) -> io::Result<CString> {
    CString::new(name.as_bytes()).map_err(|_| io::Error::other("Invalid path"))
}

fn open_at(directory: &File, name: &std::ffi::OsStr, flags: i32) -> io::Result<File> {
    let name = c_name(name)?;
    let fd = unsafe {
        libc::openat(
            directory.as_raw_fd(),
            name.as_ptr(),
            libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC | flags,
        )
    };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(unsafe { File::from_raw_fd(fd) })
}

fn stat_at(directory: &File, name: &std::ffi::OsStr) -> io::Result<libc::stat> {
    let name = c_name(name)?;
    let mut stat = std::mem::MaybeUninit::uninit();
    if unsafe {
        libc::fstatat(
            directory.as_raw_fd(),
            name.as_ptr(),
            stat.as_mut_ptr(),
            libc::AT_SYMLINK_NOFOLLOW,
        )
    } < 0
    {
        return Err(io::Error::last_os_error());
    }
    Ok(unsafe { stat.assume_init() })
}

fn link_at(directory: &File, name: &std::ffi::OsStr) -> io::Result<PathBuf> {
    let name = c_name(name)?;
    let mut bytes = vec![0; 256];
    loop {
        let n = unsafe {
            libc::readlinkat(
                directory.as_raw_fd(),
                name.as_ptr(),
                bytes.as_mut_ptr().cast(),
                bytes.len(),
            )
        };
        if n < 0 {
            return Err(io::Error::last_os_error());
        }
        if (n as usize) < bytes.len() {
            bytes.truncate(n as usize);
            return Ok(PathBuf::from(OsString::from_vec(bytes)));
        }
        if bytes.len() >= 64 * 1024 {
            return Err(io::Error::other("Symbolic link is too long"));
        }
        bytes.resize(bytes.len() * 2, 0);
    }
}

// Resolve links ourselves, only relative to descriptors beneath the pinned
// root. canonicalize() would probe outside targets (including missing ones)
// before we could reject them. Each symlink is read, checked, then expanded;
// the actual opens never follow symlinks, including concurrently replaced ones.
fn resolve(shared: Option<&Shared>, value: Option<&str>) -> ApiResult<(PathBuf, File)> {
    let shared = shared_folder(shared)?;
    let path = value.map(Path::new).unwrap_or(&shared.path);
    if !path.is_absolute() || path.as_os_str().as_bytes().contains(&0) {
        return Err(super::bad("Use an absolute path in this cell"));
    }
    let relative = path
        .strip_prefix(&shared.path)
        .map_err(|_| shared.outside())?;
    if path.components().any(|c| c == Component::ParentDir) {
        return Err(shared.outside());
    }
    let mut pending: std::collections::VecDeque<OsString> = relative
        .components()
        .map(|c| c.as_os_str().to_owned())
        .collect();
    // dup() would share the directory enumeration offset across requests.
    let root = open_at(
        &shared.directory,
        std::ffi::OsStr::new("."),
        libc::O_DIRECTORY,
    )
    .map_err(io_error)?;
    let mut directories = vec![root];
    let mut resolved = PathBuf::new();
    let mut links = 0;
    while let Some(name) = pending.pop_front() {
        if name == "." {
            continue;
        }
        if name == ".." {
            if directories.len() == 1 {
                return Err(shared.outside());
            }
            directories.pop();
            resolved.pop();
            continue;
        }
        let directory = directories.last().unwrap();
        let stat = stat_at(directory, &name).map_err(io_error)?;
        if stat.st_mode & libc::S_IFMT == libc::S_IFLNK {
            links += 1;
            if links > 40 {
                return Err(super::bad("Too many symbolic links"));
            }
            let target = link_at(directory, &name).map_err(io_error)?;
            let target = if target.is_absolute() {
                let relative = target
                    .strip_prefix(&shared.path)
                    .map_err(|_| shared.outside())?;
                directories.truncate(1);
                resolved.clear();
                relative
            } else {
                &target
            };
            // Reject an escape before looking up ANY target components, even
            // if an earlier component is missing. Runtime .. checks below also
            // catch escapes through intermediate links which change the depth.
            let mut depth = directories.len() - 1;
            for component in target.components() {
                match component {
                    Component::ParentDir if depth == 0 => return Err(shared.outside()),
                    Component::ParentDir => depth -= 1,
                    Component::Normal(_) => depth += 1,
                    _ => {}
                }
            }
            for component in target.components().rev() {
                pending.push_front(component.as_os_str().to_owned());
            }
            continue;
        }
        let mode = stat.st_mode & libc::S_IFMT;
        if mode != libc::S_IFREG && mode != libc::S_IFDIR {
            return Err(super::bad(
                "Only regular files and directories can be opened",
            ));
        }
        let file = open_at(
            directory,
            &name,
            if pending.is_empty() {
                0
            } else {
                libc::O_DIRECTORY
            },
        )
        .map_err(io_error)?;
        resolved.push(&name);
        if pending.is_empty() {
            return Ok((shared.path.join(resolved), file));
        }
        directories.push(file);
    }
    let path = if resolved.as_os_str().is_empty() {
        shared.path.clone()
    } else {
        shared.path.join(resolved)
    };
    Ok((path, directories.pop().unwrap()))
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
            unsafe {
                libc::closedir(self.0);
            }
        }
    }
    let fd = directory.try_clone()?.into_raw_fd();
    let dir = unsafe { libc::fdopendir(fd) };
    if dir.is_null() {
        let error = io::Error::last_os_error();
        unsafe {
            libc::close(fd);
        }
        return Err(error);
    }
    let dir = Directory(dir);
    let mut names = vec![];
    loop {
        #[cfg(target_os = "macos")]
        let errno = unsafe { libc::__error() };
        #[cfg(not(target_os = "macos"))]
        let errno = unsafe { libc::__errno_location() };
        unsafe {
            *errno = 0;
        }
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

fn listing(shared: Option<&Shared>, value: Option<&str>) -> ApiResult<FileListing> {
    let root = shared_folder(shared)?;
    let (path, directory) = resolve(shared, value)?;
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
            let stat = stat_at(&directory, std::ffi::OsStr::new(&name)).ok();
            let mode = stat.as_ref().map(|s| s.st_mode & libc::S_IFMT);
            let symlink = mode == Some(libc::S_IFLNK);
            let special = mode
                .is_some_and(|m| m != libc::S_IFLNK && m != libc::S_IFDIR && m != libc::S_IFREG);
            let metadata = if special {
                None
            } else {
                resolve(shared, entry_path.to_str())
                    .and_then(|(_, f)| f.metadata().map_err(io_error))
                    .ok()
            };
            entries.push(FileEntry {
                name,
                path: entry_path.to_string_lossy().into_owned(),
                symlink,
                kind: if special {
                    FileKind::Other
                } else {
                    metadata.as_ref().map(kind).unwrap_or(FileKind::Unavailable)
                },
                size: metadata.as_ref().filter(|m| m.is_file()).map(|m| m.len()),
                modified: if special {
                    stat.and_then(|s| u64::try_from(s.st_mtime).ok())
                } else {
                    metadata
                        .and_then(|m| m.modified().ok())
                        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                        .map(|t| t.as_secs())
                },
            });
        }
        entries.sort_by(|a, b| {
            (a.kind != FileKind::Directory, &a.name).cmp(&(b.kind != FileKind::Directory, &b.name))
        });
    }
    Ok(FileListing {
        parent: path
            .parent()
            .filter(|p| p.starts_with(&root.path))
            .map(|p| p.to_string_lossy().into_owned()),
        path: path.to_string_lossy().into_owned(),
        root: root.path.to_string_lossy().into_owned(),
        kind: kind(&metadata),
        size: metadata.is_file().then(|| metadata.len()),
        entries,
        truncated,
    })
}

pub(super) async fn list(
    State(store): State<Arc<Store>>,
    Query(query): Query<FileQuery>,
) -> ApiResult<Json<FileListing>> {
    tokio::task::spawn_blocking(move || listing(store.shared.as_ref(), query.path.as_deref()))
        .await
        .map_err(|e| error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
        .map(Json)
}

fn disposition(name: &str) -> String {
    let encoded: String = name
        .bytes()
        .map(|b| {
            if b.is_ascii_alphanumeric() || b"-._".contains(&b) {
                (b as char).to_string()
            } else {
                format!("%{b:02X}")
            }
        })
        .collect();
    format!("attachment; filename=\"download\"; filename*=UTF-8''{encoded}")
}

pub(super) async fn download(
    State(store): State<Arc<Store>>,
    Query(query): Query<FileQuery>,
) -> ApiResult<Response> {
    let value = query
        .path
        .ok_or_else(|| super::bad("A download path is required"))?;
    let (path, file, metadata) = tokio::task::spawn_blocking(move || {
        let (path, file) = resolve(store.shared.as_ref(), Some(&value))?;
        let metadata = file.metadata().map_err(io_error)?;
        Ok::<_, ApiError>((path, file, metadata))
    })
    .await
    .map_err(|e| error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))??;
    let name = path
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("filesystem");
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
        (
            Body::from_stream(stream),
            name.to_owned(),
            "application/octet-stream",
            Some(size),
        )
    } else if metadata.is_dir() {
        let mut command = tokio::process::Command::new("python3");
        command
            .args(["-I", "-c", include_str!("archive.py"), name])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true);
        // Only the async-signal-safe fchdir syscall runs between fork and exec.
        // Pin the archive's working directory even if its path is renamed.
        unsafe {
            command.pre_exec(move || {
                if libc::fchdir(file.as_raw_fd()) == 0 {
                    Ok(())
                } else {
                    Err(io::Error::last_os_error())
                }
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
        (
            Body::from_stream(stream),
            format!("{name}.tar.gz"),
            "application/gzip",
            None,
        )
    } else {
        return Err(super::bad(
            "Only regular files and directories can be downloaded",
        ));
    };
    let mut response = Response::builder()
        .header(header::CONTENT_TYPE, content_type)
        .header(header::CONTENT_DISPOSITION, disposition(&filename));
    if let Some(size) = size {
        response = response.header(header::CONTENT_LENGTH, size);
    }
    Ok(response.body(body).unwrap())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{io::Read, os::unix::fs::symlink};

    struct Fixture {
        directory: PathBuf,
        shared: Shared,
    }

    impl Fixture {
        fn new() -> Self {
            let directory =
                std::env::temp_dir().join(format!("cell-files-{}", crate::RequestId::generate()));
            fs::create_dir_all(directory.join("shared/sub")).unwrap();
            fs::create_dir_all(directory.join("workspace")).unwrap();
            let directory = directory.canonicalize().unwrap();
            fs::write(directory.join("private"), "outside").unwrap();
            fs::write(directory.join("workspace/notes.md"), "unshared work").unwrap();
            fs::write(directory.join("shared/sub/results.csv"), "inside").unwrap();
            let shared = Shared::new(&directory.join("shared")).unwrap();
            Self { directory, shared }
        }

        fn resolve(&self, path: &Path) -> ApiResult<(PathBuf, File)> {
            resolve(Some(&self.shared), path.to_str())
        }

        fn list(&self) -> FileListing {
            listing(Some(&self.shared), None).unwrap_or_else(|e| panic!("{}", e.message))
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.directory);
        }
    }

    fn contents(mut file: File) -> String {
        let mut bytes = String::new();
        file.read_to_string(&mut bytes).unwrap();
        bytes
    }

    #[test]
    fn links_and_parent_paths_cannot_escape_the_shared_folder() {
        let f = Fixture::new();
        let shared = &f.shared.path;
        symlink(f.directory.join("private"), shared.join("outside")).unwrap();
        symlink(shared.join("sub"), shared.join("inside")).unwrap();
        for path in [
            f.directory.join("private"),
            f.directory.join("workspace/notes.md"),
            shared.join("../private"),
            shared.join("outside"),
            f.directory.join("missing"),
            shared.join("../missing"),
        ] {
            assert_eq!(
                f.resolve(&path).err().unwrap().status,
                StatusCode::FORBIDDEN,
                "{}",
                path.display()
            );
        }
        assert_eq!(
            f.resolve(&shared.join("missing")).err().unwrap().status,
            StatusCode::NOT_FOUND
        );
        let (path, file) = f
            .resolve(&shared.join("inside/results.csv"))
            .unwrap_or_else(|e| panic!("{}", e.message));
        assert_eq!(path, shared.join("sub/results.csv"));
        assert_eq!(contents(file), "inside");
        let list = f.list();
        assert_eq!(list.path, shared.to_string_lossy());
        assert!(list.parent.is_none());
        assert!(list
            .entries
            .iter()
            .any(|entry| entry.name == "outside" && entry.kind == FileKind::Unavailable));
    }

    #[test]
    fn outside_links_do_not_probe_existence() {
        let f = Fixture::new();
        let root = &f.shared.path;
        symlink(f.directory.join("workspace"), root.join("absolute")).unwrap();
        symlink("../workspace", root.join("relative")).unwrap();
        symlink("../missing", root.join("dangling")).unwrap();
        symlink("missing/../../workspace", root.join("missing-first")).unwrap();
        symlink(
            root.join("missing/../../workspace"),
            root.join("absolute-missing-first"),
        )
        .unwrap();
        for link in [
            "absolute",
            "relative",
            "dangling",
            "missing-first",
            "absolute-missing-first",
        ] {
            for suffix in ["", "notes.md", "missing.md"] {
                let path = root.join(link).join(suffix);
                assert_eq!(
                    f.resolve(&path).err().unwrap().status,
                    StatusCode::FORBIDDEN,
                    "{}",
                    path.display()
                );
                assert_eq!(
                    listing(Some(&f.shared), path.to_str())
                        .err()
                        .unwrap()
                        .status,
                    StatusCode::FORBIDDEN
                );
            }
        }
        assert!(f
            .list()
            .entries
            .iter()
            .filter(|entry| entry.symlink)
            .all(|entry| entry.kind == FileKind::Unavailable));
    }

    #[test]
    fn relative_links_are_resolved_in_order_and_loops_are_bounded() {
        let f = Fixture::new();
        let root = &f.shared.path;
        symlink("..", root.join("sub/up")).unwrap();
        symlink("sub/up/sub/results.csv", root.join("valid")).unwrap();
        let (path, file) = f
            .resolve(&root.join("valid"))
            .unwrap_or_else(|e| panic!("{}", e.message));
        assert_eq!(path, root.join("sub/results.csv"));
        assert_eq!(contents(file), "inside");
        // Lexically this target stays inside; expanding 'up' changes its depth.
        symlink("sub/up/../private", root.join("escape")).unwrap();
        assert_eq!(
            f.resolve(&root.join("escape")).err().unwrap().status,
            StatusCode::FORBIDDEN
        );
        symlink("loop", root.join("loop")).unwrap();
        assert_eq!(
            f.resolve(&root.join("loop")).err().unwrap().status,
            StatusCode::BAD_REQUEST
        );
    }

    #[test]
    fn resolved_file_survives_replacement_after_resolution() {
        let f = Fixture::new();
        let root = &f.shared.path;
        let (_, file) = f
            .resolve(&root.join("sub/results.csv"))
            .unwrap_or_else(|e| panic!("{}", e.message));
        fs::write(f.directory.join("workspace/results.csv"), "outside").unwrap();
        fs::rename(root.join("sub"), root.join("saved")).unwrap();
        symlink(f.directory.join("workspace"), root.join("sub")).unwrap();
        assert_eq!(contents(file), "inside");
        assert_eq!(
            f.resolve(&root.join("sub/results.csv"))
                .err()
                .unwrap()
                .status,
            StatusCode::FORBIDDEN
        );
    }

    #[test]
    fn nofollow_open_refuses_a_replacement_symlink() {
        let f = Fixture::new();
        let name = std::ffi::OsStr::new("sub");
        let stat = stat_at(&f.shared.directory, name).unwrap();
        assert_eq!(stat.st_mode & libc::S_IFMT, libc::S_IFDIR);
        fs::rename(f.shared.path.join("sub"), f.shared.path.join("saved")).unwrap();
        symlink("saved", f.shared.path.join("sub")).unwrap();
        assert!(open_at(&f.shared.directory, name, libc::O_DIRECTORY).is_err());
    }

    #[test]
    fn pinned_root_survives_ancestor_and_root_replacement() {
        for ancestor in [false, true] {
            let mut f = Fixture::new();
            let parent = f.shared.path.clone();
            if ancestor {
                f.shared = Shared::new(&parent.join("sub")).unwrap();
            }
            let root = f.shared.path.clone();
            let leaf = if ancestor {
                "results.csv"
            } else {
                "sub/results.csv"
            };
            fs::create_dir_all(f.directory.join("workspace/sub")).unwrap();
            fs::write(
                f.directory.join("workspace/sub/results.csv"),
                "SECRET OUTSIDE SHARED ROOT",
            )
            .unwrap();
            fs::rename(&parent, f.directory.join("saved")).unwrap();
            symlink(f.directory.join("workspace"), &parent).unwrap();
            // Refuse to pin through a replaced ancestor, even though the final
            // component is still a directory, not a symlink.
            assert!(Shared::new(&root).is_err());
            let (path, file) = f
                .resolve(&root.join(leaf))
                .unwrap_or_else(|e| panic!("{}", e.message));
            assert_eq!(path, root.join(leaf));
            assert_eq!(contents(file), "inside");
            // Repeated listings need independent offsets on the pinned root.
            for _ in 0..3 {
                let list = f.list();
                assert_eq!(list.entries.len(), 1);
                assert_eq!(
                    list.entries[0].name,
                    if ancestor { "results.csv" } else { "sub" }
                );
            }
        }
    }

    #[test]
    fn special_files_are_listed_but_never_opened() {
        let f = Fixture::new();
        let pipe = f.shared.path.join("pipe");
        let name = c_name(pipe.as_os_str()).unwrap();
        assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
        assert_eq!(
            f.resolve(&pipe).err().unwrap().status,
            StatusCode::BAD_REQUEST
        );
        assert!(f
            .list()
            .entries
            .iter()
            .any(|entry| entry.name == "pipe" && entry.kind == FileKind::Other));
    }

    #[test]
    fn cells_set_up_before_shared_folders_offer_no_files() {
        for error in [
            listing(None, None).err().unwrap(),
            resolve(None, Some("/home/agent/shared/results"))
                .err()
                .unwrap(),
        ] {
            assert_eq!(error.status, StatusCode::NOT_FOUND);
            assert!(
                error.message.contains("Re-run cell setup"),
                "{}",
                error.message
            );
        }
    }

    #[test]
    fn attachment_names_cannot_inject_headers() {
        let value = disposition("résults\r\n\"; x=y.csv");
        assert!(!value.contains('\r') && !value.contains('\n'));
        assert!(value.contains("r%C3%A9sults%0D%0A%22%3B%20x%3Dy.csv"));
    }
}
