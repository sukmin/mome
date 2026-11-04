//! Revisions ("임시 저장") kept in the user's temp folder.
//!
//! A *revision* is one saved copy of a document's text; taking a *snapshot* is
//! writing a new revision.
//!
//! Layout:
//!
//! ```text
//! <temp dir>\
//!   memo.txt-1a2b3c4d5e6f7a8b\        one folder per document
//!     mome-doc.json                   original path, title, encoding
//!     20261006-153012-123_auto.txt    one UTF-8 file per revision, never overwritten
//!     20261006-153022-456_save.txt
//!   untitled-20261006-150000-000-1234\
//! ```
//!
//! Writing happens on a background thread. Cleanup deletes revisions older than the
//! retention period (every revision inside it is kept); it only ever touches files whose
//! names match the revision pattern inside folders that contain `mome-doc.json`, so a
//! temp folder shared with other data is safe.

use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, Sender};

use serde::{Deserialize, Serialize};

use crate::encoding::{Encoding, LineEnding};
use crate::time::LocalTime;

pub const DOC_INFO_FILE: &str = "mome-doc.json";

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum RevisionKind {
    /// Periodic snapshot while editing.
    Auto,
    /// "지금 임시 저장" from the menu.
    Manual,
    /// The document was saved to its file.
    Save,
    /// Taken just before restoring an older revision.
    Restore,
    /// Taken just before re-reading the file after another program changed it.
    Reload,
    /// The user chose "저장 안 함" (closing, new document, opening another file…).
    Close,
    /// Windows was shutting down.
    Session,
}

impl RevisionKind {
    const ALL: [RevisionKind; 7] = [
        RevisionKind::Auto,
        RevisionKind::Manual,
        RevisionKind::Save,
        RevisionKind::Restore,
        RevisionKind::Reload,
        RevisionKind::Close,
        RevisionKind::Session,
    ];

    fn tag(self) -> &'static str {
        match self {
            RevisionKind::Auto => "auto",
            RevisionKind::Manual => "manual",
            RevisionKind::Save => "save",
            RevisionKind::Restore => "restore",
            RevisionKind::Reload => "reload",
            RevisionKind::Close => "close",
            RevisionKind::Session => "session",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            RevisionKind::Auto => "자동",
            RevisionKind::Manual => "직접 저장",
            RevisionKind::Save => "파일 저장",
            RevisionKind::Restore => "되돌리기 전",
            RevisionKind::Reload => "다시 불러오기 전",
            RevisionKind::Close => "저장 안 함",
            RevisionKind::Session => "Windows 종료",
        }
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct DocInfo {
    pub path: Option<PathBuf>,
    pub title: String,
    pub encoding: Encoding,
    pub line_ending: LineEnding,
}

#[derive(Clone, Debug)]
pub struct Revision {
    pub path: PathBuf,
    pub time: LocalTime,
    pub kind: RevisionKind,
    pub size: u64,
}

#[derive(Clone, Debug)]
pub struct DocEntry {
    pub key: String,
    pub dir: PathBuf,
    pub info: DocInfo,
    pub count: usize,
    pub latest: Option<LocalTime>,
}

fn fnv1a64(bytes: &[u8]) -> u64 {
    let mut hash = 0xcbf2_9ce4_8422_2325u64;
    for b in bytes {
        hash ^= *b as u64;
        hash = hash.wrapping_mul(0x0100_0000_01b3);
    }
    hash
}

/// Keeps a file name readable in a folder name while dropping anything unsafe.
fn sanitize(name: &str) -> String {
    let cleaned: String = name
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || matches!(c, '.' | '-' | '_' | ' ') {
                c
            } else {
                '_'
            }
        })
        .take(40)
        .collect();
    let trimmed = cleaned.trim_matches(|c| c == '.' || c == ' ');
    if trimmed.is_empty() {
        "file".to_owned()
    } else {
        trimmed.to_owned()
    }
}

/// Folder name for a document stored at `path`; stable across sessions.
pub fn key_for_path(path: &Path) -> String {
    // Windows paths are case-insensitive, so the key must be too.
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().to_lowercase())
        .unwrap_or_default();
    let normalized = path.to_string_lossy().to_lowercase();
    format!("{}-{:016x}", sanitize(&name), fnv1a64(normalized.as_bytes()))
}

/// Folder name for a new, unnamed document.
pub fn key_for_untitled(now: &LocalTime) -> String {
    format!("untitled-{}-{}", now.stamp(), std::process::id())
}

fn revision_file_name(time: &LocalTime, kind: RevisionKind) -> String {
    format!("{}_{}.txt", time.stamp(), kind.tag())
}

pub fn parse_revision_name(name: &str) -> Option<(LocalTime, RevisionKind)> {
    let stem = name.strip_suffix(".txt")?;
    let (stamp, tag) = stem.split_once('_')?;
    let time = LocalTime::parse_stamp(stamp)?;
    let kind = RevisionKind::ALL.into_iter().find(|k| k.tag() == tag)?;
    Some((time, kind))
}

/// The `mome-doc.json` of a document folder.
pub fn read_doc_info(dir: &Path) -> Option<DocInfo> {
    let text = std::fs::read_to_string(dir.join(DOC_INFO_FILE)).ok()?;
    serde_json::from_str(&text).ok()
}

fn write_doc_info(dir: &Path, info: &DocInfo) -> std::io::Result<()> {
    let json = serde_json::to_string_pretty(info).map_err(std::io::Error::other)?;
    std::fs::write(dir.join(DOC_INFO_FILE), json)
}

/// Revisions in a document folder, newest first.
pub fn list_revisions(dir: &Path) -> Vec<Revision> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut revisions: Vec<Revision> = entries
        .flatten()
        .filter_map(|entry| {
            let name = entry.file_name();
            let (time, kind) = parse_revision_name(name.to_str()?)?;
            let meta = entry.metadata().ok()?;
            meta.is_file().then(|| Revision {
                path: entry.path(),
                time,
                kind,
                size: meta.len(),
            })
        })
        .collect();
    revisions.sort_by(|a, b| b.time.cmp(&a.time).then(b.path.cmp(&a.path)));
    revisions
}

/// Documents that have revisions, most recently changed first.
pub fn list_docs(root: &Path) -> Vec<DocEntry> {
    let Ok(entries) = std::fs::read_dir(root) else {
        return Vec::new();
    };
    let mut docs: Vec<DocEntry> = entries
        .flatten()
        .filter(|e| e.path().is_dir())
        .filter_map(|e| {
            let dir = e.path();
            let info = read_doc_info(&dir)?;
            let revisions = list_revisions(&dir);
            if revisions.is_empty() {
                return None;
            }
            Some(DocEntry {
                key: e.file_name().to_string_lossy().into_owned(),
                latest: revisions.first().map(|r| r.time),
                count: revisions.len(),
                info,
                dir,
            })
        })
        .collect();
    docs.sort_by_key(|d| std::cmp::Reverse(d.latest));
    docs
}

/// Reads a revision back as editor text.
pub fn read_revision(path: &Path) -> std::io::Result<String> {
    let bytes = std::fs::read(path)?;
    let text = String::from_utf8_lossy(&bytes);
    Ok(crate::encoding::normalize_newlines(&text))
}

#[derive(Clone, Debug)]
pub struct SnapshotJob {
    pub root: PathBuf,
    pub key: String,
    pub info: DocInfo,
    pub kind: RevisionKind,
    pub time: LocalTime,
    /// Editor text (`\n` line breaks).
    pub text: String,
}

impl SnapshotJob {
    pub fn dir(&self) -> PathBuf {
        self.root.join(&self.key)
    }
}

/// Writes one revision synchronously and returns its path.
pub fn write_snapshot(job: &SnapshotJob) -> std::io::Result<PathBuf> {
    let dir = job.dir();
    std::fs::create_dir_all(&dir)?;
    write_doc_info(&dir, &job.info)?;
    // Stored with Windows line breaks so the file opens nicely anywhere.
    let body = job.text.replace('\n', "\r\n");
    let mut time = job.time;
    for _ in 0..1000 {
        let path = dir.join(revision_file_name(&time, job.kind));
        match std::fs::OpenOptions::new().write(true).create_new(true).open(&path) {
            Ok(mut file) => {
                use std::io::Write as _;
                file.write_all(body.as_bytes())?;
                // Revisions exist to survive crashes and power cuts: make sure it is on disk.
                file.sync_all()?;
                return Ok(path);
            }
            Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => {
                // Same millisecond as an earlier revision: never overwrite, bump instead.
                time.millis = (time.millis + 1) % 1000;
            }
            Err(err) => return Err(err),
        }
    }
    Err(std::io::Error::other("too many revisions in the same second"))
}

/// Moves the revisions of an unnamed document into the folder of its new file name.
fn adopt(root: &Path, from_key: &str, to_key: &str, info: &DocInfo) -> std::io::Result<()> {
    if from_key == to_key {
        return Ok(());
    }
    let from = root.join(from_key);
    let to = root.join(to_key);
    if !from.is_dir() {
        return Ok(());
    }
    std::fs::create_dir_all(&to)?;
    for rev in list_revisions(&from) {
        if let Some(name) = rev.path.file_name() {
            let target = to.join(name);
            if !target.exists() {
                std::fs::rename(&rev.path, &target)?;
            }
        }
    }
    write_doc_info(&to, info)?;
    remove_if_empty(&from);
    Ok(())
}

/// Deletes a document folder if nothing but our info file is left in it.
fn remove_if_empty(dir: &Path) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let names: Vec<_> = entries.flatten().map(|e| e.file_name()).collect();
    if names.iter().all(|n| n == DOC_INFO_FILE) {
        let _ = std::fs::remove_file(dir.join(DOC_INFO_FILE));
        let _ = std::fs::remove_dir(dir);
    }
}

/// Which revisions to delete: only those older than the retention period.
/// Every revision inside the period is kept.
fn plan_cleanup(revisions: &[LocalTime], now: &LocalTime, retention_days: u32) -> Vec<usize> {
    let limit = now.to_seconds() - retention_days as i64 * 86_400;
    revisions
        .iter()
        .enumerate()
        .filter(|(_, time)| time.to_seconds() < limit)
        .map(|(i, _)| i)
        .collect()
}

/// Applies the retention policy to every document folder. Returns the number of files removed.
fn cleanup(root: &Path, retention_days: u32, now: &LocalTime) -> usize {
    let Ok(entries) = std::fs::read_dir(root) else {
        return 0;
    };
    let mut removed = 0;
    for entry in entries.flatten() {
        let dir = entry.path();
        if !dir.join(DOC_INFO_FILE).is_file() {
            continue; // Not ours.
        }
        let revisions = list_revisions(&dir);
        let times: Vec<LocalTime> = revisions.iter().map(|r| r.time).collect();
        for i in plan_cleanup(&times, now, retention_days) {
            if std::fs::remove_file(&revisions[i].path).is_ok() {
                removed += 1;
            }
        }
        remove_if_empty(&dir);
    }
    removed
}

pub enum Job {
    Snapshot(SnapshotJob),
    Cleanup { root: PathBuf, retention_days: u32 },
    Adopt { root: PathBuf, from_key: String, to_key: String, info: DocInfo },
}

/// What the history thread reports back to the window.
#[derive(Debug)]
pub enum WriterEvent {
    /// A revision was written.
    Written { path: PathBuf, time: LocalTime, kind: RevisionKind, hash: u64, key: String },
    Failed { error: String },
}

/// Background thread that writes revisions and runs cleanup.
pub struct HistoryWriter {
    tx: Sender<Job>,
    thread: std::thread::JoinHandle<()>,
}

impl HistoryWriter {
    /// Starts the thread. `notify` is called after each event, to wake up the UI.
    pub fn spawn(notify: impl Fn() + Send + 'static) -> (Self, Receiver<WriterEvent>) {
        let (tx, rx) = mpsc::channel::<Job>();
        let (event_tx, event_rx) = mpsc::channel();
        let thread = std::thread::Builder::new()
            .name("mome-history".to_owned())
            .spawn(move || {
                for job in rx {
                    // The app may already be gone; keep writing whatever is queued.
                    if let Some(event) = run_job(job) {
                        if event_tx.send(event).is_ok() {
                            notify();
                        }
                    }
                }
            })
            .expect("failed to start the history thread");
        (Self { tx, thread }, event_rx)
    }

    pub fn send(&self, job: Job) {
        let _ = self.tx.send(job);
    }

    /// Waits for queued jobs to finish (at most a couple of seconds).
    pub fn finish(self) {
        let Self { tx, thread } = self;
        drop(tx);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
        while !thread.is_finished() && std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
    }
}

/// Does one job on the history thread.
fn run_job(job: Job) -> Option<WriterEvent> {
    match job {
        Job::Snapshot(job) => match write_snapshot(&job) {
            Ok(path) => Some(WriterEvent::Written {
                path,
                time: job.time,
                kind: job.kind,
                hash: crate::document::hash_text(&job.text),
                key: job.key,
            }),
            Err(err) => {
                log::error!("snapshot failed in {}: {err}", job.root.display());
                Some(WriterEvent::Failed {
                    error: err.to_string(),
                })
            }
        },
        Job::Cleanup { root, retention_days } => {
            let removed = cleanup(&root, retention_days, &LocalTime::now());
            if removed > 0 {
                log::info!("history cleanup removed {removed} old revisions");
            }
            None
        }
        Job::Adopt {
            root,
            from_key,
            to_key,
            info,
        } => {
            if let Err(err) = adopt(&root, &from_key, &to_key, &info) {
                log::warn!("could not move revisions {from_key} -> {to_key}: {err}");
            }
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t(s: &str) -> LocalTime {
        LocalTime::parse_stamp(s).unwrap()
    }

    #[test]
    fn names_round_trip() {
        let time = t("20261006-153012-123");
        let name = revision_file_name(&time, RevisionKind::Save);
        assert_eq!(name, "20261006-153012-123_save.txt");
        assert_eq!(parse_revision_name(&name), Some((time, RevisionKind::Save)));
        assert_eq!(parse_revision_name("notes.txt"), None);
        assert_eq!(parse_revision_name("20261006-153012-123_other.txt"), None);
    }

    #[test]
    fn keys_are_stable_and_safe() {
        let a = key_for_path(Path::new("C:\\Docs\\메모 1.txt"));
        let b = key_for_path(Path::new("c:\\docs\\메모 1.TXT"));
        assert_eq!(a, b);
        assert!(a.starts_with("메모 1.txt-"));
        let weird = key_for_path(Path::new("C:\\x\\a:b*c?.txt"));
        assert!(!weird.contains([':', '*', '?']));
    }

    #[test]
    fn cleanup_plan_keeps_everything_inside_retention() {
        let now = t("20261031-120000-000");
        let revs = vec![
            t("20261031-115950-000"), // 10 s old
            t("20261031-115940-000"), // 20 s old, nearly identical: still kept
            t("20261001-120001-000"), // just under 30 days
            t("20261001-115959-000"), // just over 30 days: delete
            t("20260930-110000-000"), // 31 days: delete
        ];
        assert_eq!(plan_cleanup(&revs, &now, 30), vec![3, 4]);
        assert_eq!(plan_cleanup(&revs, &now, 1), vec![2, 3, 4]);
    }

    #[test]
    fn write_list_cleanup() {
        let root = std::env::temp_dir().join(format!("mome-history-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let job = SnapshotJob {
            root: root.clone(),
            key: "a.txt-0001".to_owned(),
            info: DocInfo {
                title: "a.txt".to_owned(),
                ..Default::default()
            },
            kind: RevisionKind::Auto,
            time: t("20261006-100000-000"),
            text: "줄1\n줄2".to_owned(),
        };
        let p1 = write_snapshot(&job).unwrap();
        let p2 = write_snapshot(&job).unwrap(); // same time: must not overwrite
        assert_ne!(p1, p2);
        assert_eq!(read_revision(&p1).unwrap(), "줄1\n줄2");
        assert_eq!(std::fs::read(&p1).unwrap(), "줄1\r\n줄2".as_bytes());

        // A foreign file in the document folder and a foreign folder are left alone.
        std::fs::write(job.dir().join("keep-me.txt"), "x").unwrap();
        std::fs::create_dir_all(root.join("other")).unwrap();
        std::fs::write(root.join("other").join("20200101-000000-000_auto.txt"), "x").unwrap();

        let docs = list_docs(&root);
        assert_eq!(docs.len(), 1);
        assert_eq!(docs[0].count, 2);

        let far_future = t("20270101-000000-000");
        assert_eq!(cleanup(&root, 30, &far_future), 2);
        assert!(job.dir().join("keep-me.txt").exists());
        assert!(root.join("other").join("20200101-000000-000_auto.txt").exists());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn adopt_moves_revisions() {
        let root = std::env::temp_dir().join(format!("mome-adopt-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let job = SnapshotJob {
            root: root.clone(),
            key: "untitled-x".to_owned(),
            info: DocInfo::default(),
            kind: RevisionKind::Auto,
            time: t("20261006-100000-000"),
            text: "hello".to_owned(),
        };
        write_snapshot(&job).unwrap();
        adopt(&root, "untitled-x", "b.txt-0002", &DocInfo::default()).unwrap();
        assert!(!root.join("untitled-x").exists());
        assert_eq!(list_revisions(&root.join("b.txt-0002")).len(), 1);
        let _ = std::fs::remove_dir_all(&root);
    }
}
