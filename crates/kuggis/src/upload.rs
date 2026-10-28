//! Pure upload inputs; clipboard capture is an optional local effect.
use crate::{
    client,
    config::{GuestPath, Name},
    Error, Result, Workspace,
};
use serde::Deserialize;
use std::{
    fs,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::{SystemTime, UNIX_EPOCH},
};

#[derive(Debug, PartialEq, Eq)]
pub enum Source {
    File {
        local: PathBuf,
        destination: GuestPath,
    },
    Clipboard,
}

#[derive(Debug)]
pub struct Options {
    pub source: Source,
    pub chat: Option<Name>,
    pub replace: bool,
}

impl Options {
    pub fn parse(args: &[String]) -> Result<Self> {
        let (mut paste, mut replace, mut chat, mut positional, mut literal) =
            (false, false, None, Vec::new(), false);
        let mut args = args.iter();
        while let Some(arg) = args.next() {
            match arg.as_str() {
                "--" if !literal => literal = true,
                "--paste" if !literal && !paste => paste = true,
                "--replace" if !literal && !replace => replace = true,
                "--chat" if !literal && chat.is_none() => {
                    chat = Some(Name::try_from(
                        args.next()
                            .ok_or_else(|| Error("--chat requires a name".into()))?
                            .clone(),
                    )?);
                }
                _ if !literal && arg.starts_with('-') => {
                    return Err(Error(format!("unknown or duplicate put option: {arg}")))
                }
                _ => positional.push(arg),
            }
        }
        let source = match (paste, positional.as_slice()) {
            (true, []) => Source::Clipboard,
            (false, [source]) => Source::File { local: PathBuf::from(source), destination: basename(Path::new(source))? },
            (false, [source, dest]) => Source::File { local: PathBuf::from(source), destination: GuestPath::try_from((*dest).clone())? },
            _ => return Err(Error("expected put FILE [DESTINATION] [--chat NAME] [--replace], or put --paste [--chat NAME]".into())),
        };
        Ok(Self {
            source,
            chat,
            replace,
        })
    }
}

pub fn basename(path: &Path) -> Result<GuestPath> {
    GuestPath::try_from(
        path.file_name()
            .and_then(|n| n.to_str())
            .ok_or_else(|| Error("source needs a filename; supply an explicit destination".into()))?
            .to_owned(),
    )
}

#[derive(Deserialize)]
#[serde(rename_all = "lowercase")]
enum Format {
    Png,
    Jpg,
    Txt,
}
impl Format {
    fn extension(&self) -> &'static str {
        match self {
            Self::Png => "png",
            Self::Jpg => "jpg",
            Self::Txt => "txt",
        }
    }
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Capture {
    format: Format,
}

struct Temporary(PathBuf);
impl Drop for Temporary {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

pub fn run(workspace: &Workspace, options: Options) -> Result<()> {
    let (source, path, _temporary) = match options.source {
        Source::File { local, destination } => (local, destination, None),
        Source::Clipboard => {
            let nonce = format!(
                "{}-{}",
                SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .map_err(|e| Error(e.to_string()))?
                    .as_nanos(),
                std::process::id()
            );
            let directory = std::env::temp_dir().join(format!("kuggis-clipboard-{nonce}"));
            let mut builder = fs::DirBuilder::new();
            #[cfg(unix)]
            {
                use std::os::unix::fs::DirBuilderExt;
                builder.mode(0o700);
            }
            builder.create(&directory)?;
            let temporary = Temporary(directory);
            let output = Command::new("python3")
                .args(["-c", include_str!("../runtime/clipboard.py")])
                .arg(&temporary.0)
                .stdin(Stdio::null())
                .stderr(Stdio::inherit())
                .output()?;
            if !output.status.success() {
                return Err(Error("clipboard capture failed; no file uploaded".into()));
            }
            let capture: Capture = serde_json::from_slice(&output.stdout)?;
            let ext = capture.format.extension();
            (
                temporary.0.join(format!("clipboard.{ext}")),
                GuestPath::try_from(format!(".kuggis/uploads/clipboard-{nonce}.{ext}"))?,
                Some(temporary),
            )
        }
    };
    let display = format!("/home/agent/workspace/{}", path.as_str());
    client::put_with_chat(
        workspace,
        &source,
        path,
        options.replace,
        options.chat.clone(),
    )?;
    println!("Uploaded: {display}");
    if let Some(chat) = options.chat {
        println!("Reference inserted into {} (not submitted).", chat.as_str());
    }
    Ok(())
}
