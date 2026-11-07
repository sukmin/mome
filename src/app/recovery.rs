//! Bringing documents back: after a Windows restart (`--restore`), after a crash
//! (orphaned session records), and from a revision opened in a new window.

use std::path::Path;

use egui::Context;

use super::dialogs::Dialog;
use super::{editor, MomeApp};
use crate::document::Document;
use crate::history;
use crate::session::{self, SessionRecord};
use crate::win;

impl MomeApp {
    /// Offers to recover documents of windows that crashed or lost power.
    pub(super) fn offer_recovery(&mut self) {
        let records = session::recoverable_orphans(self.session.sid());
        if !records.is_empty() {
            self.open_dialog(Dialog::Recover { records });
        }
    }

    /// Brings back a window from a session record (Windows restart or crash recovery).
    pub(super) fn restore_record(&mut self, ctx: &Context, record: &SessionRecord) {
        let (doc, warning) = doc_from_record(record);
        self.set_document(ctx, doc, record.cursor);
        if record.zoom > 0 {
            self.editor.zoom = record.zoom.clamp(editor::ZOOM_MIN, editor::ZOOM_MAX);
        }
        self.editor.overwrite = record.overwrite;
        if let Some(warning) = warning {
            self.notify_warning(warning);
        }
    }

    /// "복구" in the recovery dialog: the first document goes into this window if it is
    /// still empty, the others into new windows.
    pub(super) fn recover(&mut self, ctx: &Context, records: Vec<SessionRecord>) {
        let pristine = self.doc.path.is_none() && !self.doc.is_modified() && self.doc.text.is_empty();
        let mut records = records.into_iter();
        if pristine {
            if let Some(first) = records.next() {
                // Read first: without a temporary save folder the text lives next to
                // the record and goes with it.
                self.restore_record(ctx, &first);
                session::delete_record(&first.sid);
                if !self.message.as_ref().is_some_and(|m| m.warning) {
                    self.notify("저장하지 않은 문서를 복구했습니다.".to_owned());
                }
            }
        }
        for record in records {
            win::spawn_self(&["--restore".as_ref(), record.sid.as_ref()]);
        }
    }
}

/// Rebuilds a document from a session record. Returns a warning if the unsaved
/// text could not be found.
pub(super) fn doc_from_record(record: &SessionRecord) -> (Document, Option<String>) {
    let (mut doc, mut warning) = match &record.path {
        // Read the file the same way it was read before, so an encoding chosen with
        // "다른 인코딩으로 다시 열기" is kept.
        Some(path) => Document::load_or_new(path, record.file_encoding),
        None => (Document::new_untitled(), None),
    };
    if !record.doc_key.is_empty() {
        doc.doc_key = record.doc_key.clone();
    }
    doc.encoding = record.encoding;
    doc.line_ending = record.line_ending;
    if record.modified {
        match record.revision.as_deref().map(history::read_revision) {
            Some(Ok(text)) => {
                doc.text = text;
                doc.text_changed();
                // The file may have changed while the PC was off; do not overwrite it
                // behind the user's back.
                doc.autosave_blocked = true;
            }
            _ => {
                log::error!("unsaved text of session {} is missing", record.sid);
                warning = Some(
                    "저장하지 않은 내용을 찾을 수 없어 마지막으로 저장된 파일을 열었습니다. [파일 > 임시 저장 기록]을 확인해 보세요."
                        .to_owned(),
                );
            }
        }
    }
    (doc, warning)
}

/// Opens a revision as the (modified) contents of the document it belongs to.
pub(super) fn doc_from_revision(path: &Path) -> std::io::Result<Document> {
    let text = history::read_revision(path)?;
    let dir = path.parent().unwrap_or(Path::new("."));
    let info = history::read_doc_info(dir);
    let mut doc = match info.as_ref().and_then(|i| i.path.as_ref()) {
        Some(original) => Document::load_or_new(original, None).0,
        None => {
            // Never saved: keep adding revisions to the same folder.
            let mut doc = Document::new_untitled();
            if let Some(key) = dir.file_name() {
                doc.doc_key = key.to_string_lossy().into_owned();
            }
            doc
        }
    };
    if let Some(info) = info {
        doc.encoding = info.encoding;
        doc.line_ending = info.line_ending;
    }
    doc.text = text;
    doc.text_changed();
    // An old version: it reaches the original file only when the user saves it.
    doc.autosave_blocked = true;
    Ok(doc)
}
