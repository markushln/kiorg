//! "Open in editor": starts `$KIORG_EDITOR` (default `nvim`) in a terminal emulator window of
//! its own. The editor is a separate process: it survives Kiorg, and Kiorg's embedded terminal
//! is not involved at all.
//!
//! Terminal selection, first match wins:
//! 1. `$KIORG_TERMINAL`: full command prefix, used verbatim, e.g. `alacritty -e` or `kitty`
//! 2. `$TERMINAL`: a single word gets the matching "run this command" flag appended
//!    (`alacritty` -> `alacritty -e`), several words are used verbatim
//! 3. the first well-known emulator found in `$PATH`

use std::path::Path;
use std::process::{Command, Stdio};

const FALLBACK_TERMINALS: &[&str] = &[
    "kitty",
    "alacritty",
    "foot",
    "wezterm",
    "ghostty",
    "gnome-terminal",
    "konsole",
    "xfce4-terminal",
    "xterm",
    "x-terminal-emulator",
];

fn split_words(value: Option<&str>) -> Vec<String> {
    value
        .unwrap_or_default()
        .split_whitespace()
        .map(str::to_owned)
        .collect()
}

/// Editor command: `$KIORG_EDITOR` split on whitespace (e.g. `nvim -R`), default `nvim`.
#[must_use]
pub fn editor_command() -> Vec<String> {
    editor_command_from(std::env::var("KIORG_EDITOR").ok().as_deref())
}

fn editor_command_from(value: Option<&str>) -> Vec<String> {
    let parts = split_words(value);
    if parts.is_empty() {
        vec!["nvim".to_owned()]
    } else {
        parts
    }
}

/// Arguments that make a terminal emulator run the rest of the command line.
fn exec_flag(program: &str) -> &'static [&'static str] {
    let name = Path::new(program)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or(program);
    match name {
        "kitty" | "foot" | "footclient" => &[],
        "wezterm" => &["start", "--"],
        "gnome-terminal" => &["--"],
        "xfce4-terminal" | "terminator" => &["-x"],
        _ => &["-e"],
    }
}

fn is_in_path(program: &str) -> bool {
    #[cfg(unix)]
    let is_executable = |p: &Path| {
        use std::os::unix::fs::PermissionsExt;
        p.metadata()
            .is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
    };
    #[cfg(not(unix))]
    let is_executable = |p: &Path| p.is_file();
    if program.contains('/') {
        return is_executable(Path::new(program));
    }
    std::env::var_os("PATH").is_some_and(|paths| {
        std::env::split_paths(&paths).any(|dir| is_executable(&dir.join(program)))
    })
}

/// Pick the terminal command prefix. `available` tells whether a program can be started.
fn terminal_command_from(
    kiorg_terminal: Option<&str>,
    terminal: Option<&str>,
    available: impl Fn(&str) -> bool,
) -> Option<Vec<String>> {
    let explicit = split_words(kiorg_terminal);
    if !explicit.is_empty() {
        return Some(explicit);
    }
    let from_env = split_words(terminal);
    if from_env.len() > 1 {
        return Some(from_env);
    }
    from_env
        .first()
        .map(String::as_str)
        .into_iter()
        .chain(FALLBACK_TERMINALS.iter().copied())
        .find(|program| available(program))
        .map(|program| {
            let mut command = vec![program.to_owned()];
            command.extend(exec_flag(program).iter().map(|s| (*s).to_owned()));
            command
        })
}

fn terminal_command() -> Option<Vec<String>> {
    terminal_command_from(
        std::env::var("KIORG_TERMINAL").ok().as_deref(),
        std::env::var("TERMINAL").ok().as_deref(),
        is_in_path,
    )
}

/// `<terminal...> <editor...> <file>`, started in `working_dir`.
fn build_command(
    terminal: &[String],
    editor: &[String],
    file: &Path,
    working_dir: &Path,
) -> Command {
    let mut command = Command::new(&terminal[0]);
    command
        .args(&terminal[1..])
        .args(editor)
        .arg(file)
        .current_dir(working_dir)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    // own process group: the editor window must not die together with Kiorg
    #[cfg(unix)]
    std::os::unix::process::CommandExt::process_group(&mut command, 0);
    command
}

/// Open `file` in the editor, in a new terminal window. Returns immediately.
pub fn open_in_editor(file: &Path) -> Result<(), String> {
    let terminal = terminal_command().ok_or_else(|| {
        "No terminal emulator found. Set $KIORG_TERMINAL, e.g. \"kitty\" or \"alacritty -e\""
            .to_owned()
    })?;
    let working_dir = file.parent().unwrap_or_else(|| Path::new("."));
    let mut child = build_command(&terminal, &editor_command(), file, working_dir)
        .spawn()
        .map_err(|e| format!("Failed to start {}: {e}", terminal[0]))?;
    // reap the child so it does not stay a zombie
    std::thread::spawn(move || {
        let _ = child.wait();
    });
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(items: &[&str]) -> Vec<String> {
        items.iter().map(|i| (*i).to_owned()).collect()
    }

    #[test]
    fn editor_defaults_and_arguments() {
        assert_eq!(editor_command_from(None), s(&["nvim"]));
        assert_eq!(editor_command_from(Some("  ")), s(&["nvim"]));
        assert_eq!(editor_command_from(Some("nvim -R")), s(&["nvim", "-R"]));
    }

    #[test]
    fn exec_flags() {
        assert!(exec_flag("kitty").is_empty());
        assert!(exec_flag("/usr/bin/foot").is_empty());
        assert_eq!(exec_flag("wezterm"), ["start", "--"]);
        assert_eq!(exec_flag("gnome-terminal"), ["--"]);
        assert_eq!(exec_flag("alacritty"), ["-e"]);
        assert_eq!(exec_flag("some-unknown-term"), ["-e"]);
    }

    #[test]
    fn kiorg_terminal_is_used_verbatim() {
        let got = terminal_command_from(Some("alacritty --class edit -e"), Some("kitty"), |_| true);
        assert_eq!(got, Some(s(&["alacritty", "--class", "edit", "-e"])));
    }

    #[test]
    fn terminal_env_single_word_gets_flag() {
        assert_eq!(
            terminal_command_from(None, Some("alacritty"), |_| true),
            Some(s(&["alacritty", "-e"]))
        );
        assert_eq!(
            terminal_command_from(None, Some("kitty"), |_| true),
            Some(s(&["kitty"]))
        );
        // several words: user knows best
        assert_eq!(
            terminal_command_from(None, Some("foot -T x"), |_| false),
            Some(s(&["foot", "-T", "x"]))
        );
    }

    #[test]
    fn falls_back_to_first_available() {
        // $TERMINAL points to something that is not installed
        let got = terminal_command_from(None, Some("missing"), |p| p == "foot" || p == "xterm");
        assert_eq!(got, Some(s(&["foot"])));
        assert_eq!(
            terminal_command_from(None, None, |p| p == "xterm"),
            Some(s(&["xterm", "-e"]))
        );
        assert_eq!(terminal_command_from(None, None, |_| false), None);
    }

    #[test]
    fn command_line_order() {
        let cmd = build_command(
            &s(&["alacritty", "-e"]),
            &s(&["nvim", "-R"]),
            Path::new("/tmp/dir/a.txt"),
            Path::new("/tmp/dir"),
        );
        assert_eq!(cmd.get_program(), "alacritty");
        let args: Vec<_> = cmd.get_args().collect();
        assert_eq!(args, ["-e", "nvim", "-R", "/tmp/dir/a.txt"]);
        assert_eq!(cmd.get_current_dir(), Some(Path::new("/tmp/dir")));
    }
}
