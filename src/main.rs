//! mome (모메) - a text editor for Windows 11 that bends Notepad a little.
//!
//! Usage:
//!   mome.exe [FILE]
//!   mome.exe --restore <session id>     (used by the RunOnce entry after a Windows restart)
//!   mome.exe --revision <revision file> (opens a revision from the history in a new window)
//!   mome.exe --placement <l,t,r,b,max>  (새로 만들기로 띄운 창이 뜰 자리: 띄운 창에서 조금 비켜난 곳)

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
mod update;
mod win;

use std::ffi::OsString;
use std::path::PathBuf;

use app::{Launch, MomeApp};
use settings::{Settings, WindowPlacement};

pub const APP_NAME: &str = "mome";
pub const APP_VERSION: &str = env!("CARGO_PKG_VERSION");

/// The command line (see the top of this file).
#[derive(Debug, Default, PartialEq)]
struct Args {
    file: Option<PathBuf>,
    restore_sid: Option<String>,
    revision: Option<PathBuf>,
    placement: Option<WindowPlacement>,
}

fn parse_args(args: impl Iterator<Item = OsString>) -> Args {
    let mut parsed = Args::default();
    let mut args = args.peekable();
    while let Some(arg) = args.next() {
        match arg.to_str() {
            Some("--restore") => parsed.restore_sid = args.next().map(|s| s.to_string_lossy().into_owned()),
            Some("--revision") => parsed.revision = args.next().map(PathBuf::from),
            Some("--placement") => parsed.placement = args.next().and_then(|s| placement::from_arg(&s.to_string_lossy())),
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
    // 업데이트 뒤 "다시 시작"으로 떴는지(표시는 여기서 지운다), 그리고 지난 업데이트가 남긴 옛 파일 치우기.
    let updated_from = update::take_restart_mark();
    update::clean_leftovers();

    let settings = Settings::load();
    let restore = args.restore_sid.as_deref().and_then(|sid| {
        let record = session::load_record(sid);
        if record.is_none() {
            log::warn!("no session record for {sid}; starting normally");
        }
        record
    });
    if updated_from.is_some() && !restart_can_take_over(restore.as_ref()) {
        // 업데이트 뒤 "다시 시작"으로 떴는데 넘겨받을 문서를 읽을 수 없다. 창을 띄우지 않고 끝낸다:
        // 앞선 창은 이 프로세스가 끝난 것을 보고 그대로 남아, 저장하지 않은 글을 계속 가지고 있는다.
        log::error!("cannot take over the document for the restart; leaving it with the previous window");
        log::logger().flush();
        std::process::exit(RESTART_FAILED);
    }
    // A relative path from the command line is relative to the caller's directory.
    let file = args.file.map(|p| std::path::absolute(&p).unwrap_or(p));

    // Applied with Win32 once the window exists (see `placement::restore`); egui
    // positions are in points, whose size depends on the monitor.
    let placement = restore
        .as_ref()
        .and_then(|r| r.placement)
        .or(args.placement)
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
        updated_from,
    };
    eframe::run_native(
        APP_NAME,
        options,
        Box::new(move |cc| Ok(Box::new(MomeApp::new(cc, settings, launch)))),
    )
}

/// "다시 시작"으로 뜬 프로세스가 문서를 넘겨받지 못했을 때의 종료 코드.
const RESTART_FAILED: i32 = 3;

/// 업데이트 뒤 "다시 시작"으로 뜬 이 프로세스가 앞선 창의 문서를 넘겨받을 수 있는지:
/// 세션 기록이 있고, 저장하지 않은 글이 있다면 그 글을 읽을 수 있다.
fn restart_can_take_over(record: Option<&session::SessionRecord>) -> bool {
    match record {
        None => false,
        Some(record) if !record.modified => true,
        Some(record) => record.revision.as_deref().is_some_and(|path| history::read_revision(path).is_ok()),
    }
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
    fn 다시_시작은_글을_읽을_수_있을_때만_넘겨받는다() {
        use session::SessionRecord;
        assert!(!restart_can_take_over(None));
        // 고치지 않은 문서는 파일에서 다시 읽는다.
        assert!(restart_can_take_over(Some(&SessionRecord::default())));
        // 저장하지 않은 글이 있는데 그 글이 없거나 읽을 수 없으면 넘겨받지 않는다.
        let lost = SessionRecord { modified: true, ..Default::default() };
        assert!(!restart_can_take_over(Some(&lost)));
        let missing = SessionRecord {
            modified: true,
            revision: Some(std::env::temp_dir().join("mome-test-없는-글.txt")),
            ..Default::default()
        };
        assert!(!restart_can_take_over(Some(&missing)));
    }

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
        // 음수로 시작하는 자리도 값으로 읽는다(파일 이름으로 읽지 않는다).
        assert_eq!(
            parse(&["--placement", "-11,-11,900,700,1", "C:\\a.txt"]),
            Args {
                file: Some(PathBuf::from("C:\\a.txt")),
                placement: Some(WindowPlacement { left: -11, top: -11, right: 900, bottom: 700, maximized: true }),
                ..Args::default()
            }
        );
        assert_eq!(parse(&["--placement", "엉뚱한 값"]), Args::default());
        assert_eq!(
            parse(&["--revision", "C:\\t\\r.txt"]),
            Args {
                revision: Some(PathBuf::from("C:\\t\\r.txt")),
                ..Args::default()
            }
        );
    }
}
