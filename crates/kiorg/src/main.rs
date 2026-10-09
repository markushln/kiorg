#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]
use clap::{CommandFactory, FromArgMatches, Parser};
use eframe::egui;
use std::fs;
use std::path::PathBuf;
use tracing_subscriber::{EnvFilter, fmt};

use kiorg::app::Kiorg;
use kiorg::picker::{PickerConfig, PickerMode, PickerResult};

#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
struct Args {
    /// Directory to open (default: use saved state or current directory)
    directory: Option<PathBuf>,

    /// Override the configuration directory
    #[arg(short, long, env = "KIORG_CONFIG_DIR")]
    config_dir: Option<PathBuf>,

    /// Clear the preview cache before starting
    #[arg(long)]
    clear_cache: bool,

    /// Print the cache and config directory, then exit
    #[arg(long)]
    print_dirs: bool,

    /// Run as a file chooser: print the selection to stdout (one absolute path per line).
    /// Exit code: 0 = selection made, 1 = cancelled, 2 = error
    #[arg(long)]
    picker: bool,

    /// (picker) Choose a directory instead of a file
    #[arg(long = "directory", requires = "picker", conflicts_with = "save")]
    pick_directory: bool,

    /// (picker) Choose a file name to save to
    #[arg(long, requires = "picker")]
    save: bool,

    /// (picker) Allow selecting multiple entries (mark with Space)
    #[arg(long, requires = "picker")]
    multiple: bool,

    /// (picker) Window title
    #[arg(long, requires = "picker")]
    title: Option<String>,

    /// (picker) Label of the accept button
    #[arg(long, requires = "picker")]
    accept_label: Option<String>,

    /// (picker) Directory to start in
    #[arg(long, requires = "picker")]
    current_folder: Option<PathBuf>,

    /// (picker) Suggested file name for --save
    #[arg(long, requires = "picker")]
    current_name: Option<String>,
}

fn init_tracing() {
    // Get log level from environment variable or use "info" as default
    let env_filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| {
        EnvFilter::new("info,font=error,pdf_render=error,eframe=error,winit=error,wgpu_hal=error")
    });

    // In picker mode stdout carries the selection, so logs must go to stderr.
    // (Checked on the raw args because tracing is set up before argument parsing.)
    let picker_mode = std::env::args_os().any(|a| a == "--picker");
    if picker_mode {
        fmt::fmt()
            .with_env_filter(env_filter)
            .with_target(true)
            .with_writer(std::io::stderr)
            .init();
    } else {
        // Initialize the tracing subscriber
        fmt::fmt()
            .with_env_filter(env_filter)
            .with_target(true)
            .init();
    }
}

fn main() -> Result<(), eframe::Error> {
    init_tracing();
    image_extras::register();
    kiorg::ui::terminal::init();

    let mut cmd = Args::command();
    let config_dir = kiorg::config::get_kiorg_config_dir(None);
    let cache_dir = kiorg::utils::preview_cache::get_cache_dir().unwrap_or_default();
    let help_extra = format!(
        "\nDirectories:\n  Config: {}\n  Cache:  {}",
        config_dir.display(),
        cache_dir.display()
    );
    cmd = cmd.after_help(help_extra);

    let matches = cmd.get_matches();
    let args = Args::from_arg_matches(&matches).unwrap_or_else(|e| e.exit());

    if args.print_dirs {
        let config_dir = kiorg::config::get_kiorg_config_dir(args.config_dir.as_deref());
        let cache_dir = kiorg::utils::preview_cache::get_cache_dir().unwrap_or_default();
        println!("Config: {}", config_dir.display());
        println!("Cache:  {}", cache_dir.display());
        return Ok(());
    }

    if args.clear_cache {
        kiorg::utils::preview_cache::purge_cache_dir();
    }

    let picker_mode = args.picker;
    let picker_config = picker_mode.then(|| {
        let mode = if args.save {
            PickerMode::Save
        } else if args.pick_directory {
            PickerMode::OpenDirectory
        } else {
            PickerMode::OpenFile
        };
        PickerConfig {
            mode,
            multiple: args.multiple && mode != PickerMode::Save,
            title: args.title.clone(),
            accept_label: args.accept_label.clone(),
            current_name: args.current_name.clone(),
        }
    });
    let picker_result: PickerResult = PickerResult::default();

    // If a directory is provided, validate and canonicalize it
    let initial_dir = if picker_mode {
        // A file chooser must not pop up an error dialog for a stale folder hint
        // from the calling application: fall back to the parent or $HOME instead.
        Some(kiorg::picker::initial_dir(
            args.current_folder
                .as_deref()
                .or(args.directory.as_deref()),
        ))
    } else if let Some(dir) = args.directory {
        // Validate the provided directory
        if !dir.exists() {
            return kiorg::startup_error::StartupErrorApp::show_error_dialog(
                format!("Directory '{}' does not exist", dir.display()),
                "Filesystem Error".to_string(),
                Some(format!("Requested directory: {}", dir.display())),
            );
        }

        if !dir.is_dir() {
            return kiorg::startup_error::StartupErrorApp::show_error_dialog(
                format!("'{}' is not a directory", dir.display()),
                "Filesystem Error".to_string(),
                Some(format!("Path provided: {}", dir.display())),
            );
        }

        // Canonicalize the path to get absolute path
        let canonical_dir = match fs::canonicalize(&dir) {
            Ok(path) => path,
            Err(e) => {
                return kiorg::startup_error::StartupErrorApp::show_error_dialog(
                    format!("Failed to canonicalize path '{}': {}", dir.display(), e),
                    "Permission Error".to_string(),
                    Some(format!("Path provided: {}", dir.display())),
                );
            }
        };

        Some(canonical_dir)
    } else {
        // No directory provided, use None to load from saved state
        None
    };

    // Load the app icon from embedded data
    let icon_data = kiorg::utils::icon::load_app_icon();

    let mut viewport = egui::ViewportBuilder::default()
        .with_inner_size([1280.0, 800.0])
        .with_min_inner_size([800.0, 600.0])
        .with_icon(icon_data)
        // separate app id so window managers can float/size the chooser independently
        .with_app_id(if picker_mode { "kiorg-picker" } else { "kiorg" });
    if let Some(cfg) = &picker_config {
        viewport = viewport.with_title(cfg.window_title());
    }

    let options = eframe::NativeOptions {
        renderer: eframe::Renderer::Wgpu,
        viewport,
        ..Default::default()
    };

    let picker_result_for_app = picker_result.clone();
    let run_result = eframe::run_native(
        "Kiorg",
        options,
        Box::new(|cc| {
            egui_extras::install_image_loaders(&cc.egui_ctx);
            // Kiorg manages its own theme system, so we disable system theme following
            // by enforcing Dark theme preference (defaulting to dark base).
            cc.egui_ctx
                .options_mut(|o| o.theme_preference = egui::ThemePreference::Dark);

            // Configure fonts for proper emoji and system font rendering
            kiorg::font::configure_egui_fonts(&cc.egui_ctx);

            match Kiorg::new(cc, initial_dir, args.config_dir) {
                Ok(mut app) => {
                    if let Some(cfg) = picker_config {
                        kiorg::picker::enable(&mut app, cfg, picker_result_for_app);
                    }
                    Ok(Box::new(app))
                }
                Err(e) => {
                    // Show the error in a startup error dialog instead of exiting
                    // Reset viewport size for error dialog
                    cc.egui_ctx
                        .send_viewport_cmd(egui::ViewportCommand::InnerSize(egui::Vec2::new(
                            600.0, 400.0,
                        )));
                    cc.egui_ctx
                        .send_viewport_cmd(egui::ViewportCommand::MinInnerSize(egui::Vec2::new(
                            300.0, 250.0,
                        )));

                    // Check if it's a config error to include config path in additional info
                    let additional_info = match &e {
                        kiorg::app::KiorgError::ConfigError(config_error) => Some(format!(
                            "Config file: {}",
                            config_error.config_path().display()
                        )),
                        _ => None,
                    };

                    Ok(kiorg::startup_error::StartupErrorApp::create_error_app(
                        cc,
                        e.to_string(),
                        "Application Initialization Error".to_string(),
                        additional_info,
                    ))
                }
            }
        }),
    );

    if picker_mode {
        return finish_picker(run_result, &picker_result);
    }
    run_result
}

/// Report the picker outcome through stdout / the exit code and terminate the process.
fn finish_picker(
    run_result: Result<(), eframe::Error>,
    result: &PickerResult,
) -> Result<(), eframe::Error> {
    use kiorg::picker::{EXIT_CANCELLED, EXIT_ERROR, write_selection};

    if let Err(e) = run_result {
        eprintln!("kiorg picker: {e}");
        std::process::exit(EXIT_ERROR);
    }

    let selection = result.lock().ok().and_then(|mut slot| slot.take());
    match selection {
        Some(paths) if !paths.is_empty() => {
            if let Err(e) = write_selection(&mut std::io::stdout().lock(), &paths) {
                eprintln!("kiorg picker: cannot write selection: {e}");
                std::process::exit(EXIT_ERROR);
            }
            Ok(()) // exit code 0
        }
        _ => std::process::exit(EXIT_CANCELLED),
    }
}
