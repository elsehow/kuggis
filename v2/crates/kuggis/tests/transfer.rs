use kuggis::transfer::{self, PendingFile};
use std::{
    fs::{self, File},
    io::{BufReader, Cursor},
    path::PathBuf,
    sync::atomic::{AtomicUsize, Ordering},
};

struct Directory(PathBuf);
impl Directory {
    fn new() -> Self {
        static COUNT: AtomicUsize = AtomicUsize::new(0);
        let path = std::env::temp_dir().join(format!(
            "kuggis-test-{}-{}",
            std::process::id(),
            COUNT.fetch_add(1, Ordering::SeqCst)
        ));
        fs::create_dir(&path).unwrap();
        Self(path)
    }
}
impl Drop for Directory {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).unwrap();
    }
}

fn framed(dir: &Directory, bytes: &[u8]) -> Vec<u8> {
    let source = dir.0.join("source");
    fs::write(&source, bytes).unwrap();
    let mut frame = Vec::new();
    transfer::send(&mut File::open(source).unwrap(), &mut frame).unwrap();
    frame
}

#[test]
fn arbitrary_binary_round_trips_and_publication_is_atomic() {
    let dir = Directory::new();
    let bytes: Vec<_> = (0..10000).map(|n| (n % 256) as u8).collect();
    let frame = framed(&dir, &bytes);
    let destination = dir.0.join("out");
    let mut pending = PendingFile::new(&destination).unwrap();
    transfer::receive(&mut BufReader::new(Cursor::new(frame)), &mut pending.file).unwrap();
    assert!(!destination.exists());
    pending.commit(false).unwrap();
    assert_eq!(fs::read(destination).unwrap(), bytes);
}

#[test]
fn partial_corrupt_and_extra_bytes_never_publish() {
    let dir = Directory::new();
    let frame = framed(&dir, b"content\0\xff");
    let mut corrupted = frame.clone();
    *corrupted.last_mut().unwrap() ^= 1;
    let mut excess = frame.clone();
    excess.push(1);
    for frame in [frame[..frame.len() - 1].to_vec(), corrupted, excess] {
        let destination = dir.0.join("out");
        let mut pending = PendingFile::new(&destination).unwrap();
        assert!(
            transfer::receive(&mut BufReader::new(Cursor::new(frame)), &mut pending.file).is_err()
        );
        drop(pending);
        assert!(!destination.exists());
    }
    assert_eq!(fs::read_dir(&dir.0).unwrap().count(), 1);
}

#[test]
fn existing_destination_requires_explicit_replace() {
    let dir = Directory::new();
    let destination = dir.0.join("out");
    fs::write(&destination, b"old").unwrap();
    let frame = framed(&dir, b"new");
    for replace in [false, true] {
        let mut pending = PendingFile::new(&destination).unwrap();
        transfer::receive(
            &mut BufReader::new(Cursor::new(frame.clone())),
            &mut pending.file,
        )
        .unwrap();
        assert_eq!(pending.commit(replace).is_ok(), replace);
        assert_eq!(
            fs::read(&destination).unwrap(),
            if replace { b"new" } else { b"old" }
        );
    }
}

#[test]
fn empty_files_work_and_untrusted_lengths_are_bounded() {
    let dir = Directory::new();
    let destination = dir.0.join("out");
    let mut pending = PendingFile::new(&destination).unwrap();
    let frame = framed(&dir, b"");
    transfer::receive(&mut BufReader::new(Cursor::new(frame)), &mut pending.file).unwrap();
    pending.commit(false).unwrap();
    assert_eq!(fs::metadata(&destination).unwrap().len(), 0);
    let frame = format!(
        "{{\"size\":18446744073709551615,\"sha256\":\"{}\"}}\n",
        "0".repeat(64)
    );
    let mut pending = PendingFile::new(&dir.0.join("too-big")).unwrap();
    assert!(transfer::receive(&mut BufReader::new(Cursor::new(frame)), &mut pending.file).is_err());
}
