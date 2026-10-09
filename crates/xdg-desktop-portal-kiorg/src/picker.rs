//! Spawns `kiorg --picker ...` and turns its stdout into a typed [`Outcome`].
//!
//! Contract with Kiorg (adjust `PickerRequest::to_args` if the real flags differ):
//!
//! ```text
//! kiorg --picker [--directory | --save] [--multiple]
//!       [--title T] [--accept-label L]
//!       [--current-folder DIR] [--current-name NAME]
//! ```
//! * exit 0 + one absolute path per line on stdout  -> selection
//! * exit 0 + empty stdout, or exit 1               -> user cancelled
//! * anything else                                  -> error

use std::{
    ffi::OsString,
    os::unix::ffi::{OsStrExt, OsStringExt},
    path::PathBuf,
    process::Stdio,
};

use tokio::{io::AsyncReadExt, process::Command, sync::Notify};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Mode {
    #[default]
    OpenFile,
    OpenDirectory,
    Save,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PickerRequest {
    pub mode: Mode,
    pub multiple: bool,
    pub title: Option<String>,
    pub accept_label: Option<String>,
    pub current_folder: Option<PathBuf>,
    pub current_name: Option<String>,
}

impl PickerRequest {
    /// The single place that knows Kiorg's picker CLI.
    pub fn to_args(&self) -> Vec<OsString> {
        let mut a: Vec<OsString> = vec!["--picker".into()];
        match self.mode {
            Mode::OpenFile => {}
            Mode::OpenDirectory => a.push("--directory".into()),
            Mode::Save => a.push("--save".into()),
        }
        if self.multiple && self.mode != Mode::Save {
            a.push("--multiple".into());
        }
        let mut opt = |flag: &str, val: Option<OsString>| {
            if let Some(v) = val.filter(|v| !v.is_empty()) {
                a.push(flag.into());
                a.push(v);
            }
        };
        opt("--title", self.title.clone().map(Into::into));
        opt("--accept-label", self.accept_label.clone().map(Into::into));
        opt(
            "--current-folder",
            self.current_folder.clone().map(Into::into),
        );
        opt("--current-name", self.current_name.clone().map(Into::into));
        a
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    Selected(Vec<PathBuf>),
    Cancelled,
    Failed(String),
}

/// Parses stdout: one absolute path per line; empty and relative lines are ignored.
pub fn parse_output(stdout: &[u8]) -> Vec<PathBuf> {
    stdout
        .split(|b| *b == b'\n')
        .map(|l| l.strip_suffix(b"\r").unwrap_or(l))
        .filter(|l| l.first() == Some(&b'/'))
        .map(|l| PathBuf::from(OsString::from_vec(l.to_vec())))
        .collect()
}

pub fn interpret(stdout: &[u8], success: bool, code: Option<i32>) -> Outcome {
    let paths = parse_output(stdout);
    if success {
        if paths.is_empty() {
            Outcome::Cancelled
        } else {
            Outcome::Selected(paths)
        }
    } else if code == Some(1) {
        Outcome::Cancelled
    } else {
        Outcome::Failed(format!("picker exited abnormally (code: {code:?})"))
    }
}

/// `/home/a b/ä.txt` -> `file:///home/a%20b/%C3%A4.txt`
pub fn path_to_uri(path: &std::path::Path) -> String {
    let mut out = String::from("file://");
    for &b in path.as_os_str().as_bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' | b'/' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

#[derive(Debug, Clone)]
pub struct Picker {
    program: OsString,
    prefix_args: Vec<OsString>,
}

impl Picker {
    pub fn new(program: impl Into<OsString>) -> Self {
        Self {
            program: program.into(),
            prefix_args: Vec::new(),
        }
    }

    /// Args placed before the picker args (handy for tests: `sh script.sh`).
    pub fn with_prefix_args<I, S>(mut self, args: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<OsString>,
    {
        self.prefix_args = args.into_iter().map(Into::into).collect();
        self
    }

    /// Uses `$KIORG_PICKER_BIN`, falling back to `kiorg` from `$PATH`.
    pub fn from_env() -> Self {
        Self::new(std::env::var_os("KIORG_PICKER_BIN").unwrap_or_else(|| "kiorg".into()))
    }

    /// Runs the picker. If `cancel` is notified (portal `Request.Close`), the child is killed.
    pub async fn run(&self, req: &PickerRequest, cancel: &Notify) -> Outcome {
        let mut child = match Command::new(&self.program)
            .args(&self.prefix_args)
            .args(req.to_args())
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .kill_on_drop(true)
            .spawn()
        {
            Ok(c) => c,
            Err(e) => return Outcome::Failed(format!("cannot start {:?}: {e}", self.program)),
        };
        let mut stdout = child.stdout.take().expect("stdout is piped");

        let work = async {
            let mut buf = Vec::new();
            stdout.read_to_end(&mut buf).await?;
            let status = child.wait().await?;
            std::io::Result::Ok((buf, status))
        };

        tokio::select! {
            _ = cancel.notified() => Outcome::Cancelled, // child is killed on drop
            res = work => match res {
                Ok((buf, st)) => interpret(&buf, st.success(), st.code()),
                Err(e) => Outcome::Failed(e.to_string()),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{path::Path, sync::Arc, time::Duration};

    fn script(body: &str) -> (tempfile::TempDir, Picker) {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("mock.sh");
        std::fs::write(&p, body).unwrap();
        // run via `sh` to avoid ETXTBSY races with freshly written executables
        (dir, Picker::new("sh").with_prefix_args([p]))
    }

    #[test]
    fn parses_lines() {
        let out = parse_output(b"/a/b\n\nrelative\n/c d\r\n");
        assert_eq!(out, vec![PathBuf::from("/a/b"), PathBuf::from("/c d")]);
    }

    #[test]
    fn uri_encoding() {
        assert_eq!(
            path_to_uri(Path::new("/home/a b/ä.txt")),
            "file:///home/a%20b/%C3%A4.txt"
        );
    }

    #[test]
    fn args() {
        let r = PickerRequest {
            mode: Mode::OpenDirectory,
            multiple: true,
            title: Some("Pick".into()),
            current_folder: Some("/srv".into()),
            ..Default::default()
        };
        let a: Vec<_> = r
            .to_args()
            .into_iter()
            .map(|s| s.into_string().unwrap())
            .collect();
        assert_eq!(
            a,
            [
                "--picker",
                "--directory",
                "--multiple",
                "--title",
                "Pick",
                "--current-folder",
                "/srv"
            ]
        );
    }

    #[tokio::test]
    async fn selection() {
        let (_d, p) = script("printf '%s\\n' /tmp/a '/tmp/b c'\n");
        let out = p.run(&PickerRequest::default(), &Notify::new()).await;
        assert_eq!(
            out,
            Outcome::Selected(vec!["/tmp/a".into(), "/tmp/b c".into()])
        );
    }

    #[tokio::test]
    async fn args_reach_child() {
        let (_d, p) = script("printf '%s\\n' \"$@\"\n");
        let req = PickerRequest {
            current_folder: Some("/srv/x".into()),
            ..Default::default()
        };
        // flags are dropped by parse_output (not absolute), the folder value survives
        assert_eq!(
            p.run(&req, &Notify::new()).await,
            Outcome::Selected(vec!["/srv/x".into()])
        );
    }

    #[tokio::test]
    async fn cancelled_by_exit_code() {
        let (_d, p) = script("exit 1\n");
        assert_eq!(
            p.run(&PickerRequest::default(), &Notify::new()).await,
            Outcome::Cancelled
        );
    }

    #[tokio::test]
    async fn failure() {
        let (_d, p) = script("exit 7\n");
        assert!(matches!(
            p.run(&PickerRequest::default(), &Notify::new()).await,
            Outcome::Failed(_)
        ));
    }

    #[tokio::test]
    async fn close_kills_child() {
        let (_d, p) = script("sleep 30\n");
        let n = Arc::new(Notify::new());
        let n2 = n.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(100)).await;
            n2.notify_one();
        });
        let out =
            tokio::time::timeout(Duration::from_secs(5), p.run(&PickerRequest::default(), &n))
                .await
                .expect("must return promptly");
        assert_eq!(out, Outcome::Cancelled);
    }
}
