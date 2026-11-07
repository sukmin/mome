//! The open document: its text, where it lives on disk and how it is encoded.

use std::hash::{Hash as _, Hasher as _};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use crate::encoding::{self, Encoding, LineEnding, Unrepresentable};
use crate::history;
use crate::time::LocalTime;

/// Files larger than this are refused; the editor would be unusable anyway.
const MAX_FILE_BYTES: u64 = 64 * 1024 * 1024;
/// Above this size editing gets noticeably slower; we warn once.
const LARGE_FILE_BYTES: u64 = 2 * 1024 * 1024;

/// Creates or truncates `path`, writes `bytes` and waits until they are on the disk.
pub fn write_synced(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    write_all_synced(std::fs::File::create(path)?, bytes)
}

fn write_all_synced(mut file: std::fs::File, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write as _;
    file.write_all(bytes)?;
    sync(&file)
}

/// Flushes `file` to the disk. Drives that cannot flush (some network and virtual
/// drives answer "invalid function" or "not supported") have taken the data anyway.
fn sync(file: &std::fs::File) -> std::io::Result<()> {
    const ERROR_INVALID_FUNCTION: i32 = 1;
    const ERROR_NOT_SUPPORTED: i32 = 50;
    match file.sync_all() {
        Err(e) if matches!(e.raw_os_error(), Some(ERROR_INVALID_FUNCTION | ERROR_NOT_SUPPORTED)) => Ok(()),
        other => other,
    }
}

/// Overwrites `path` without a temporary file. The space is claimed first, so a full
/// disk fails before anything is overwritten instead of leaving the file cut short.
fn write_in_place(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write as _;
    let mut file = std::fs::OpenOptions::new().write(true).create(true).truncate(false).open(path)?;
    let len = bytes.len() as u64;
    if len > file.metadata()?.len() {
        file.set_len(len)?;
    }
    file.write_all(bytes)?;
    file.set_len(len)?;
    sync(&file)
}

/// Writes `bytes` to `path` so that a crash or power cut never leaves a half-written file:
/// the data goes to a temporary file next to it, is flushed, and then replaces the
/// original with `ReplaceFileW` (which keeps its attributes and permissions).
/// Writes in place only when no file can be created next to it (e.g. a folder we may
/// write files in but not create them); a failed write (full disk) is reported
/// without touching the original.
pub fn write_file_safely(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    use windows_sys::Win32::Storage::FileSystem::{ReplaceFileW, REPLACEFILE_IGNORE_MERGE_ERRORS};

    // A symbolic link is written through, keeping the link.
    let is_link = std::fs::symlink_metadata(path).is_ok_and(|m| m.file_type().is_symlink());
    let Some(name) = path.file_name().filter(|_| !is_link) else {
        return write_in_place(path, bytes);
    };
    // The process id keeps two windows saving the same file apart.
    let side = |what: &str| path.with_file_name(format!(".{}.{}.mome-{what}", name.to_string_lossy(), std::process::id()));
    let tmp = side("saving");
    let file = match std::fs::File::create(&tmp) {
        Ok(file) => file,
        Err(err) => {
            log::warn!("cannot create {} ({err}); writing in place", tmp.display());
            return write_in_place(path, bytes);
        }
    };
    if let Err(err) = write_all_synced(file, bytes) {
        let _ = std::fs::remove_file(&tmp);
        return Err(err);
    }
    if !path.exists() {
        return std::fs::rename(&tmp, path).or_else(|_| {
            let _ = std::fs::remove_file(&tmp);
            write_in_place(path, bytes)
        });
    }
    // With a backup name, a half-done replacement leaves the original under that
    // name instead of a name we cannot know.
    let backup = side("backup");
    let target = crate::win::wide_path(path);
    let replacement = crate::win::wide_path(&tmp);
    let backup_w = crate::win::wide_path(&backup);
    let ok = unsafe {
        ReplaceFileW(
            target.as_ptr(),
            replacement.as_ptr(),
            backup_w.as_ptr(),
            REPLACEFILE_IGNORE_MERGE_ERRORS,
            std::ptr::null(),
            std::ptr::null(),
        )
    } != 0;
    if ok {
        let _ = std::fs::remove_file(&backup);
        return Ok(());
    }
    log::warn!("ReplaceFileW failed for {} ({})", path.display(), std::io::Error::last_os_error());
    if !path.exists() {
        // The original was moved aside but the new file not put in its place
        // (ERROR_UNABLE_TO_MOVE_REPLACEMENT_2): finish by hand, or put the original back.
        return match std::fs::rename(&tmp, path) {
            Ok(()) => {
                let _ = std::fs::remove_file(&backup);
                Ok(())
            }
            Err(err) => {
                if backup.exists() {
                    let _ = std::fs::rename(&backup, path);
                }
                Err(kept_in(err, &tmp))
            }
        };
    }
    // The original is untouched: write in place, keeping the new text in the
    // temporary file until that has worked.
    match write_in_place(path, bytes) {
        Ok(()) => {
            let _ = std::fs::remove_file(&tmp);
            Ok(())
        }
        Err(err) => Err(kept_in(err, &tmp)),
    }
}

/// `err`, saying that the text is safe in `tmp`.
fn kept_in(err: std::io::Error, tmp: &Path) -> std::io::Error {
    std::io::Error::new(err.kind(), format!("{err} (저장하려던 내용은 {}에 남아 있습니다)", tmp.display()))
}

pub fn hash_text(text: &str) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    text.hash(&mut hasher);
    hasher.finish()
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DiskStamp {
    pub modified: Option<SystemTime>,
    pub len: u64,
}

impl DiskStamp {
    pub fn of(path: &Path) -> Option<Self> {
        let meta = std::fs::metadata(path).ok()?;
        Some(Self {
            modified: meta.modified().ok(),
            len: meta.len(),
        })
    }
}

pub struct Document {
    /// Editor text with `\n` line breaks.
    pub text: String,
    pub path: Option<PathBuf>,
    pub encoding: Encoding,
    pub line_ending: LineEnding,
    /// Folder name of this document's revisions in the temp directory.
    pub doc_key: String,
    /// Hash of `text` as last loaded from or saved to disk.
    pub saved_hash: u64,
    pub saved_encoding: Encoding,
    pub saved_line_ending: LineEnding,
    /// Hash of the current `text`; refreshed by [`Document::text_changed`].
    pub current_hash: u64,
    /// File state when we last read or wrote it, to notice changes by other programs.
    pub disk_stamp: Option<DiskStamp>,
    /// The text is not exactly what the file holds: some bytes could not be decoded
    /// (binary file or wrong encoding) or NUL characters were replaced. Saving over the
    /// original would change those parts, so the user is asked first.
    pub lossy: bool,
    /// The file exists but could not be read (e.g. its drive was not connected yet
    /// when the window was restored), so the text is not its contents. Saving over
    /// it is confirmed first.
    pub unread: bool,
    /// The text came from a revision or a restored session, not from the file:
    /// the original is not written by "원본 파일에도 자동 저장" until the user saves
    /// once (the file may have changed meanwhile, or the text is an old version).
    pub autosave_blocked: bool,
}

/// What the user should be told after loading a file.
pub struct LoadReport {
    /// Some bytes could not be decoded and show as '�'.
    pub lossy: bool,
    /// NUL characters show as '�'.
    pub had_nul: bool,
    /// Big enough for editing to be slow.
    pub large: bool,
}

impl Document {
    pub fn new_untitled() -> Self {
        let empty_hash = hash_text("");
        Self {
            text: String::new(),
            path: None,
            encoding: Encoding::Utf8,
            line_ending: LineEnding::CrLf,
            doc_key: history::key_for_untitled(&LocalTime::now()),
            saved_hash: empty_hash,
            saved_encoding: Encoding::Utf8,
            saved_line_ending: LineEnding::CrLf,
            current_hash: empty_hash,
            disk_stamp: None,
            lossy: false,
            unread: false,
            autosave_blocked: false,
        }
    }

    /// A document for a path that does not exist yet (e.g. `mome.exe new.txt`).
    pub fn new_at(path: &Path) -> Self {
        let mut doc = Self::new_untitled();
        doc.path = Some(path.to_path_buf());
        doc.doc_key = history::key_for_path(path);
        doc
    }

    pub fn load(path: &Path, forced: Option<Encoding>) -> std::io::Result<(Self, LoadReport)> {
        let meta = std::fs::metadata(path)?;
        if meta.is_dir() {
            return Err(std::io::Error::other("폴더는 열 수 없습니다."));
        }
        if meta.len() > MAX_FILE_BYTES {
            return Err(std::io::Error::other(format!(
                "파일이 너무 큽니다 ({} MB). {} MB 이하의 파일만 열 수 있습니다.",
                meta.len() / (1024 * 1024),
                MAX_FILE_BYTES / (1024 * 1024)
            )));
        }
        let bytes = std::fs::read(path)?;
        let decoded = encoding::decode(&bytes, forced);
        let hash = hash_text(&decoded.text);
        let doc = Self {
            text: decoded.text,
            path: Some(path.to_path_buf()),
            encoding: decoded.encoding,
            line_ending: decoded.line_ending,
            doc_key: history::key_for_path(path),
            saved_hash: hash,
            saved_encoding: decoded.encoding,
            saved_line_ending: decoded.line_ending,
            current_hash: hash,
            disk_stamp: DiskStamp::of(path),
            lossy: decoded.lossy || decoded.had_nul,
            unread: false,
            autosave_blocked: false,
        };
        let report = LoadReport {
            lossy: decoded.lossy,
            had_nul: decoded.had_nul,
            large: meta.len() > LARGE_FILE_BYTES,
        };
        Ok((doc, report))
    }

    /// Loads `path`, or starts an empty document for it if the file is gone. A file
    /// that is there but cannot be read now gives an empty document marked
    /// [`Self::unread`], and a warning for the user.
    pub fn load_or_new(path: &Path, forced: Option<Encoding>) -> (Self, Option<String>) {
        match Self::load(path, forced) {
            Ok((doc, _)) => (doc, None),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => (Self::new_at(path), None),
            Err(err) => {
                log::warn!("cannot read {}: {err}", path.display());
                let mut doc = Self::new_at(path);
                doc.unread = true;
                // A stamp no real file has: once the file can be read, it counts as
                // changed on disk and the user is offered to load it.
                doc.disk_stamp = Some(DiskStamp { modified: None, len: u64::MAX });
                let warning = format!(
                    "'{}' 파일을 읽지 못했습니다({err}). 드라이브가 연결되었는지 확인한 뒤 [파일 > 열기]로 다시 여세요.",
                    path.display()
                );
                (doc, Some(warning))
            }
        }
    }

    /// Must be called whenever `text` changes.
    pub fn text_changed(&mut self) {
        self.current_hash = hash_text(&self.text);
    }

    pub fn is_modified(&self) -> bool {
        self.current_hash != self.saved_hash
            || self.encoding != self.saved_encoding
            || self.line_ending != self.saved_line_ending
    }

    /// File name, or "제목 없음" for a new document.
    pub fn title(&self) -> String {
        self.path
            .as_deref()
            .and_then(Path::file_name)
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| "제목 없음".to_owned())
    }

    pub fn encode(&self) -> Result<Vec<u8>, Unrepresentable> {
        encoding::encode(&self.text, self.encoding, self.line_ending)
    }

    /// Writes already-encoded bytes to `path` and makes it the document's file.
    pub fn write_to(&mut self, path: &Path, bytes: &[u8]) -> std::io::Result<()> {
        write_file_safely(path, bytes)?;
        if self.path.as_deref() != Some(path) {
            self.path = Some(path.to_path_buf());
        }
        // What is on disk now is exactly our text.
        self.lossy = false;
        self.unread = false;
        self.autosave_blocked = false;
        self.saved_hash = self.current_hash;
        self.saved_encoding = self.encoding;
        self.saved_line_ending = self.line_ending;
        self.disk_stamp = DiskStamp::of(path);
        Ok(())
    }

    /// True if the file on disk changed since we last read or wrote it.
    pub fn changed_on_disk(&self) -> bool {
        let (Some(path), Some(stamp)) = (&self.path, self.disk_stamp) else {
            return false;
        };
        match DiskStamp::of(path) {
            Some(now) => now != stamp,
            None => false, // Deleted or unreachable: keep the text, the user can save again.
        }
    }

    pub fn doc_info(&self) -> history::DocInfo {
        history::DocInfo {
            path: self.path.clone(),
            title: self.title(),
            encoding: self.encoding,
            line_ending: self.line_ending,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn modified_tracks_hash_and_format() {
        let mut doc = Document::new_untitled();
        assert!(!doc.is_modified());
        doc.text.push_str("abc");
        doc.text_changed();
        assert!(doc.is_modified());
        doc.text.clear();
        doc.text_changed();
        assert!(!doc.is_modified(), "undoing back to the saved text is not a modification");
        doc.line_ending = LineEnding::Lf;
        assert!(doc.is_modified());
    }

    #[test]
    fn load_and_save() {
        let dir = std::env::temp_dir().join(format!("mome-doc-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("a.txt");
        std::fs::write(&path, b"one\r\ntwo\r\n").unwrap();
        let (mut doc, report) = Document::load(&path, None).unwrap();
        assert!(!report.lossy);
        assert_eq!(doc.text, "one\ntwo\n");
        assert_eq!(doc.line_ending, LineEnding::CrLf);
        doc.text.push_str("셋\n");
        doc.text_changed();
        let bytes = doc.encode().unwrap();
        doc.write_to(&path, &bytes).unwrap();
        assert!(!doc.is_modified());
        assert_eq!(std::fs::read(&path).unwrap(), "one\r\ntwo\r\n셋\r\n".as_bytes());
        assert!(!doc.changed_on_disk());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn safe_write_replaces_and_cleans_up() {
        let dir = std::env::temp_dir().join(format!("mome-safe-write-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("메모.txt");
        write_file_safely(&path, b"first").unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"first");
        write_file_safely(&path, b"second, longer").unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"second, longer");
        let leftovers: Vec<_> = std::fs::read_dir(&dir).unwrap().flatten().map(|e| e.file_name()).collect();
        assert_eq!(leftovers.len(), 1, "temporary file left behind: {leftovers:?}");

        // Writing in place grows and shrinks the file exactly.
        write_in_place(&path, b"a much longer third version").unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"a much longer third version");
        write_in_place(&path, b"4th").unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"4th");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn unreadable_file_is_not_an_empty_one() {
        let dir = std::env::temp_dir().join(format!("mome-unread-test-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("폴더.txt")).unwrap();
        // Gone: an ordinary new document.
        let (doc, warning) = Document::load_or_new(&dir.join("없음.txt"), None);
        assert!(!doc.unread && warning.is_none());
        // There but unreadable (here: a folder): marked, with a warning.
        let (doc, warning) = Document::load_or_new(&dir.join("폴더.txt"), None);
        assert!(doc.unread && warning.is_some());
        assert!(doc.text.is_empty());
        let _ = std::fs::remove_dir_all(dir);
    }
}
