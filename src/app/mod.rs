//! The mome application: state, frame loop and document operations.

mod commands;
mod dialogs;
mod editor;
mod findbar;
mod invisible_bar;
mod menu;
mod recovery;
mod status;
mod viewer;

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::mpsc::Receiver;
use std::time::{Duration, Instant};

use egui::{Align2, CornerRadius, FontFamily, FontId, Stroke, StrokeKind, ViewportCommand};
use windows_sys::Win32::Foundation::HWND;

use crate::document::Document;
use crate::encoding::Encoding;
use crate::fonts::{self, FontStore, SystemFont};
use crate::history::{self, HistoryWriter, Job, RevisionKind, SnapshotJob, WriterEvent};
use crate::print::PrintState;
use crate::session::{Session, SessionRecord};
use crate::settings::{Settings, WindowPlacement};
use crate::stats::TextStats;
use crate::theme::{self, Palette};
use crate::placement;
use crate::time::LocalTime;
use crate::win;
use crate::APP_NAME;

pub use commands::Cmd;
use dialogs::{Dialog, SettingsForm};
use editor::Editor;
use findbar::FindBar;
use invisible_bar::InvisibleState;
use recovery::doc_from_revision;
use viewer::{ViewKind, Viewer};

/// How the process was started.
#[derive(Default)]
pub struct Launch {
    /// File given on the command line.
    pub file: Option<PathBuf>,
    /// Session being restored after Windows restarted.
    pub restore: Option<SessionRecord>,
    /// A revision opened from the history of another window.
    pub revision: Option<PathBuf>,
    /// Where the window was last time (or before Windows restarted).
    pub placement: Option<WindowPlacement>,
}

/// What to do once the user has dealt with unsaved changes.
#[derive(Clone, Debug, PartialEq)]
pub enum After {
    New,
    Open(Option<PathBuf>),
    Reopen(Encoding),
    Exit,
}

struct StatusMessage {
    text: String,
    warning: bool,
    until: Instant,
}

#[derive(Default)]
struct SnapshotState {
    /// First unsnapshotted change.
    dirty_since: Option<Instant>,
    /// Hash of the text in the latest revision of this document.
    last_hash: Option<u64>,
    last_time: Option<LocalTime>,
}

const CLEANUP_EVERY: Duration = Duration::from_secs(60 * 60);
const MESSAGE_TIME: Duration = Duration::from_secs(6);
/// Documents up to this size are copied for the shutdown hook on every change;
/// larger ones at most every [`SESSION_COPY_INTERVAL`].
const SESSION_COPY_EAGER_BYTES: usize = 256 * 1024;
const SESSION_COPY_INTERVAL: Duration = Duration::from_millis(500);

pub struct MomeApp {
    settings: Settings,
    doc: Document,
    stats: TextStats,
    editor: Editor,
    find: FindBar,
    /// 보이지 않는 문자 찾기 (the bar, and the markers in the text).
    invisible: InvisibleState,
    /// 읽기 모드 (Markdown and canvas reader).
    viewer: Viewer,
    dialog: Option<Dialog>,
    /// Dialogs waiting for the open one to close.
    dialog_queue: VecDeque<Dialog>,
    /// Commands from menus, shortcuts and buttons, run at the end of the frame.
    pending_cmds: Vec<Cmd>,
    /// `None` once `on_exit` has waited for it to finish.
    history: Option<HistoryWriter>,
    history_rx: Receiver<WriterEvent>,
    snapshot: SnapshotState,
    session: Session,
    hwnd: HWND,
    print_state: PrintState,
    fonts: FontStore,
    /// The font definitions must be rebuilt (font chosen or previewed).
    fonts_dirty: bool,
    /// Font being tried in the font dialog's preview.
    preview_font: Option<SystemFont>,
    /// Status bar message and when it disappears.
    message: Option<StatusMessage>,
    last_title: String,
    /// Let the window close without asking about unsaved changes.
    allow_close: bool,
    /// The window had focus last frame (to notice when it comes back).
    was_focused: bool,
    last_cleanup: Option<Instant>,
    /// Where the window is, saved on exit and for the shutdown hook.
    window_placement: Option<WindowPlacement>,
    /// Where the window was restored to, until checked once it is shown.
    placement_to_check: Option<WindowPlacement>,
    /// Increased on every text change; keys the find bar's match cache.
    text_version: u64,
    /// The shutdown hook's copy of the text is behind (copies are throttled for big documents).
    session_text_stale: bool,
    session_text_at: Instant,
}

impl MomeApp {
    pub fn new(cc: &eframe::CreationContext<'_>, settings: Settings, launch: Launch) -> Self {
        let ctx = &cc.egui_ctx;
        // Ctrl+plus/minus zoom the text (see commands.rs), not the whole UI.
        ctx.options_mut(|o| o.zoom_with_keyboard = false);
        theme::install(ctx);
        theme::apply(ctx, settings.dark_mode);

        // Images in Markdown and canvases; ours goes last so it is asked first.
        egui_extras::install_image_loaders(ctx);

        let mut fonts = FontStore::default();
        ctx.set_fonts(fonts.definitions(&settings.font, None));

        let hwnd = win::hwnd_of(cc).unwrap_or(std::ptr::null_mut());
        placement::install_dpi_fix(hwnd);
        let placed = launch.placement.as_ref().and_then(|p| placement::restore(hwnd, p));
        let mut session = launch
            .restore
            .as_ref()
            .and_then(|record| Session::resume(&record.sid))
            .unwrap_or_else(Session::new);
        session.install_shutdown_hook(hwnd);

        let repaint_ctx = ctx.clone();
        let (history, history_rx) = HistoryWriter::spawn(move || repaint_ctx.request_repaint());

        let mut app = Self {
            window_placement: placed.or(launch.placement),
            placement_to_check: placed,
            settings,
            doc: Document::new_untitled(),
            stats: TextStats::default(),
            editor: Editor::new(),
            find: FindBar::default(),
            invisible: InvisibleState::default(),
            viewer: Viewer::default(),
            dialog: None,
            dialog_queue: VecDeque::new(),
            pending_cmds: Vec::new(),
            history: Some(history),
            history_rx,
            snapshot: SnapshotState::default(),
            session,
            hwnd,
            print_state: PrintState::default(),
            fonts,
            fonts_dirty: false,
            preview_font: None,
            message: None,
            last_title: String::new(),
            allow_close: false,
            was_focused: true,
            last_cleanup: None,
            text_version: 0,
            session_text_stale: false,
            session_text_at: Instant::now(),
        };
        app.find.options = app.settings.find.clone();
        // Our loader goes last so it is asked first for `file:` pictures.
        ctx.add_bytes_loader(app.viewer.loader.clone());
        app.start(ctx, launch);
        app
    }

    /// Opens the initial document and queues first-run / recovery dialogs.
    fn start(&mut self, ctx: &egui::Context, launch: Launch) {
        let restoring = launch.restore.is_some();
        if let Some(record) = launch.restore {
            log::info!("restoring session {}", record.sid);
            self.restore_record(ctx, &record);
            // The record now belongs to this window (same id); it is rewritten below.
        } else if let Some(path) = launch.revision {
            match doc_from_revision(&path) {
                Ok(doc) => self.set_document(ctx, doc, 0),
                Err(err) => self.show_error("임시 저장 버전을 열 수 없습니다", &format!("{}\n{err}", path.display())),
            }
        } else if let Some(path) = launch.file {
            self.open_path(ctx, &path, None);
        } else {
            self.set_document(ctx, Document::new_untitled(), 0);
        }

        if self.settings.temp_dir.is_none() {
            self.open_dialog(Dialog::Settings(SettingsForm::first_run()));
        }
        if !restoring {
            self.offer_recovery();
        }
        self.session.write_record();
        #[cfg(debug_assertions)]
        self.start_tests();
    }

    /// Debug builds only: lets the UI be tested without synthesizing keyboard or
    /// mouse input on the user's desktop.
    #[cfg(debug_assertions)]
    fn start_tests(&mut self) {
        // MOME_TEST_CMDS=History,Font runs those commands at start-up.
        if let Ok(script) = std::env::var("MOME_TEST_CMDS") {
            for name in script.split(',').map(str::trim) {
                let cmd = match name {
                    "History" => Cmd::History,
                    "Settings" => Cmd::Settings,
                    "Font" => Cmd::Font,
                    "Find" => Cmd::Find,
                    "Replace" => Cmd::Replace,
                    "FindNext" => Cmd::FindNext,
                    "GoTo" => Cmd::GoTo,
                    "Shortcuts" => Cmd::Shortcuts,
                    "About" => Cmd::About,
                    "ToggleDark" => Cmd::ToggleDark,
                    "FormatJson" => Cmd::FormatJson,
                    "FindInvisible" => Cmd::FindInvisible,
                    "ToggleReader" => Cmd::ToggleReader,
                    "ZoomIn" => Cmd::ZoomIn,
                    "ZoomOut" => Cmd::ZoomOut,
                    _ => continue,
                };
                self.pending_cmds.push(cmd);
            }
        }
        // MOME_TEST_FIND=query|||replacement|||flags fills in the find bar.
        if let Ok(spec) = std::env::var("MOME_TEST_FIND") {
            self.test_find(&spec);
        }
        // MOME_TEST_DPI=96,144 pretends the window is dragged onto monitors with those
        // DPIs; the results go to the log.
        if let Ok(dpis) = std::env::var("MOME_TEST_DPI") {
            let dpis = dpis.split(',').filter_map(|d| d.trim().parse().ok()).collect();
            placement::simulate_drags(self.hwnd, dpis);
        }
    }

    fn palette(&self) -> Palette {
        theme::palette(self.settings.dark_mode)
    }

    // ----- documents -------------------------------------------------------------

    /// Makes `doc` the open document.
    fn set_document(&mut self, ctx: &egui::Context, doc: Document, cursor: usize) {
        self.doc = doc;
        // Canvases open in the reader; Markdown keeps the mode of the last note.
        let reading = match ViewKind::of(self.doc.path.as_deref()) {
            Some(ViewKind::Canvas) => true,
            Some(ViewKind::Markdown) => self.viewer.on,
            None => false,
        };
        self.viewer.reset();
        self.viewer.on = reading;
        if reading {
            // The find bar works on the hidden text; it must not stay over the reader.
            self.find.close();
        }
        self.stats = TextStats::compute(&self.doc.text);
        self.text_version += 1;
        self.snapshot = SnapshotState::default();
        if self.doc.is_modified() {
            // Restored text that is not on disk yet: snapshot it again soon.
            self.snapshot.dirty_since = Some(Instant::now());
        }
        let cursor = cursor.min(self.stats.total_chars());
        self.editor.reset(ctx, cursor, self.doc.text.len());
        self.find.invalidate();
        let text = self.doc.text.clone();
        let hash = self.doc.current_hash;
        self.session.update(|s| {
            s.text = text;
            s.text_hash = hash;
            s.revision_hash = None;
            s.record.revision = None;
        });
        self.session_text_stale = false;
        self.sync_session(ctx);
        self.session.write_record();
    }

    /// Called whenever the editor text changed.
    fn on_text_changed(&mut self) {
        self.doc.text_changed();
        self.stats = TextStats::compute(&self.doc.text);
        self.text_version += 1;
        self.snapshot.dirty_since.get_or_insert_with(Instant::now);
        self.session_text_stale = true;
        if self.doc.text.len() <= SESSION_COPY_EAGER_BYTES {
            self.copy_text_to_session();
        }
    }

    /// Gives the shutdown hook the current text.
    fn copy_text_to_session(&mut self) {
        self.session.update(|s| {
            s.text.clear();
            s.text.push_str(&self.doc.text);
            s.text_hash = self.doc.current_hash;
        });
        self.session_text_stale = false;
        self.session_text_at = Instant::now();
    }

    /// Big documents: copy the text for the shutdown hook at most twice a second.
    fn tick_session_text(&mut self, ctx: &egui::Context) {
        if !self.session_text_stale {
            return;
        }
        let since = self.session_text_at.elapsed();
        if since >= SESSION_COPY_INTERVAL {
            self.copy_text_to_session();
        } else {
            ctx.request_repaint_after(SESSION_COPY_INTERVAL - since);
        }
    }

    /// Changes settings in this window and in the shared settings file. `change` runs
    /// twice (on our copy and on the file's), so callers clone what they move in.
    fn change_settings(&mut self, change: impl Fn(&mut Settings)) {
        change(&mut self.settings);
        Settings::update_on_disk(change);
    }

    /// Opens a file into this window. Returns false if it could not be opened.
    fn open_path(&mut self, ctx: &egui::Context, path: &Path, encoding: Option<Encoding>) -> bool {
        match Document::load(path, encoding) {
            Ok((doc, report)) => {
                self.set_document(ctx, doc, 0);
                self.change_settings(|s| s.push_recent(path));
                if report.lossy {
                    self.notify_warning(format!(
                        "일부 글자를 {} 인코딩으로 읽지 못해 '�'로 표시했습니다. [인코딩 > 다른 인코딩으로 다시 열기]를 사용해 보세요.",
                        self.doc.encoding.label()
                    ));
                } else if report.had_nul {
                    self.notify_warning("파일의 NUL 문자를 '�'로 바꿨습니다. 저장하면 NUL 문자는 사라집니다.".to_owned());
                } else if report.large {
                    self.notify_warning("파일이 커서 편집이 느릴 수 있습니다.".to_owned());
                } else {
                    self.warn_invisible();
                }
                true
            }
            Err(err) if err.kind() == std::io::ErrorKind::NotFound && encoding.is_none() => {
                // Like Notepad: `mome.exe new.txt` starts a new file with that name.
                self.set_document(ctx, Document::new_at(path), 0);
                self.notify("새 파일입니다. 저장하면 파일이 만들어집니다.".to_owned());
                true
            }
            Err(err) => {
                self.show_error("파일을 열 수 없습니다", &format!("{}\n\n{err}", path.display()));
                self.change_settings(|s| s.recent_files.retain(|p| p != path));
                false
            }
        }
    }

    fn pick_open_path(&mut self, frame: &eframe::Frame) -> Option<PathBuf> {
        let mut dialog = rfd::FileDialog::new()
            .set_title("열기")
            .add_filter("텍스트 파일 (*.txt, *.md, *.json, *.canvas)", &["txt", "md", "json", "canvas"])
            .add_filter("텍스트 문서 (*.txt)", &["txt"])
            .add_filter("Markdown (*.md)", &["md"])
            .add_filter("JSON (*.json)", &["json"])
            .add_filter("옵시디언 캔버스 (*.canvas)", &["canvas"])
            .add_filter("모든 파일 (*.*)", &["*"])
            .set_parent(frame);
        if let Some(dir) = self.doc.path.as_deref().and_then(Path::parent) {
            dialog = dialog.set_directory(dir);
        }
        dialog.pick_file()
    }

    /// Saves the document, asking for a name if it has none, then does `after`.
    /// Returns true on success.
    fn save(&mut self, ctx: &egui::Context, frame: &eframe::Frame, after: Option<After>) -> bool {
        let path = match self.doc.path.clone() {
            Some(path) => path,
            None => match self.pick_save_path(frame, "저장") {
                Some(path) => path,
                None => return false,
            },
        };
        self.save_to(ctx, &path, after)
    }

    fn save_as(&mut self, ctx: &egui::Context, frame: &eframe::Frame) -> bool {
        match self.pick_save_path(frame, "다른 이름으로 저장") {
            Some(path) => self.save_to(ctx, &path, None),
            None => false,
        }
    }

    fn pick_save_path(&self, frame: &eframe::Frame, title: &str) -> Option<PathBuf> {
        const TYPES: [(&str, &str); 4] = [
            ("텍스트 문서 (*.txt)", "txt"),
            ("Markdown (*.md)", "md"),
            ("JSON (*.json)", "json"),
            ("옵시디언 캔버스 (*.canvas)", "canvas"),
        ];
        // The current file's type comes first, so the dialog does not offer to add ".txt".
        let ext = self.doc.path.as_deref().and_then(Path::extension).map(|e| e.to_string_lossy().to_lowercase());
        let current = TYPES.iter().position(|(_, e)| Some(*e) == ext.as_deref());
        let mut dialog = rfd::FileDialog::new().set_title(title);
        match current {
            Some(i) => dialog = dialog.add_filter(TYPES[i].0, &[TYPES[i].1]),
            // An unusual extension such as .log: keep it by showing all files first.
            None if ext.is_some() => dialog = dialog.add_filter("모든 파일 (*.*)", &["*"]),
            None => {}
        }
        for (i, (name, e)) in TYPES.iter().enumerate() {
            if Some(i) != current {
                dialog = dialog.add_filter(*name, &[*e]);
            }
        }
        if current.is_some() || ext.is_none() {
            dialog = dialog.add_filter("모든 파일 (*.*)", &["*"]);
        }
        dialog = dialog
            .set_file_name(match &self.doc.path {
                Some(_) => self.doc.title(),
                None => "제목 없음.txt".to_owned(),
            })
            .set_parent(frame);
        if let Some(dir) = self.doc.path.as_deref().and_then(Path::parent) {
            dialog = dialog.set_directory(dir);
        }
        dialog.save_file()
    }

    fn save_to(&mut self, ctx: &egui::Context, path: &Path, after: Option<After>) -> bool {
        if (self.doc.lossy || self.doc.unread) && self.doc.path.as_deref() == Some(path) {
            // Saving would replace bytes we could not read with '�', or a file we
            // could not read at all.
            self.open_dialog(Dialog::LossySave {
                path: path.to_path_buf(),
                after,
            });
            return false;
        }
        let bytes = match self.doc.encode() {
            Ok(bytes) => bytes,
            Err(unrepresentable) => {
                self.open_dialog(Dialog::EncodingFallback {
                    sample: unrepresentable.sample,
                    path: path.to_path_buf(),
                    after,
                });
                return false;
            }
        };
        let old_key = self.doc.doc_key.clone();
        let was_untitled = self.doc.path.is_none();
        if let Err(err) = self.doc.write_to(path, &bytes) {
            self.show_error(
                "저장할 수 없습니다",
                &format!("{}\n\n{err}\n\n[다른 이름으로 저장]으로 다른 위치에 저장해 보세요.", path.display()),
            );
            return false;
        }
        let new_key = history::key_for_path(path);
        if new_key != old_key {
            // A new document's history moves with it to its first file name. A file saved
            // under another name starts a new history; the old file keeps its own.
            if was_untitled {
                if let (Some(root), Some(writer)) = (self.settings.temp_dir.clone(), &self.history) {
                    writer.send(Job::Adopt {
                        root,
                        from_key: old_key,
                        to_key: new_key.clone(),
                        info: self.doc.doc_info(),
                    });
                }
            } else {
                self.snapshot.last_hash = None;
            }
            self.doc.doc_key = new_key;
        }
        self.take_snapshot(RevisionKind::Save);
        self.snapshot.dirty_since = None;
        self.change_settings(|s| s.push_recent(path));
        self.session.update(|s| {
            s.record.revision = None;
            s.revision_hash = None;
        });
        self.sync_session(ctx);
        self.session.write_record();
        self.notify(format!("{}에 저장했습니다.", self.doc.title()));
        if let Some(after) = after {
            self.continue_after(ctx, None, after);
        }
        true
    }

    /// Asks about unsaved changes before `after`, or does it right away.
    fn confirm_then(&mut self, ctx: &egui::Context, frame: &eframe::Frame, after: After) {
        if self.doc.is_modified() {
            self.open_dialog(Dialog::SavePrompt { after });
        } else {
            self.continue_after(ctx, Some(frame), after);
        }
    }

    /// The unsaved changes are handled; carry on.
    fn continue_after(&mut self, ctx: &egui::Context, frame: Option<&eframe::Frame>, after: After) {
        match after {
            After::New => self.set_document(ctx, Document::new_untitled(), 0),
            After::Open(Some(path)) => {
                self.open_path(ctx, &path, None);
            }
            After::Open(None) => match frame {
                Some(frame) => {
                    if let Some(path) = self.pick_open_path(frame) {
                        self.open_path(ctx, &path, None);
                    }
                }
                // No frame here (we came from a dialog): ask again next frame.
                None => self.pending_cmds.push(Cmd::OpenUnchecked),
            },
            After::Reopen(encoding) => {
                if let Some(path) = self.doc.path.clone() {
                    self.message = None;
                    let opened = self.open_path(ctx, &path, Some(encoding));
                    // Keep a decoding warning visible rather than covering it up.
                    if opened && !self.message.as_ref().is_some_and(|m| m.warning) {
                        self.notify(format!("{} 인코딩으로 다시 열었습니다.", encoding.label()));
                    }
                }
            }
            After::Exit => {
                self.allow_close = true;
                ctx.send_viewport_cmd(ViewportCommand::Close);
            }
        }
    }

    /// "저장 안 함": keep the text in the history anyway, then continue.
    fn discard_then(&mut self, ctx: &egui::Context, after: After) {
        if !self.snapshot_sync(RevisionKind::Close) {
            // The dialog promised the text stays in the history; it would not.
            self.show_error(
                "임시 저장하지 못했습니다",
                "저장하지 않은 내용을 임시 저장 폴더에 남기지 못해서 닫지 않았습니다. \
                 임시 저장 폴더(USB·네트워크 드라이브라면 연결 상태)를 확인하거나 [다른 이름으로 저장]으로 저장해 주세요.",
            );
            return;
        }
        self.continue_after(ctx, None, after);
    }

    // ----- revisions ----------------------------------------------------------------

    fn snapshot_job(&self, kind: RevisionKind) -> Option<SnapshotJob> {
        Some(SnapshotJob {
            root: self.settings.temp_dir.clone()?,
            key: self.doc.doc_key.clone(),
            info: self.doc.doc_info(),
            kind,
            time: LocalTime::now(),
            text: self.doc.text.clone(),
        })
    }

    /// Queues a revision. Returns false if there was nothing new to store.
    fn take_snapshot(&mut self, kind: RevisionKind) -> bool {
        let hash = self.doc.current_hash;
        let unchanged = self.snapshot.last_hash == Some(hash);
        let skip = match kind {
            RevisionKind::Auto => unchanged || !self.doc.is_modified(),
            _ => unchanged,
        };
        if skip {
            return false;
        }
        let (Some(job), Some(writer)) = (self.snapshot_job(kind), &self.history) else {
            return false;
        };
        writer.send(Job::Snapshot(job));
        self.snapshot.last_hash = Some(hash);
        true
    }

    /// Writes a revision right away (when the text is about to go away). Returns
    /// false if unsaved text should have been kept but could not be written.
    fn snapshot_sync(&mut self, kind: RevisionKind) -> bool {
        if !self.doc.is_modified() || self.snapshot.last_hash == Some(self.doc.current_hash) {
            return true;
        }
        let Some(job) = self.snapshot_job(kind) else {
            return true; // No temporary save folder: nothing was promised.
        };
        match history::write_snapshot(&job) {
            Ok(_) => {
                self.snapshot.last_hash = Some(self.doc.current_hash);
                true
            }
            Err(err) => {
                log::error!("could not keep unsaved text: {err}");
                false
            }
        }
    }

    fn tick_snapshots(&mut self, ctx: &egui::Context) {
        let interval = Duration::from_secs(self.settings.snapshot_interval_secs);
        let Some(since) = self.snapshot.dirty_since else {
            return;
        };
        let elapsed = since.elapsed();
        if elapsed < interval {
            ctx.request_repaint_after(interval - elapsed);
            return;
        }
        self.snapshot.dirty_since = None;
        self.take_snapshot(RevisionKind::Auto);
        if self.settings.autosave_original && self.doc.path.is_some() && self.doc.is_modified() {
            self.autosave_original(ctx);
        }
    }

    /// Writes a named document back to its file without any dialogs.
    fn autosave_original(&mut self, ctx: &egui::Context) {
        let Some(path) = self.doc.path.clone() else {
            return;
        };
        if self.doc.lossy {
            self.notify_warning(
                "제대로 읽지 못한 글자가 있는 파일이라 원본 파일 자동 저장을 건너뛰었습니다.".to_owned(),
            );
            return;
        }
        if self.doc.autosave_blocked || self.doc.unread {
            // Restored or old text, or a file we could not read: only a save the
            // user makes may replace the file.
            return;
        }
        if self.doc.changed_on_disk() {
            self.notify_warning(
                "다른 프로그램에서 파일이 변경되어 원본 파일 자동 저장을 건너뛰었습니다.".to_owned(),
            );
            return;
        }
        match self.doc.encode() {
            Ok(bytes) => match self.doc.write_to(&path, &bytes) {
                Ok(()) => {
                    self.sync_session(ctx);
                    self.session.write_record();
                }
                Err(err) => self.notify_warning(format!("원본 파일에 자동 저장하지 못했습니다: {err}")),
            },
            Err(_) => self.notify_warning(format!(
                "{} 인코딩으로 저장할 수 없는 문자가 있어 원본 파일 자동 저장을 건너뛰었습니다.",
                self.doc.encoding.label()
            )),
        }
    }

    fn poll_history(&mut self) {
        while let Ok(event) = self.history_rx.try_recv() {
            match event {
                WriterEvent::Written {
                    path,
                    time,
                    kind,
                    hash,
                    key,
                } => {
                    if key != self.doc.doc_key {
                        continue;
                    }
                    self.snapshot.last_time = Some(time);
                    if kind != RevisionKind::Save {
                        self.session.update(|s| {
                            s.record.revision = Some(path);
                            s.revision_hash = Some(hash);
                        });
                        self.session.write_record();
                    }
                }
                WriterEvent::Failed { error } => {
                    self.notify_warning(format!("임시 저장하지 못했습니다: {error}"));
                    // The text is not in the history after all: try again later, and
                    // do not count it as kept when the window closes.
                    self.snapshot.last_hash = None;
                    self.snapshot.dirty_since.get_or_insert_with(Instant::now);
                }
            }
        }
    }

    fn tick_cleanup(&mut self) {
        let due = self.last_cleanup.is_none_or(|t| t.elapsed() >= CLEANUP_EVERY);
        if !due {
            return;
        }
        if let (Some(root), Some(writer)) = (self.settings.temp_dir.clone(), &self.history) {
            writer.send(Job::Cleanup {
                root,
                retention_days: self.settings.retention_days,
            });
            self.last_cleanup = Some(Instant::now());
        }
    }

    // ----- session -----------------------------------------------------------------

    /// Mirrors the cheap parts of the state into the shutdown hook's copy.
    fn sync_session(&self, ctx: &egui::Context) {
        let cursor = self.editor.cursor(ctx);
        let doc = &self.doc;
        self.session.update(|s| {
            let r = &mut s.record;
            r.doc_key.clone_from(&doc.doc_key);
            r.path.clone_from(&doc.path);
            r.title = doc.title();
            r.encoding = doc.encoding;
            r.file_encoding = doc.path.as_ref().map(|_| doc.saved_encoding);
            r.line_ending = doc.line_ending;
            r.modified = doc.is_modified();
            r.cursor = cursor;
            r.zoom = self.editor.zoom;
            r.overwrite = self.editor.overwrite;
            r.placement = self.window_placement;
            s.history_root.clone_from(&self.settings.temp_dir);
            s.doc_info = doc.doc_info();
        });
    }

    /// Remembers where the window is (also while it is minimized or maximized).
    fn track_window(&mut self) {
        if let Some(now) = placement::current(self.hwnd) {
            self.window_placement = Some(now);
        }
    }

    /// See [`placement::check_after_show`].
    fn check_restored_placement(&mut self) {
        if let Some(placed) = self.placement_to_check {
            if placement::is_visible(self.hwnd) {
                self.placement_to_check = None;
                placement::check_after_show(self.hwnd, &placed);
            }
        }
    }

    // ----- feedback ------------------------------------------------------------------

    fn notify(&mut self, text: String) {
        self.message = Some(StatusMessage {
            text,
            warning: false,
            until: Instant::now() + MESSAGE_TIME,
        });
    }

    fn notify_warning(&mut self, text: String) {
        log::warn!("{text}");
        self.message = Some(StatusMessage {
            text,
            warning: true,
            until: Instant::now() + MESSAGE_TIME * 2,
        });
    }

    /// Clears the status bar message when its time is up.
    fn tick_message(&mut self, ctx: &egui::Context) {
        if let Some(message) = &self.message {
            let left = message.until.saturating_duration_since(Instant::now());
            if left.is_zero() {
                self.message = None;
            } else {
                ctx.request_repaint_after(left);
            }
        }
    }

    fn show_error(&mut self, title: &str, body: &str) {
        log::error!("{title}: {body}");
        self.open_dialog(Dialog::Message {
            title: title.to_owned(),
            body: body.to_owned(),
        });
    }

    fn open_dialog(&mut self, dialog: Dialog) {
        if self.dialog.is_some() {
            self.dialog_queue.push_back(dialog);
        } else {
            self.dialog = Some(dialog);
        }
    }

    fn update_title(&mut self, ctx: &egui::Context) {
        let title = format!(
            "{}{} - {APP_NAME}",
            if self.doc.is_modified() { "*" } else { "" },
            self.doc.title()
        );
        if title != self.last_title {
            ctx.send_viewport_cmd(ViewportCommand::Title(title.clone()));
            self.last_title = title;
        }
    }

    // ----- frame -----------------------------------------------------------------------

    /// Runs in `logic`, which eframe also calls while the window is minimized
    /// (closing from the taskbar), when `ui` is not called at all.
    fn handle_close_request(&mut self, ctx: &egui::Context) {
        if !ctx.input(|i| i.viewport().close_requested()) || self.allow_close {
            return;
        }
        if self.doc.is_modified() {
            ctx.send_viewport_cmd(ViewportCommand::CancelClose);
            if !matches!(self.dialog, Some(Dialog::SavePrompt { .. })) {
                self.dialog_queue.clear();
                self.dialog = Some(Dialog::SavePrompt { after: After::Exit });
                // The question must be visible to be answered.
                ctx.send_viewport_cmd(ViewportCommand::Minimized(false));
                ctx.send_viewport_cmd(ViewportCommand::Focus);
            }
        }
    }

    fn handle_dropped_files(&mut self, ctx: &egui::Context) {
        let dropped = ctx.input(|i| i.raw.dropped_files.first().map(|f| f.path().to_path_buf()));
        if let Some(path) = dropped {
            if path.is_file() && self.dialog.is_none() {
                self.pending_cmds.push(Cmd::OpenPath(path));
            }
        }
    }

    fn check_external_change(&mut self, ctx: &egui::Context) {
        let focused = ctx.input(|i| i.focused);
        if focused && !self.was_focused && self.dialog.is_none() && self.doc.changed_on_disk() {
            self.open_dialog(Dialog::Reload);
        }
        self.was_focused = focused;
    }

    fn paint_drop_hint(&self, ctx: &egui::Context) {
        if ctx.input(|i| i.raw.hovered_files.is_empty()) {
            return;
        }
        let p = self.palette();
        let rect = ctx.content_rect().shrink(8.0);
        let painter = ctx.layer_painter(egui::LayerId::new(egui::Order::Foreground, egui::Id::new("drop")));
        painter.rect_filled(rect, CornerRadius::same(10), p.editor_bg.gamma_multiply(0.85));
        painter.rect_stroke(rect, CornerRadius::same(10), Stroke::new(3.0, p.accent), StrokeKind::Inside);
        painter.text(
            rect.center(),
            Align2::CENTER_CENTER,
            "놓으면 이 파일을 엽니다",
            FontId::new(22.0, FontFamily::Proportional),
            p.accent,
        );
    }

    fn rebuild_fonts(&mut self, ctx: &egui::Context) {
        if self.fonts_dirty {
            self.fonts_dirty = false;
            ctx.set_fonts(self.fonts.definitions(&self.settings.font, self.preview_font.as_ref()));
        }
    }

    fn editor_font(&self) -> FontId {
        FontId::new(
            (self.settings.font.size * self.editor.zoom as f32 / 100.0).max(4.0),
            FontFamily::Name(fonts::EDITOR_FAMILY.into()),
        )
    }

    /// The text area, filling the space between the bars.
    fn editor_panel(&mut self, ui: &mut egui::Ui) {
        let p = self.palette();
        egui::CentralPanel::default()
            .frame(egui::Frame::NONE.fill(p.editor_bg))
            .show(ui, |ui| {
                // Searching every edit is only needed to show the matches; F3 with the
                // bar closed searches when pressed.
                let find_open = self.find.open;
                if find_open {
                    self.find.refresh(&self.doc.text, self.text_version);
                    self.find.update_preview(&self.doc.text);
                }
                let opts = editor::ShowOptions {
                    font: self.editor_font(),
                    wrap: self.settings.word_wrap,
                    text_color: p.editor_fg,
                    highlights: if find_open { &self.find.matches } else { &[] },
                    current: self.find.current,
                    match_bg: p.match_bg,
                    current_bg: p.current_match_bg,
                    // Exclusions only mean something while replacing.
                    excluded: if find_open && self.find.replace_mode { &self.find.excluded } else { &[] },
                    hint: self
                        .find
                        .preview
                        .as_ref()
                        .filter(|_| find_open)
                        .map(|(at, s)| (*at, s.as_str())),
                    hint_bg: p.bar_bg,
                    hint_border: p.accent,
                    marks: if self.settings.show_invisible { &self.invisible.marks } else { &[] },
                    mark_color: p.invisible,
                };
                let output = self.editor.show(ui, &mut self.doc.text, &opts);
                if output.changed {
                    self.on_text_changed();
                    // The markers were placed for the text before this edit.
                    ui.ctx().request_repaint();
                }
                self.context_menu(&output.response);
            });
    }

    fn run_pending_cmds(&mut self, ctx: &egui::Context, frame: &mut eframe::Frame) {
        let commands = std::mem::take(&mut self.pending_cmds);
        for cmd in commands {
            // Clicking a menu or button takes keyboard focus from the text; give it
            // back unless the command moves focus somewhere else on purpose.
            let keeps_focus_elsewhere = matches!(cmd, Cmd::Find | Cmd::Replace) || self.find_has_focus(ctx);
            self.run(ctx, frame, cmd);
            if self.dialog.is_none() && !keeps_focus_elsewhere {
                self.editor.request_focus();
            }
        }
    }

    fn save_settings_on_exit(&mut self) {
        self.track_window();
        let (placement, find) = (self.window_placement, self.find.options.clone());
        self.change_settings(|s| {
            s.placement = placement;
            s.find = find.clone();
        });
    }
}

impl eframe::App for MomeApp {
    fn clear_color(&self, _visuals: &egui::Visuals) -> [f32; 4] {
        self.palette().editor_bg.to_normalized_gamma_f32()
    }

    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.poll_history();
        self.handle_close_request(ctx);
        // Minimized or hidden windows get logic-only passes whose input is replayed
        // later; only time-based work is safe then.
        let visible = ctx.input(|i| i.viewport().visible()) != Some(false);
        self.check_restored_placement();
        self.track_window();
        if visible {
            self.handle_dropped_files(ctx);
            self.check_external_change(ctx);
            if self.dialog.is_none() {
                self.handle_shortcuts(ctx);
                if self.reading().is_none() && self.editor.before_frame(ctx, &mut self.doc.text) {
                    self.on_text_changed();
                }
            }
        }
        self.tick_snapshots(ctx);
        self.tick_cleanup();
        self.tick_session_text(ctx);
        self.sync_session(ctx);
        self.tick_message(ctx);
    }

    fn ui(&mut self, ui: &mut egui::Ui, frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        self.rebuild_fonts(&ctx);
        self.refresh_invisible();

        self.menu_bar(ui);
        if self.find.open && self.reading().is_none() {
            self.find_bar(ui);
        }
        if self.invisible.open && self.reading().is_none() {
            self.invisible_bar(ui);
        }
        if self.settings.show_status_bar {
            self.status_bar(ui);
        }
        match self.reading() {
            Some(kind) => self.reader_panel(ui, kind),
            None => self.editor_panel(ui),
        }
        self.dialogs(&ctx, frame);
        self.paint_drop_hint(&ctx);

        self.run_pending_cmds(&ctx, frame);
        self.update_title(&ctx);
    }

    fn on_exit(&mut self) {
        log::info!("closing normally");
        // Safety net: whatever is on screen is kept in the history even if the
        // window went away without the usual save prompt.
        self.snapshot_sync(RevisionKind::Close);
        self.save_settings_on_exit();
        self.session.end_normally();
        // Let queued revisions reach the disk before the process ends.
        if let Some(writer) = self.history.take() {
            writer.finish();
        }
    }
}
