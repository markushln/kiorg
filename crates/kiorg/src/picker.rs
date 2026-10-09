//! File-chooser ("picker") mode.
//!
//! Started with `kiorg --picker ...` (see `main.rs`). Used by
//! `xdg-desktop-portal-kiorg`, but usable from any script:
//!
//! * the selection is printed to stdout, one absolute path per line
//! * exit code 0 = selection made, 1 = cancelled, 2 = error
//! * everything else (logs) goes to stderr
//!
//! Keys in picker mode (on top of the normal navigation keys):
//!
//! | Key                | Open file          | Select folder           | Save                       |
//! |--------------------|--------------------|-------------------------|----------------------------|
//! | `Enter` / `o`      | open file / enter dir | pick highlighted dir | enter dir / copy file name |
//! | `Ctrl+Enter`       | -                  | pick current dir        | save with current name     |
//! | `Space`            | mark (if `--multiple`) | mark dirs (`--multiple`) | -                      |
//! | `Tab`              | -                  | -                       | edit file name             |
//! | `Esc` / `q`        | cancel             | cancel                  | cancel                     |
//!
//! Destructive actions (delete, rename, cut/paste, terminal, undo/redo, open-with,
//! drag & drop, file drops) are disabled.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use egui::{Color32, Context, Shadow};

use crate::app::Kiorg;
use crate::config::shortcuts::ShortcutAction;

pub const EXIT_SELECTED: i32 = 0;
pub const EXIT_CANCELLED: i32 = 1;
pub const EXIT_ERROR: i32 = 2;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum PickerMode {
    #[default]
    OpenFile,
    OpenDirectory,
    Save,
}

#[derive(Debug, Clone, Default)]
pub struct PickerConfig {
    pub mode: PickerMode,
    /// Allow marking several entries (ignored in `Save` mode).
    pub multiple: bool,
    pub title: Option<String>,
    pub accept_label: Option<String>,
    /// Suggested file name (`Save` mode).
    pub current_name: Option<String>,
}

impl PickerConfig {
    #[must_use]
    pub fn window_title(&self) -> String {
        match self.title.as_deref().filter(|t| !t.trim().is_empty()) {
            Some(t) => t.to_owned(),
            None => match self.mode {
                PickerMode::OpenFile => "Open File",
                PickerMode::OpenDirectory => "Select Folder",
                PickerMode::Save => "Save File",
            }
            .to_owned(),
        }
    }

    #[must_use]
    pub fn accept_text(&self) -> String {
        match self.accept_label.as_deref().filter(|t| !t.trim().is_empty()) {
            // GTK-style mnemonics ("_Open") are common in portal requests
            Some(t) => t.replace('_', ""),
            None => match self.mode {
                PickerMode::OpenFile => "Open",
                PickerMode::OpenDirectory => "Select",
                PickerMode::Save => "Save",
            }
            .to_owned(),
        }
    }
}

/// Shared slot through which the selection leaves the GUI event loop.
/// `None` after the app exited means "cancelled".
pub type PickerResult = Arc<Mutex<Option<Vec<PathBuf>>>>;

pub struct PickerState {
    pub config: PickerConfig,
    pub result: PickerResult,
    /// File name being edited (`Save` mode).
    pub name: String,
    /// Ask the name field to grab focus on the next draw.
    pub name_focus: bool,
    /// The name field currently has keyboard focus; normal key handling is suspended.
    pub editing_name: bool,
    /// Path the user was already warned about overwriting.
    pub overwrite_pending: Option<PathBuf>,
}

impl PickerState {
    #[must_use]
    pub fn new(config: PickerConfig, result: PickerResult) -> Self {
        let save = config.mode == PickerMode::Save;
        let name = config
            .current_name
            .as_deref()
            .and_then(sanitize_name)
            .unwrap_or_default();
        Self {
            config,
            result,
            name,
            name_focus: save,
            editing_name: save,
            overwrite_pending: None,
        }
    }
}

/// Switch an existing app instance into picker mode.
pub fn enable(app: &mut Kiorg, config: PickerConfig, result: PickerResult) {
    app.picker = Some(PickerState::new(config, result));
}

// ---------------------------------------------------------------------------------------------
// Pure helpers (unit-tested below)
// ---------------------------------------------------------------------------------------------

/// Reduce a suggested name to its last path component; `None` if nothing usable is left.
#[must_use]
pub fn sanitize_name(name: &str) -> Option<String> {
    let n = Path::new(name).file_name()?.to_str()?;
    validate_name(n).ok()
}

pub fn validate_name(name: &str) -> Result<String, String> {
    let trimmed = name.trim();
    if trimmed.is_empty() {
        return Err("Enter a file name".to_owned());
    }
    if trimmed == "." || trimmed == ".." {
        return Err(format!("'{trimmed}' is not a valid file name"));
    }
    if trimmed.contains(['/', '\\', '\0', '\n']) {
        return Err("File name must not contain / \\ or line breaks".to_owned());
    }
    Ok(trimmed.to_owned())
}

/// Turn the UI state into the final list of paths (or a user-facing error message).
pub fn resolve_selection(
    config: &PickerConfig,
    current_dir: &Path,
    highlighted: Option<&Path>,
    marked: &[PathBuf],
    name: &str,
) -> Result<Vec<PathBuf>, String> {
    let use_marked = config.multiple && !marked.is_empty();

    let mut paths: Vec<PathBuf> = match config.mode {
        PickerMode::Save => vec![current_dir.join(validate_name(name)?)],
        PickerMode::OpenFile => {
            let candidates: Vec<PathBuf> = if use_marked {
                marked.to_vec()
            } else {
                highlighted.map(Path::to_path_buf).into_iter().collect()
            };
            if candidates.is_empty() {
                return Err("Nothing selected".to_owned());
            }
            if let Some(dir) = candidates.iter().find(|p| p.is_dir()) {
                return Err(format!("'{}' is a directory", dir.display()));
            }
            candidates
        }
        PickerMode::OpenDirectory => {
            if use_marked {
                if let Some(f) = marked.iter().find(|p| !p.is_dir()) {
                    return Err(format!("'{}' is not a directory", f.display()));
                }
                marked.to_vec()
            } else {
                match highlighted {
                    Some(p) if p.is_dir() => vec![p.to_path_buf()],
                    _ => vec![current_dir.to_path_buf()],
                }
            }
        }
    };

    paths.sort();
    paths.dedup();

    // The stdout protocol is line based.
    if let Some(p) = paths.iter().find(|p| p.to_string_lossy().contains('\n')) {
        return Err(format!(
            "Path contains a line break and cannot be returned: {}",
            p.display().to_string().escape_debug()
        ));
    }
    Ok(paths)
}

/// Directory the picker starts in: the requested dir, else its parent, else `$HOME`, else ".".
#[must_use]
pub fn initial_dir(requested: Option<&Path>) -> PathBuf {
    let usable = |p: &Path| p.is_dir().then(|| std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf()));
    requested
        .and_then(usable)
        .or_else(|| requested.and_then(Path::parent).and_then(usable))
        .or_else(dirs::home_dir)
        .unwrap_or_else(|| PathBuf::from("."))
}

/// Write the selection to `w`: one path per line.
pub fn write_selection<W: Write>(w: &mut W, paths: &[PathBuf]) -> std::io::Result<()> {
    for p in paths {
        #[cfg(unix)]
        {
            use std::os::unix::ffi::OsStrExt;
            w.write_all(p.as_os_str().as_bytes())?;
        }
        #[cfg(not(unix))]
        w.write_all(p.to_string_lossy().as_bytes())?;
        w.write_all(b"\n")?;
    }
    w.flush()
}

/// What picker mode does with a shortcut action.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Disposition {
    /// Picker handles it: Enter / open
    Activate,
    /// Picker handles it: quit without selection
    Cancel,
    /// Disabled in picker mode
    Block,
    /// Normal handling
    Pass,
}

#[must_use]
pub fn intercept(action: &ShortcutAction) -> Disposition {
    match action {
        ShortcutAction::OpenDirectoryOrFile => Disposition::Activate,
        ShortcutAction::Exit => Disposition::Cancel,
        ShortcutAction::DeleteEntry
        | ShortcutAction::RenameEntry
        | ShortcutAction::CutEntry
        | ShortcutAction::PasteEntry
        | ShortcutAction::OpenTerminal
        | ShortcutAction::OpenWithCommand
        | ShortcutAction::Undo
        | ShortcutAction::Redo
        | ShortcutAction::ShowActionHistory => Disposition::Block,
        _ => Disposition::Pass,
    }
}

// ---------------------------------------------------------------------------------------------
// Behaviour wired into the app
// ---------------------------------------------------------------------------------------------

/// Leave the app with `paths` as the result.
fn finish(app: &mut Kiorg, paths: Vec<PathBuf>) {
    if let Some(state) = &app.picker {
        if let Ok(mut slot) = state.result.lock() {
            *slot = Some(paths);
        }
    }
    app.shutdown_requested = true;
}

/// Leave the app without a result.
pub fn cancel(app: &mut Kiorg) {
    app.shutdown_requested = true;
}

fn collect_marked(app: &mut Kiorg) -> Vec<PathBuf> {
    let tab = app.tab_manager.current_tab_mut();
    if tab.is_range_selection_active() {
        tab.apply_range_selection_to_marked();
        tab.range_selection_start = None;
    }
    tab.marked_entries.iter().cloned().collect()
}

/// Confirm the selection. `current_dir_only`: ignore highlight and marks (Ctrl+Enter).
pub fn confirm(app: &mut Kiorg, highlighted: Option<PathBuf>, current_dir_only: bool) {
    let Some(state) = &app.picker else { return };
    let config = state.config.clone();
    let name = state.name.clone();

    let marked = if config.multiple && !current_dir_only && config.mode != PickerMode::Save {
        collect_marked(app)
    } else {
        Vec::new()
    };
    let tab = app.tab_manager.current_tab_ref();
    let current_dir = tab.current_path.clone();
    let highlighted = if current_dir_only {
        None
    } else {
        highlighted.or_else(|| tab.selected_entry().map(|e| e.meta.path.clone()))
    };

    match resolve_selection(&config, &current_dir, highlighted.as_deref(), &marked, &name) {
        Ok(paths) if config.mode == PickerMode::Save => confirm_save(app, paths),
        Ok(paths) => finish(app, paths),
        Err(msg) => app.notify_error(msg),
    }
}

/// Save mode: ask once before overwriting an existing file.
fn confirm_save(app: &mut Kiorg, mut paths: Vec<PathBuf>) {
    let Some(path) = paths.pop() else { return };
    if path.is_dir() {
        app.notify_error(format!("'{}' is a directory", path.display()));
        return;
    }
    let already_warned = app
        .picker
        .as_ref()
        .is_some_and(|s| s.overwrite_pending.as_deref() == Some(path.as_path()));
    if path.exists() && !already_warned {
        if let Some(s) = app.picker.as_mut() {
            s.overwrite_pending = Some(path.clone());
        }
        app.notify_info(format!(
            "'{}' exists - confirm again to overwrite",
            path.file_name().map(|n| n.to_string_lossy()).unwrap_or_default()
        ));
        return;
    }
    finish(app, vec![path]);
}

/// Enter / `o` / double-click. `path` overrides the highlighted entry (double-click).
pub fn activate(app: &mut Kiorg, path: Option<PathBuf>) {
    let Some(state) = &app.picker else { return };
    let config = state.config.clone();

    let highlighted = path.or_else(|| {
        app.tab_manager
            .current_tab_ref()
            .selected_entry()
            .map(|e| e.meta.path.clone())
    });

    match config.mode {
        PickerMode::OpenFile => {
            let has_marks = config.multiple && !collect_marked(app).is_empty();
            match highlighted {
                Some(p) if p.is_dir() && !has_marks => app.navigate_to_dir(p),
                other => confirm(app, other, false),
            }
        }
        // Enter picks the highlighted directory (navigate with l / right / h / left).
        PickerMode::OpenDirectory => confirm(app, highlighted, false),
        PickerMode::Save => match highlighted {
            Some(p) if p.is_dir() => app.navigate_to_dir(p),
            Some(p) if p.is_file() => {
                if let (Some(name), Some(s)) = (
                    p.file_name().map(|n| n.to_string_lossy().into_owned()),
                    app.picker.as_mut(),
                ) {
                    s.name = name;
                    s.name_focus = true;
                    s.editing_name = true;
                }
            }
            _ => {}
        },
    }
}

/// Picker-specific raw keys, called before the shortcut tree. Returns true if consumed.
pub fn handle_raw_key(app: &mut Kiorg, key: egui::Key, modifiers: egui::Modifiers) -> bool {
    let Some(state) = &app.picker else {
        return false;
    };
    let mode = state.config.mode;
    match key {
        egui::Key::Escape => {
            if app.key_buffer.is_empty() {
                cancel(app);
            } else {
                // abort a partial key sequence first, like vim
                app.key_buffer.clear();
            }
            true
        }
        egui::Key::Enter if modifiers.ctrl || modifiers.command => {
            confirm(app, None, true);
            true
        }
        egui::Key::Tab if mode == PickerMode::Save => {
            if let Some(s) = app.picker.as_mut() {
                s.name_focus = true;
                s.editing_name = true;
            }
            true
        }
        _ => false,
    }
}

fn name_input_id() -> egui::Id {
    egui::Id::new("kiorg_picker_name_input")
}

/// While the name field is focused it owns the keyboard. Returns true if input was consumed.
pub fn handle_key_press(ctx: &Context, app: &mut Kiorg) -> bool {
    let Some(state) = &app.picker else {
        return false;
    };
    if !state.editing_name {
        return false;
    }

    let (enter, escape) = ctx.input(|i| {
        (
            i.key_pressed(egui::Key::Enter),
            i.key_pressed(egui::Key::Escape),
        )
    });

    if enter {
        confirm(app, None, true);
    } else if escape {
        // Esc leaves the field; a second Esc cancels the dialog.
        ctx.memory_mut(|m| m.surrender_focus(name_input_id()));
        if let Some(s) = app.picker.as_mut() {
            s.editing_name = false;
            s.name_focus = false;
        }
    }
    true
}

/// Bottom bar with title, hints, name field (save) and Accept/Cancel buttons.
pub fn draw(ctx: &Context, app: &mut Kiorg) {
    let marked_count = app.tab_manager.current_tab_ref().marked_entries.len();
    let Some(state) = app.picker.as_mut() else {
        return;
    };
    let colors = &app.colors;

    let mode = state.config.mode;
    let multiple = state.config.multiple;
    let title = state.config.window_title();
    let accept_text = state.config.accept_text();

    let hint = match mode {
        PickerMode::OpenFile if multiple => {
            "Space: mark | Enter: open | l/h: enter/leave dir | Esc: cancel"
        }
        PickerMode::OpenFile => "Enter: open | l/h: enter/leave dir | Esc: cancel",
        PickerMode::OpenDirectory if multiple => {
            "Space: mark | Enter: pick highlighted | Ctrl+Enter: pick this dir | Esc: cancel"
        }
        PickerMode::OpenDirectory => {
            "Enter: pick highlighted | Ctrl+Enter: pick this dir | l/h: enter/leave dir | Esc: cancel"
        }
        PickerMode::Save => "Tab: edit name | Ctrl+Enter: save | Esc: cancel",
    };

    let mut accept_clicked = false;
    let mut cancel_clicked = false;

    egui::Area::new(egui::Id::new("kiorg_picker_bar"))
        .anchor(egui::Align2::CENTER_BOTTOM, egui::vec2(0.0, -10.0))
        .interactable(true)
        .movable(false)
        .show(ctx, |ui| {
            let shadow = Shadow {
                offset: [0, 4],
                blur: 12,
                spread: 0,
                color: Color32::from_black_alpha(60),
            };
            egui::Frame::default()
                .fill(colors.bg_extreme)
                .inner_margin(8.0)
                .shadow(shadow)
                .show(ui, |ui| {
                    ui.set_max_width(720.0);
                    ui.horizontal(|ui| {
                        ui.label(egui::RichText::new(&title).color(colors.highlight).strong());
                        if multiple && marked_count > 0 {
                            ui.label(
                                egui::RichText::new(format!("{marked_count} marked"))
                                    .color(colors.link_text),
                            );
                        }
                        ui.label(egui::RichText::new(hint).color(colors.fg_light));
                    });
                    ui.horizontal(|ui| {
                        if mode == PickerMode::Save {
                            let response = ui.add(
                                egui::TextEdit::singleline(&mut state.name)
                                    .id(name_input_id())
                                    .hint_text("File name")
                                    .desired_width(380.0),
                            );
                            if state.name_focus {
                                response.request_focus();
                                state.name_focus = false;
                            }
                            state.editing_name = response.has_focus();
                        }
                        if ui.button(accept_text.as_str()).clicked() {
                            accept_clicked = true;
                        }
                        if ui.button("Cancel").clicked() {
                            cancel_clicked = true;
                        }
                    });
                });
        });

    if accept_clicked {
        // Save: use the name field; open modes: highlighted entry / current dir
        confirm(app, None, false);
    } else if cancel_clicked {
        cancel(app);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn cfg(mode: PickerMode, multiple: bool) -> PickerConfig {
        PickerConfig {
            mode,
            multiple,
            ..Default::default()
        }
    }

    #[test]
    fn names() {
        assert_eq!(validate_name("  a.txt ").unwrap(), "a.txt");
        assert!(validate_name("").is_err());
        assert!(validate_name("..").is_err());
        assert!(validate_name("a/b").is_err());
        assert!(validate_name("a\nb").is_err());
        assert_eq!(sanitize_name("/tmp/dir/report.pdf").as_deref(), Some("report.pdf"));
        assert_eq!(sanitize_name("/"), None);
    }

    #[test]
    fn open_file_single_and_dir_rejected() {
        let d = tempfile::tempdir().unwrap();
        let f = d.path().join("a.txt");
        fs::write(&f, b"x").unwrap();
        let c = cfg(PickerMode::OpenFile, false);

        assert_eq!(
            resolve_selection(&c, d.path(), Some(&f), &[], "").unwrap(),
            vec![f.clone()]
        );
        assert!(resolve_selection(&c, d.path(), Some(d.path()), &[], "").is_err());
        assert!(resolve_selection(&c, d.path(), None, &[], "").is_err());
    }

    #[test]
    fn open_file_multiple_prefers_marked_and_sorts() {
        let d = tempfile::tempdir().unwrap();
        let (a, b) = (d.path().join("a"), d.path().join("b"));
        fs::write(&a, b"").unwrap();
        fs::write(&b, b"").unwrap();
        let c = cfg(PickerMode::OpenFile, true);
        let got = resolve_selection(&c, d.path(), Some(&a), &[b.clone(), a.clone()], "").unwrap();
        assert_eq!(got, vec![a.clone(), b.clone()]);

        // without `multiple`, marks are ignored
        let c = cfg(PickerMode::OpenFile, false);
        let got = resolve_selection(&c, d.path(), Some(&a), &[b], "").unwrap();
        assert_eq!(got, vec![a]);
    }

    #[test]
    fn open_directory_modes() {
        let d = tempfile::tempdir().unwrap();
        let sub = d.path().join("sub");
        let file = d.path().join("f");
        fs::create_dir(&sub).unwrap();
        fs::write(&file, b"").unwrap();
        let c = cfg(PickerMode::OpenDirectory, false);

        assert_eq!(resolve_selection(&c, d.path(), Some(&sub), &[], "").unwrap(), vec![sub.clone()]);
        // highlighted file or nothing -> current directory
        assert_eq!(
            resolve_selection(&c, d.path(), Some(&file), &[], "").unwrap(),
            vec![d.path().to_path_buf()]
        );
        assert_eq!(
            resolve_selection(&c, d.path(), None, &[], "").unwrap(),
            vec![d.path().to_path_buf()]
        );
        // marked non-directory is an error
        let c = cfg(PickerMode::OpenDirectory, true);
        assert!(resolve_selection(&c, d.path(), None, &[file], "").is_err());
    }

    #[test]
    fn save_joins_name() {
        let c = cfg(PickerMode::Save, false);
        let got = resolve_selection(&c, Path::new("/srv/x"), None, &[], "doc.txt").unwrap();
        assert_eq!(got, vec![PathBuf::from("/srv/x/doc.txt")]);
        assert!(resolve_selection(&c, Path::new("/srv/x"), None, &[], "a/b").is_err());
    }

    #[cfg(unix)]
    #[test]
    fn newline_paths_rejected() {
        let d = tempfile::tempdir().unwrap();
        let f = d.path().join("bad\nname");
        fs::write(&f, b"").unwrap();
        let c = cfg(PickerMode::OpenFile, false);
        assert!(resolve_selection(&c, d.path(), Some(&f), &[], "").is_err());
    }

    #[test]
    fn output_format() {
        let mut buf = Vec::new();
        write_selection(&mut buf, &[PathBuf::from("/a/b"), PathBuf::from("/c d")]).unwrap();
        assert_eq!(buf, b"/a/b\n/c d\n");
    }

    #[test]
    fn initial_dir_fallbacks() {
        let d = tempfile::tempdir().unwrap();
        let canon = fs::canonicalize(d.path()).unwrap();
        assert_eq!(initial_dir(Some(d.path())), canon);
        // a file path falls back to its parent directory
        let f = d.path().join("f");
        fs::write(&f, b"").unwrap();
        assert_eq!(initial_dir(Some(&f)), canon);
        // nonexistent -> something usable (home or ".")
        assert!(initial_dir(Some(Path::new("/definitely/not/here"))).is_dir());
        assert!(initial_dir(None).is_dir());
    }

    #[test]
    fn interception() {
        assert_eq!(intercept(&ShortcutAction::OpenDirectoryOrFile), Disposition::Activate);
        assert_eq!(intercept(&ShortcutAction::Exit), Disposition::Cancel);
        for a in [
            ShortcutAction::DeleteEntry,
            ShortcutAction::RenameEntry,
            ShortcutAction::CutEntry,
            ShortcutAction::PasteEntry,
            ShortcutAction::OpenTerminal,
            ShortcutAction::OpenWithCommand,
            ShortcutAction::Undo,
            ShortcutAction::Redo,
        ] {
            assert_eq!(intercept(&a), Disposition::Block, "{a:?}");
        }
        assert_eq!(intercept(&ShortcutAction::MoveDown), Disposition::Pass);
        assert_eq!(intercept(&ShortcutAction::SelectEntry), Disposition::Pass);
    }

    #[test]
    fn labels() {
        let mut c = cfg(PickerMode::OpenFile, false);
        assert_eq!(c.window_title(), "Open File");
        assert_eq!(c.accept_text(), "Open");
        c.title = Some("Attach".into());
        c.accept_label = Some("_Choose".into());
        assert_eq!(c.window_title(), "Attach");
        assert_eq!(c.accept_text(), "Choose");
    }
}
