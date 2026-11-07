//! Markdown for the reader view: Obsidian syntax rewritten to CommonMark, links
//! resolved to files, and a loader for local images.

use std::collections::HashMap;
use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use egui::load::{BytesLoadResult, BytesLoader, BytesPoll, LoadError};

/// Scheme of the links made from `[[wikilinks]]`; resolved by [`resolve_link`].
pub const WIKI_SCHEME: &str = "wiki:";

const IMAGE_EXTENSIONS: &[&str] = &["png", "jpg", "jpeg", "gif", "webp", "bmp"];
/// Obsidian's callout types (`> [!note]`): type, Korean label, icon, colour.
pub const CALLOUTS: &[(&str, &str, char, [u8; 3])] = &[
    ("NOTE", "노트", '✏', BLUE),
    ("INFO", "정보", 'ℹ', BLUE),
    ("TODO", "할 일", '☑', BLUE),
    ("ABSTRACT", "요약", '📋', CYAN),
    ("SUMMARY", "요약", '📋', CYAN),
    ("TLDR", "요약", '📋', CYAN),
    ("TIP", "팁", '💡', CYAN),
    ("HINT", "팁", '💡', CYAN),
    ("IMPORTANT", "중요", '❗', CYAN),
    ("SUCCESS", "성공", '✔', GREEN),
    ("CHECK", "성공", '✔', GREEN),
    ("DONE", "완료", '✔', GREEN),
    ("QUESTION", "질문", '❓', ORANGE),
    ("HELP", "질문", '❓', ORANGE),
    ("FAQ", "질문", '❓', ORANGE),
    ("WARNING", "경고", '⚠', ORANGE),
    ("CAUTION", "주의", '⚠', ORANGE),
    ("ATTENTION", "주의", '⚠', ORANGE),
    ("FAILURE", "실패", '❌', RED),
    ("FAIL", "실패", '❌', RED),
    ("MISSING", "없음", '❌', RED),
    ("DANGER", "위험", '⚡', RED),
    ("ERROR", "오류", '⚡', RED),
    ("BUG", "버그", '🐛', RED),
    ("EXAMPLE", "예시", '📑', PURPLE),
    ("QUOTE", "인용", '“', GRAY),
    ("CITE", "인용", '“', GRAY),
];
const BLUE: [u8; 3] = [0x08, 0x6D, 0xDD];
const CYAN: [u8; 3] = [0x00, 0xA3, 0xB4];
const GREEN: [u8; 3] = [0x08, 0xA0, 0x4A];
const ORANGE: [u8; 3] = [0xE0, 0x7A, 0x00];
const RED: [u8; 3] = [0xD9, 0x32, 0x3C];
const PURPLE: [u8; 3] = [0x7B, 0x5C, 0xE6];
const GRAY: [u8; 3] = [0x88, 0x88, 0x88];

/// Files the vault index takes in at most.
const INDEX_LIMIT: usize = 200_000;
/// How long a vault index is used before the folders are read again.
const INDEX_TTL: Duration = Duration::from_secs(10);

pub fn is_image(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| IMAGE_EXTENSIONS.iter().any(|i| e.eq_ignore_ascii_case(i)))
}

/// Rewrites Obsidian additions so a CommonMark viewer can show them:
/// front matter becomes a code block, `[[note|alias]]` a link, `![[image.png]]` an
/// image, and `%%comments%%` disappear. Code blocks and code spans are left alone.
pub fn prepare(text: &str, base_dir: Option<&Path>) -> String {
    let mut out = String::with_capacity(text.len() + 64);
    let mut rest = text;
    if let Some((front, after)) = split_front_matter(text) {
        // A fence longer than any backtick run inside, so the block cannot end early.
        let fence = "`".repeat(longest_run(front, '`').max(2) + 1);
        out.push_str(&fence);
        out.push('\n');
        out.push_str(front);
        out.push('\n');
        out.push_str(&fence);
        out.push_str("\n\n");
        rest = after;
    }
    let mut fence: Option<Fence> = None;
    let mut in_comment = false;
    for line in rest.split_inclusive('\n') {
        let content = unquoted(line);
        if let Some(f) = fence {
            if f.quoted && !is_quoted(line) {
                // Leaving the block quote ends a code block opened inside it.
                fence = None;
            } else {
                out.push_str(line);
                if f.closed_by(content) {
                    fence = None;
                }
                continue;
            }
        }
        if !in_comment {
            if let Some((ch, len)) = fence_of(content) {
                fence = Some(Fence { ch, len, quoted: is_quoted(line) });
                out.push_str(line);
                continue;
            }
            if let Some((head, title)) = callout(line) {
                out.push_str(&head);
                if let Some((prefix, title)) = title {
                    let mut shown = String::new();
                    in_comment = rewrite_line(title, base_dir, false, &mut shown);
                    let shown = shown.trim();
                    if !shown.is_empty() {
                        out.push_str(prefix);
                        out.push_str("**");
                        out.push_str(shown);
                        out.push_str("**\n");
                        // An empty quote line ends the title's paragraph.
                        out.push_str(prefix.trim_end());
                        out.push('\n');
                    }
                }
                continue;
            }
        }
        in_comment = rewrite_line(line, base_dir, in_comment, &mut out);
    }
    out
}

/// An open code block.
#[derive(Clone, Copy)]
struct Fence {
    ch: char,
    len: usize,
    /// Opened inside a block quote or callout.
    quoted: bool,
}

impl Fence {
    /// Whether `content` (a line without its quote markers) closes this block.
    fn closed_by(self, content: &str) -> bool {
        let run = content.chars().take_while(|c| *c == self.ch).count();
        run >= self.len && content[run..].trim().is_empty()
    }
}

fn is_quoted(line: &str) -> bool {
    line.trim_start().starts_with('>')
}

/// The line without indentation and block quote markers (`> > `).
fn unquoted(line: &str) -> &str {
    line.trim_start_matches(['>', ' ', '\t'])
}

fn longest_run(s: &str, ch: char) -> usize {
    let mut longest = 0;
    let mut run = 0;
    for c in s.chars() {
        run = if c == ch { run + 1 } else { 0 };
        longest = longest.max(run);
    }
    longest
}

/// A callout's first line, `> [!type]- Title`: the viewer only knows `> [!TYPE]`
/// alone, so the title goes on a line of its own, in bold. Unknown types show as
/// notes titled with the type, as in Obsidian. Returns (new first line, (quote
/// prefix, title)).
fn callout(line: &str) -> Option<(String, Option<(&str, &str)>)> {
    let body = line.trim_end_matches(['\n', '\r']);
    let after_quote = body.trim_start_matches(['>', ' ', '\t']);
    let prefix = &body[..body.len() - after_quote.len()];
    if !prefix.contains('>') {
        return None;
    }
    let rest = after_quote.strip_prefix("[!")?;
    let close = rest.find(']')?;
    let kind = rest[..close].to_uppercase();
    if kind.is_empty() || !kind.chars().all(|c| c.is_alphanumeric() || c == '-' || c == '_') {
        return None;
    }
    let mut title = rest[close + 1..].trim_start_matches(['+', '-']).trim();
    let known = CALLOUTS.iter().any(|(id, ..)| *id == kind);
    let id = if known { kind.as_str() } else { "NOTE" };
    if !known && title.is_empty() {
        title = &rest[..close];
    }
    let head = format!("{prefix}[!{id}]\n");
    Some((head, (!title.is_empty()).then_some((prefix, title))))
}

/// `---` front matter at the very start: (its body, the text after it).
fn split_front_matter(text: &str) -> Option<(&str, &str)> {
    let body = text.strip_prefix("---\n")?;
    let mut at = 0;
    for line in body.split_inclusive('\n') {
        if matches!(line.trim_end(), "---" | "...") {
            let front = body[..at].trim_end_matches('\n');
            return Some((front, &body[at + line.len()..]));
        }
        at += line.len();
    }
    None
}

/// A code fence (``` or ~~~, three or more) opening this line: (character, length).
/// `content` is the line without indentation and quote markers.
fn fence_of(content: &str) -> Option<(char, usize)> {
    let ch = content.chars().next().filter(|c| *c == '`' || *c == '~')?;
    let len = content.chars().take_while(|c| *c == ch).count();
    // After a backtick fence there may be no more backticks: ```code``` is inline code.
    let inline = ch == '`' && content[len..].contains('`');
    (len >= 3 && !inline).then_some((ch, len))
}

/// Rewrites one line outside code blocks. Returns whether a `%%` comment is still open.
fn rewrite_line(line: &str, base_dir: Option<&Path>, mut in_comment: bool, out: &mut String) -> bool {
    let mut rest = line;
    while !rest.is_empty() {
        if in_comment {
            match rest.find("%%") {
                Some(end) => {
                    rest = &rest[end + 2..];
                    in_comment = false;
                }
                None => {
                    // Keep the line structure so paragraphs do not merge.
                    if rest.ends_with('\n') {
                        out.push('\n');
                    }
                    return true;
                }
            }
            continue;
        }
        let Some(at) = rest.find(['`', '%', '[', '!']) else {
            out.push_str(rest);
            break;
        };
        out.push_str(&rest[..at]);
        rest = &rest[at..];
        if rest.starts_with('`') {
            // A code span: copied as is up to the closing run of the same length.
            let run = rest.chars().take_while(|c| *c == '`').count();
            let ticks = &rest[..run];
            let end = rest[run..].find(ticks).map_or(rest.len(), |e| run + e + run);
            out.push_str(&rest[..end]);
            rest = &rest[end..];
        } else if let Some(after) = rest.strip_prefix("%%") {
            in_comment = true;
            rest = after;
        } else if let Some((inner, after)) = rest.strip_prefix('!').and_then(wikilink) {
            embed(inner, base_dir, out);
            rest = after;
        } else if let Some((inner, after)) = wikilink(rest) {
            link(inner, out);
            rest = after;
        } else {
            let ch = rest.chars().next().map_or(1, char::len_utf8);
            out.push_str(&rest[..ch]);
            rest = &rest[ch..];
        }
    }
    in_comment
}

/// `[[inner]]` at the start of `s`: (inner, text after it).
fn wikilink(s: &str) -> Option<(&str, &str)> {
    let body = s.strip_prefix("[[")?;
    let end = body.find("]]")?;
    let inner = &body[..end];
    (!inner.is_empty() && !inner.contains(['[', '\n'])).then(|| (inner, &body[end + 2..]))
}

/// Splits `note#heading|alias` into (note, heading, alias).
fn parts(inner: &str) -> (&str, Option<&str>, Option<&str>) {
    let (target, alias) = match inner.split_once('|') {
        // In tables the `|` is written `\|`.
        Some((t, a)) => (t.trim_end_matches('\\'), Some(a.trim()).filter(|a| !a.is_empty())),
        None => (inner, None),
    };
    let (note, heading) = match target.split_once('#') {
        Some((n, h)) => (n, Some(h)),
        None => (target, None),
    };
    (note.trim(), heading.map(str::trim), alias)
}

fn link(inner: &str, out: &mut String) {
    let (note, heading, alias) = parts(inner);
    let shown = match (alias, heading) {
        (Some(a), _) => a.to_owned(),
        (None, Some(h)) if note.is_empty() => h.to_owned(),
        (None, Some(h)) => format!("{note} > {h}"),
        (None, None) => note.to_owned(),
    };
    if note.is_empty() {
        // `[[#Heading]]` points into this note; just show the text.
        escape_into(&shown, out);
        return;
    }
    out.push('[');
    escape_into(&shown, out);
    out.push_str("](<");
    out.push_str(WIKI_SCHEME);
    out.push_str(&destination(note));
    out.push_str(">)");
}

fn embed(inner: &str, base_dir: Option<&Path>, out: &mut String) {
    let (note, _, _) = parts(inner);
    if is_image(Path::new(note)) {
        let found = base_dir.and_then(|dir| find_file(note, dir));
        match found {
            Some(path) => {
                out.push_str("![");
                escape_into(note, out);
                out.push_str("](<");
                out.push_str(&destination(&file_uri(&path)));
                out.push_str(">)");
            }
            None => {
                out.push_str("*(그림을 찾을 수 없음: ");
                escape_into(note, out);
                out.push_str(")*");
            }
        }
    } else {
        // An embedded note: shown as a link to it.
        out.push_str("↳ ");
        link(inner, out);
    }
}

/// Text that may sit inside `<…>` link destinations (a backslash at the end would
/// escape the closing `>`).
fn destination(s: &str) -> String {
    let s: String = s.chars().filter(|c| !matches!(c, '<' | '>' | '\n')).collect();
    s.trim_end_matches('\\').to_owned()
}

fn escape_into(s: &str, out: &mut String) {
    for c in s.chars() {
        if matches!(c, '\\' | '[' | ']' | '*' | '_' | '`' | '<') {
            out.push('\\');
        }
        out.push(c);
    }
}

/// `file:///C:/dir/a%25b.png`: only `%` is encoded, matching [`LocalFileLoader`].
pub fn file_uri(path: &Path) -> String {
    let p = path.to_string_lossy().replace('\\', "/").replace('%', "%25");
    if let Some(unc) = p.strip_prefix("//?/UNC/") {
        return format!("file://{unc}");
    }
    let p = p.strip_prefix("//?/").unwrap_or(&p);
    if let Some(unc) = p.strip_prefix("//") {
        format!("file://{unc}")
    } else {
        format!("file:///{p}")
    }
}

/// The base for relative image paths in standard Markdown (`![](img/a.png)`).
pub fn base_uri(dir: &Path) -> String {
    let mut uri = file_uri(dir);
    if !uri.ends_with('/') {
        uri.push('/');
    }
    uri
}

/// The Obsidian vault holding `dir`: the nearest folder with `.obsidian` in it.
pub fn vault_root(dir: &Path) -> Option<PathBuf> {
    dir.ancestors().find(|d| d.join(".obsidian").is_dir()).map(Path::to_path_buf)
}

/// Finds the file a link names, the way Obsidian does: next to the note, from the
/// vault root, then anywhere in the vault by file name. Notes may leave out `.md`.
pub fn find_file(name: &str, dir: &Path) -> Option<PathBuf> {
    let name = name.trim().replace('\\', "/");
    if name.is_empty() || !is_relative_name(&name) {
        return None;
    }
    let with_md = format!("{name}.md");
    let candidates: Vec<&str> = if Path::new(&name).extension().is_some() {
        vec![&name, &with_md]
    } else {
        vec![&with_md, &name]
    };
    let root = vault_root(dir);
    for base in std::iter::once(dir).chain(root.as_deref()) {
        for c in &candidates {
            let path = base.join(c);
            if path.is_file() {
                return Some(path);
            }
        }
    }
    let search_root = root.as_deref().unwrap_or(dir);
    let wanted: Vec<String> = candidates
        .iter()
        .filter_map(|c| Path::new(c).file_name())
        .map(|n| n.to_string_lossy().to_lowercase())
        .collect();
    search(search_root, &wanted)
}

/// A name written in a note or canvas: only paths inside the vault or next to the
/// note count, never `C:\…`, `\\server\…` or `/…`.
pub fn is_relative_name(name: &str) -> bool {
    let path = Path::new(name);
    !path.has_root() && !path.components().any(|c| matches!(c, Component::Prefix(_)))
}

/// File names in a vault (lower case → the shallowest file with that name), so
/// links that give only a name do not walk the folders each time.
struct VaultIndex {
    root: PathBuf,
    built: Instant,
    files: HashMap<String, PathBuf>,
}

static INDEX: Mutex<Option<VaultIndex>> = Mutex::new(None);

impl VaultIndex {
    /// Reads the folders breadth first, skipping hidden ones such as `.obsidian`.
    fn build(root: &Path) -> Self {
        let mut files = HashMap::new();
        let mut queue = std::collections::VecDeque::from([root.to_path_buf()]);
        let mut seen = 0;
        'walk: while let Some(dir) = queue.pop_front() {
            let Ok(entries) = std::fs::read_dir(&dir) else {
                continue;
            };
            for entry in entries.flatten() {
                seen += 1;
                if seen > INDEX_LIMIT {
                    break 'walk;
                }
                let name = entry.file_name().to_string_lossy().to_lowercase();
                let Ok(kind) = entry.file_type() else {
                    continue;
                };
                if kind.is_dir() {
                    if !name.starts_with('.') {
                        queue.push_back(entry.path());
                    }
                } else {
                    files.entry(name).or_insert_with(|| entry.path());
                }
            }
        }
        Self {
            root: root.to_path_buf(),
            built: Instant::now(),
            files,
        }
    }
}

/// The file named one of `wanted` (lower case) anywhere under `root`.
fn search(root: &Path, wanted: &[String]) -> Option<PathBuf> {
    let mut index = INDEX.lock().unwrap_or_else(|e| e.into_inner());
    let stale = index.as_ref().is_none_or(|i| i.root != root || i.built.elapsed() > INDEX_TTL);
    if stale {
        *index = Some(VaultIndex::build(root));
    }
    let files = &index.as_ref()?.files;
    wanted.iter().find_map(|w| files.get(w).cloned())
}

/// `\\server\share` (lower case) of a network path; device paths count too.
/// `None` for paths on this PC.
pub fn network_share(path: &Path) -> Option<String> {
    let s = path.to_string_lossy().replace('/', "\\");
    let rest = if let Some(rest) = s.strip_prefix(r"\\?\UNC\") {
        rest
    } else if let Some(rest) = s.strip_prefix(r"\\?\") {
        // `\\?\C:\…` is a local path; other verbatim paths are devices.
        return (rest.as_bytes().get(1) != Some(&b':')).then(|| s.to_lowercase());
    } else if s.starts_with(r"\\.\") {
        return Some(s.to_lowercase());
    } else {
        s.strip_prefix(r"\\")?
    };
    let mut parts = rest.split('\\');
    let server = parts.next().unwrap_or_default();
    let share = parts.next().unwrap_or_default();
    Some(format!(r"\\{server}\{share}").to_lowercase())
}

/// Whether a note in `dir` may make mome read `path`: anything on this PC, but a
/// network path only on the note's own share (a link must not make Windows
/// connect, and send the user's credentials, to some other server).
pub fn may_read(path: &Path, dir: Option<&Path>) -> bool {
    match network_share(path) {
        None => true,
        Some(share) => dir.and_then(network_share).is_some_and(|own| own == share),
    }
}

/// Where a clicked link goes.
#[derive(Debug, PartialEq)]
pub enum LinkTarget {
    /// A file on this PC.
    File(PathBuf),
    /// A web page or other URL, for the browser.
    Url(String),
    /// A note or file that could not be found.
    Missing(String),
    /// A place in this note (`#heading`).
    Anchor,
    /// A network path on another server, which links may not open.
    Blocked(String),
}

/// Resolves a link clicked in a note in `dir` (`None` for untitled text).
pub fn resolve_link(url: &str, dir: Option<&Path>) -> LinkTarget {
    if let Some(name) = url.strip_prefix(WIKI_SCHEME) {
        return match dir.and_then(|d| find_file(name, d)) {
            Some(path) => LinkTarget::File(path),
            None => LinkTarget::Missing(name.to_owned()),
        };
    }
    if url.starts_with('#') {
        return LinkTarget::Anchor;
    }
    if url.starts_with("file:") {
        let path = file_uri_path(url);
        return if may_read(&path, dir) {
            LinkTarget::File(path)
        } else {
            LinkTarget::Blocked(path.display().to_string())
        };
    }
    if has_scheme(url) {
        return LinkTarget::Url(url.to_owned());
    }
    let path = percent_decode(url.split('#').next().unwrap_or(url));
    if path.is_empty() {
        return LinkTarget::Missing(url.to_owned());
    }
    let Some(dir) = dir else {
        return LinkTarget::Missing(path);
    };
    let joined = dir.join(&path);
    if !may_read(&joined, Some(dir)) {
        return LinkTarget::Blocked(joined.display().to_string());
    }
    if joined.exists() {
        return LinkTarget::File(joined);
    }
    match find_file(&path, dir) {
        Some(found) => LinkTarget::File(found),
        None => LinkTarget::Missing(path),
    }
}

/// The path in a `file:` URI: `file:///C:/a%20b.md#x` → `C:/a b.md`, and
/// `file://server/share/x` → `\\server\share\x`.
fn file_uri_path(uri: &str) -> PathBuf {
    let rest = uri.strip_prefix("file:").unwrap_or(uri);
    let rest = rest.split('#').next().unwrap_or(rest);
    match rest.strip_prefix("///") {
        Some(local) => PathBuf::from(percent_decode(local)),
        None => PathBuf::from(format!(r"\\{}", percent_decode(rest.trim_start_matches('/')))),
    }
}

/// `https:`, `mailto:` and the like, but not a drive letter (`C:`).
pub fn has_scheme(url: &str) -> bool {
    match url.split_once(':') {
        Some((scheme, _)) => {
            scheme.len() > 1
                && scheme.chars().next().is_some_and(|c| c.is_ascii_alphabetic())
                && scheme.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'))
        }
        None => false,
    }
}

pub fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let (Some(hi), Some(lo)) = (hex(bytes[i + 1]), hex(bytes[i + 2])) {
                out.push(hi * 16 + lo);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn hex(b: u8) -> Option<u8> {
    (b as char).to_digit(16).map(|d| d as u8)
}

/// A file's bytes, or why it could not be read.
type FileResult = Result<Arc<[u8]>, String>;

/// Reads `file:` images for the viewers. Unlike egui_extras' loader it decodes
/// `%20`-style escapes, so `![](my%20image.png)` works, and it refuses network
/// paths other than the document's own share (see [`may_read`]). Being added
/// last, it is asked before egui_extras' loader, which therefore never sees a
/// `file:` URI.
#[derive(Default)]
pub struct LocalFileLoader {
    cache: Mutex<HashMap<String, FileResult>>,
    /// Folder of the document being shown.
    dir: Mutex<Option<PathBuf>>,
}

impl LocalFileLoader {
    pub const ID: &'static str = "mome::LocalFileLoader";

    /// The document whose pictures are shown now lives in `dir`.
    pub fn set_document_dir(&self, dir: Option<&Path>) {
        let mut current = self.dir.lock().unwrap_or_else(|e| e.into_inner());
        if current.as_deref() != dir {
            *current = dir.map(Path::to_path_buf);
        }
    }
}

impl BytesLoader for LocalFileLoader {
    fn id(&self) -> &str {
        Self::ID
    }

    fn load(&self, _ctx: &egui::Context, uri: &str) -> BytesLoadResult {
        if !uri.starts_with("file:") {
            return Err(LoadError::NotSupported);
        }
        let path = file_uri_path(uri);
        let dir = self.dir.lock().unwrap_or_else(|e| e.into_inner()).clone();
        if !may_read(&path, dir.as_deref()) {
            return Err(LoadError::Loading("다른 컴퓨터(네트워크 경로)의 그림은 불러오지 않습니다.".to_owned()));
        }
        let mut cache = self.cache.lock().unwrap_or_else(|e| e.into_inner());
        let entry = cache
            .entry(uri.to_owned())
            .or_insert_with(|| std::fs::read(&path).map(Arc::from).map_err(|e| e.to_string()));
        match entry {
            Ok(bytes) => Ok(BytesPoll::Ready {
                size: None,
                bytes: egui::load::Bytes::Shared(Arc::clone(bytes)),
                mime: None,
            }),
            Err(err) => Err(LoadError::Loading(err.clone())),
        }
    }

    fn forget(&self, uri: &str) {
        self.cache.lock().unwrap_or_else(|e| e.into_inner()).remove(uri);
    }

    fn forget_all(&self) {
        self.cache.lock().unwrap_or_else(|e| e.into_inner()).clear();
    }

    fn byte_size(&self) -> usize {
        let cache = self.cache.lock().unwrap_or_else(|e| e.into_inner());
        cache.values().map(|v| v.as_ref().map_or(0, |b| b.len())).sum()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wikilinks() {
        assert_eq!(prepare("[[노트]] 끝", None), "[노트](<wiki:노트>) 끝");
        assert_eq!(prepare("[[a/b#제목|별칭]]", None), "[별칭](<wiki:a/b>)");
        assert_eq!(prepare("[[노트#제목]]", None), "[노트 > 제목](<wiki:노트>)");
        assert_eq!(prepare("[[#제목]]", None), "제목");
        assert_eq!(prepare("[[a_b]]", None), "[a\\_b](<wiki:a_b>)");
        assert_eq!(prepare("[링크](http://x) [[", None), "[링크](http://x) [[");
    }

    #[test]
    fn code_is_untouched() {
        let text = "`[[a]]` [[b]]\n```\n[[c]] %%x%%\n```\n%%숨김%% 보임";
        assert_eq!(
            prepare(text, None),
            "`[[a]]` [b](<wiki:b>)\n```\n[[c]] %%x%%\n```\n 보임"
        );
    }

    #[test]
    fn comments_span_lines() {
        assert_eq!(prepare("가 %%시작\n중간\n끝%% 나\n다", None), "가 \n\n 나\n다");
    }

    #[test]
    fn front_matter() {
        assert_eq!(prepare("---\ntags: a\n---\n# 제목", None), "```\ntags: a\n```\n\n# 제목");
        assert_eq!(prepare("---\n열린 채", None), "---\n열린 채");
    }

    #[test]
    fn callouts() {
        assert_eq!(prepare("> [!note]
> 본문", None), "> [!NOTE]
> 본문");
        assert_eq!(prepare("> [!tip]- 접힌 [[제목]]
> 본문", None), "> [!TIP]
> **접힌 [제목](<wiki:제목>)**
>
> 본문");
        assert_eq!(prepare(">[!메모]
>본문", None), ">[!NOTE]
>**메모**
>
>본문");
        assert_eq!(prepare("[!note] 인용 아님", None), "[!note] 인용 아님");
        assert_eq!(prepare("```
> [!note] 코드
```", None), "```
> [!note] 코드
```");
    }

    #[test]
    fn missing_image() {
        assert_eq!(prepare("![[없음.png]]", None), "*(그림을 찾을 수 없음: 없음.png)*");
        assert_eq!(prepare("![[노트]]", None), "↳ [노트](<wiki:노트>)");
    }

    #[test]
    fn uris_and_links() {
        assert_eq!(file_uri(Path::new(r"C:\a b\c%.png")), "file:///C:/a b/c%25.png");
        assert_eq!(file_uri(Path::new(r"\\srv\share\x.png")), "file://srv/share/x.png");
        assert_eq!(percent_decode("a%20b%2"), "a b%2");
        assert_eq!(percent_decode("%ED%95%9C"), "한");
        assert_eq!(resolve_link("https://x.kr", None), LinkTarget::Url("https://x.kr".into()));
        assert_eq!(resolve_link("mailto:a@b", None), LinkTarget::Url("mailto:a@b".into()));
        assert_eq!(
            resolve_link("file:///C:/a%20b.png", None),
            LinkTarget::File(PathBuf::from("C:/a b.png"))
        );
        assert_eq!(resolve_link("wiki:x", None), LinkTarget::Missing("x".into()));
        assert!(!has_scheme("C:/x"));
        assert_eq!(resolve_link("#제목", None), LinkTarget::Anchor);
        assert_eq!(
            resolve_link("file:///C:/a.md#h", None),
            LinkTarget::File(PathBuf::from("C:/a.md"))
        );
        assert_eq!(file_uri(Path::new(r"\\?\UNC\srv\share\x.png")), "file://srv/share/x.png");
    }

    #[test]
    fn network_paths_are_kept_out() {
        assert_eq!(network_share(Path::new(r"C:\a")), None);
        assert_eq!(network_share(Path::new(r"\\?\C:\a")), None);
        assert_eq!(network_share(Path::new(r"\\Srv\Share\a\b")).as_deref(), Some(r"\\srv\share"));
        assert_eq!(network_share(Path::new("//srv/share/a")).as_deref(), Some(r"\\srv\share"));
        assert_eq!(network_share(Path::new(r"\\?\UNC\srv\share\a")).as_deref(), Some(r"\\srv\share"));
        assert!(network_share(Path::new(r"\\.\pipe\x")).is_some());

        let local = Some(Path::new(r"C:\notes"));
        let shared = Some(Path::new(r"\\srv\share\notes"));
        assert!(may_read(Path::new(r"\\srv\share\img.png"), shared));
        assert!(!may_read(Path::new(r"\\srv\share\img.png"), local));
        assert!(!may_read(Path::new(r"\\evil\x\img.png"), shared));

        assert!(matches!(resolve_link("file://evil/share/a.md", local), LinkTarget::Blocked(_)));
        assert!(matches!(resolve_link("//evil/share/a.md", local), LinkTarget::Blocked(_)));
        assert!(!is_relative_name(r"\\evil\share\a.png"));
        assert!(!is_relative_name("C:/a.png"));
        assert!(!is_relative_name("/a.png"));
        assert!(is_relative_name("첨부/a.png"));
        assert!(is_relative_name("../a.md"));
        assert_eq!(find_file(r"\\evil\share\a.png", Path::new(r"C:\notes")), None);
    }

    #[test]
    fn fences_in_quotes_and_odd_fences() {
        // A code block inside a callout keeps its %% and [[ ]] as they are.
        let text = "> [!note]\n> ```\n> printf(\"%%d\") [[x]]\n> ```\n> 뒤 [[y]]\n\n다음";
        assert_eq!(
            prepare(text, None),
            "> [!NOTE]\n> ```\n> printf(\"%%d\") [[x]]\n> ```\n> 뒤 [y](<wiki:y>)\n\n다음"
        );
        // Leaving the quote ends a code block opened in it.
        assert_eq!(prepare("> ```\n> a\n\n[[b]]", None), "> ```\n> a\n\n[b](<wiki:b>)");
        // ```code``` at the start of a line is inline code, not a fence.
        assert_eq!(prepare("```a``` [[b]]\n[[c]]", None), "```a``` [b](<wiki:b>)\n[c](<wiki:c>)");
        // Backticks in the front matter cannot close its code block early.
        assert_eq!(prepare("---\na: ```\n---\n[[b]]", None), "````\na: ```\n````\n\n[b](<wiki:b>)");
    }

    #[test]
    fn table_aliases_and_callout_titles() {
        assert_eq!(prepare("| [[노트\\|별칭]] |", None), "| [별칭](<wiki:노트>) |");
        assert_eq!(
            prepare("> [!tip] 제목 %%숨김\n> 본문%% 보임\n끝", None),
            "> [!TIP]\n> **제목**\n>\n 보임\n끝"
        );
    }

    #[test]
    fn finds_notes_in_vault() {
        let root = std::env::temp_dir().join(format!("mome-md-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join(".obsidian")).unwrap();
        std::fs::create_dir_all(root.join("일기/2026")).unwrap();
        std::fs::create_dir_all(root.join("글")).unwrap();
        std::fs::write(root.join("일기/2026/오늘.md"), "x").unwrap();
        std::fs::write(root.join("글/그림.png"), "x").unwrap();
        let from = root.join("글");
        assert_eq!(find_file("오늘", &from), Some(root.join("일기/2026/오늘.md")));
        assert_eq!(find_file("일기/2026/오늘", &from), Some(root.join("일기/2026/오늘.md")));
        assert_eq!(find_file("그림.png", &from), Some(root.join("글/그림.png")));
        assert_eq!(find_file("없는 노트", &from), None);
        assert_eq!(
            resolve_link("../일기/2026/%EC%98%A4%EB%8A%98.md", Some(&from)),
            LinkTarget::File(from.join("../일기/2026/오늘.md"))
        );
        let _ = std::fs::remove_dir_all(&root);
    }
}
