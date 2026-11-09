//! mome (모메) - a text editor for Windows 11 that bends Notepad a little.
//!
//! Usage:
//!   mome.exe [FILE]
//!   mome.exe --restore <session id>     (used by the RunOnce entry after a Windows restart)
//!   mome.exe --revision <revision file> (opens a revision from the history in a new window)

// Never open a console window; log lines go to %LOCALAPPDATA%\mome\mome.log.
#![windows_subsystem = "windows"]

mod app;
mod dockerfile;
mod document;
mod encoding;
mod find;
mod fonts;
mod history;
mod canvas;
mod invisible;
mod json;
mod logging;
mod markdown;
mod paths;
mod placement;
mod print;
mod session;
mod settings;
mod stats;
mod theme;
mod time;
mod win;

use std::ffi::OsString;
use std::path::PathBuf;

use app::{Launch, MomeApp};
use settings::Settings;

pub const APP_NAME: &str = "mome";
pub const APP_VERSION: &str = env!("CARGO_PKG_VERSION");

/// The command line (see the top of this file).
#[derive(Debug, Default, PartialEq)]
struct Args {
    file: Option<PathBuf>,
    restore_sid: Option<String>,
    revision: Option<PathBuf>,
}

fn parse_args(args: impl Iterator<Item = OsString>) -> Args {
    let mut parsed = Args::default();
    let mut args = args.peekable();
    while let Some(arg) = args.next() {
        match arg.to_str() {
            Some("--restore") => parsed.restore_sid = args.next().map(|s| s.to_string_lossy().into_owned()),
            Some("--revision") => parsed.revision = args.next().map(PathBuf::from),
            _ if parsed.file.is_none() => parsed.file = Some(PathBuf::from(arg)),
            _ => {}
        }
    }
    parsed
}

fn main() -> eframe::Result {
    logging::init();
    let args = parse_args(std::env::args_os().skip(1));
    log::info!("starting mome {APP_VERSION}");

    let settings = Settings::load();
    let restore = args.restore_sid.as_deref().and_then(|sid| {
        let record = session::load_record(sid);
        if record.is_none() {
            log::warn!("no session record for {sid}; starting normally");
        }
        record
    });
    // A relative path from the command line is relative to the caller's directory.
    let file = args.file.map(|p| std::path::absolute(&p).unwrap_or(p));

    // Applied with Win32 once the window exists (see `placement::restore`); egui
    // positions are in points, whose size depends on the monitor.
    let placement = restore
        .as_ref()
        .and_then(|r| r.placement)
        .or(settings.placement)
        .filter(placement::is_usable);

    let mut viewport = egui::ViewportBuilder::default()
        .with_title(APP_NAME)
        .with_app_id("mome")
        .with_inner_size([900.0, 640.0])
        .with_min_inner_size([360.0, 220.0])
        .with_drag_and_drop(true);
    if let Some(icon) = load_window_icon() {
        viewport = viewport.with_icon(icon);
    }

    let options = eframe::NativeOptions {
        viewport,
        centered: placement.is_none(),
        ..Default::default()
    };

    let launch = Launch {
        file,
        restore,
        revision: args.revision,
        placement,
    };
    eframe::run_native(
        APP_NAME,
        options,
        Box::new(move |cc| Ok(Box::new(MomeApp::new(cc, settings, launch)))),
    )
}

/// The icon shown in the title bar and the task bar (embedded at compile time).
fn load_window_icon() -> Option<egui::IconData> {
    let bytes = include_bytes!("../assets/icon.png");
    let image = image::load_from_memory(bytes).ok()?.into_rgba8();
    let (width, height) = image.dimensions();
    Some(egui::IconData {
        rgba: image.into_raw(),
        width,
        height,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_arguments() {
        let parse = |args: &[&str]| parse_args(args.iter().map(OsString::from));
        assert_eq!(
            parse(&["C:\\a.txt"]),
            Args {
                file: Some(PathBuf::from("C:\\a.txt")),
                ..Args::default()
            }
        );
        assert_eq!(
            parse(&["--restore", "20261006-1"]),
            Args {
                restore_sid: Some("20261006-1".to_owned()),
                ..Args::default()
            }
        );
        assert_eq!(
            parse(&["--revision", "C:\\t\\r.txt"]),
            Args {
                revision: Some(PathBuf::from("C:\\t\\r.txt")),
                ..Args::default()
            }
        );
    }
}
