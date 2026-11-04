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
    use std::io::Write as _;
    let mut file = std::fs::File::create(path)?;
    file.write_all(bytes)?;
    file.sync_all()
}

/// Writes `bytes` to `path` so that a crash or power cut never leaves a half-written file:
/// the data goes to a temporary file next to it, is flushed, and then replaces the
/// original with `ReplaceFileW` (which keeps its attributes and permissions).
/// Falls back to writing in place when that is not possible (e.g. read-only folder).
pub fn write_file_safely(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    use windows_sys::Win32::Storage::FileSystem::{ReplaceFileW, REPLACEFILE_IGNORE_MERGE_ERRORS};

    let write_direct = || write_synced(path, bytes);
    // A symbolic link is written through, keeping the link.
    let is_link = std::fs::symlink_metadata(path).is_ok_and(|m| m.file_type().is_symlink());
    let Some(name) = path.file_name().filter(|_| !is_link) else {
        return write_direct();
    };
    let tmp = path.with_file_name(format!(".{}.mome-saving", name.to_string_lossy()));
    if write_synced(&tmp, bytes).is_err() {
        let _ = std::fs::remove_file(&tmp);
        return write_direct();
    }
    if !path.exists() {
        return std::fs::rename(&tmp, path).or_else(|_| {
            let _ = std::fs::remove_file(&tmp);
            write_direct()
        });
    }
    let target = crate::win::wide_path(path);
    let replacement = crate::win::wide_path(&tmp);
    let ok = unsafe {
        ReplaceFileW(
            target.as_ptr(),
            replacement.as_ptr(),
            std::ptr::null(),
            REPLACEFILE_IGNORE_MERGE_ERRORS,
            std::ptr::null(),
            std::ptr::null(),
        )
    } != 0;
    if ok {
        Ok(())
    } else {
        log::warn!(
            "ReplaceFileW failed for {} ({}); writing in place",
            path.display(),
            std::io::Error::last_os_error()
        );
        let _ = std::fs::remove_file(&tmp);
        write_direct()
    }
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
        };
        let report = LoadReport {
            lossy: decoded.lossy,
            had_nul: decoded.had_nul,
            large: meta.len() > LARGE_FILE_BYTES,
        };
        Ok((doc, report))
    }

    /// Loads `path`, or starts an empty document for it if the file cannot be read
    /// (e.g. it was deleted since).
    pub fn load_or_new(path: &Path, forced: Option<Encoding>) -> Self {
        match Self::load(path, forced) {
            Ok((doc, _)) => doc,
            Err(_) => Self::new_at(path),
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
        let _ = std::fs::remove_dir_all(dir);
    }
}
