//! Dockerfile syntax colors for the text area.
//!
//! Lines mean what BuildKit's parser makes of them: `#` starts a comment only as the
//! first thing on a line (also between the lines of a continued instruction), parser
//! directives come before everything else and can change the escape character, and
//! here-documents run to their terminator whatever they contain.

use std::collections::VecDeque;
use std::ops::Range;
use std::path::Path;

/// Bigger files stay plain: the colors are worked out for every layout.
const MAX_TEXT_LEN: usize = 256 * 1024;
/// How far a `${` looks for its `}`, so a long line of unclosed ones stays quick.
const MAX_BRACED_VARIABLE: usize = 256;

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Kind {
    Comment,
    /// Instructions, the words that act like them (`AS`), here-document markers.
    Keyword,
    /// `--from=` style flags and parser directives.
    Flag,
    /// Strings and here-document bodies.
    Quoted,
    Variable,
}

#[derive(Debug, PartialEq)]
pub struct Span {
    pub range: Range<usize>,
    pub kind: Kind,
}

/// Whether `path` names a Dockerfile: `Dockerfile`, `Dockerfile.dev`, `web.dockerfile`,
/// or the same with Podman's `Containerfile`.
pub fn is_dockerfile(path: &Path) -> bool {
    let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
        return false;
    };
    let name = name.to_ascii_lowercase();
    // Formats of their own that merely start with the name, such as an ignore list
    // (`Dockerfile.dockerignore`) or a document about Dockerfiles (`Dockerfile.md`).
    let ext = name.rsplit_once('.').map(|(_, ext)| ext);
    if matches!(
        ext,
        Some("dockerignore" | "md" | "markdown" | "json" | "canvas" | "log" | "csv" | "ini" | "xml" | "yaml" | "yml")
    ) {
        return false;
    }
    ["dockerfile", "containerfile"].iter().any(|base| {
        name.strip_prefix(base).is_some_and(|rest| rest.is_empty() || rest.starts_with('.'))
            || name.strip_suffix(base).is_some_and(|rest| rest.ends_with('.'))
    })
}

/// The colored parts of `text`, ascending and not overlapping.
pub fn spans(text: &str) -> Vec<Span> {
    if text.len() > MAX_TEXT_LEN {
        return Vec::new();
    }
    let mut scanner = Scanner {
        text,
        spans: Vec::new(),
        escape: b'\\',
        directives_possible: true,
        continued: None,
        heredocs: VecDeque::new(),
    };
    let mut start = 0;
    for line in text.split('\n') {
        let end = start + line.len();
        scanner.line(start..end);
        start = end + 1;
    }
    scanner.spans
}

/// How the arguments of an instruction are read.
#[derive(Clone, Copy)]
enum Shape {
    From,
    Healthcheck,
    Onbuild,
    /// RUN, COPY and ADD, which can take here-documents.
    Heredocs,
    Plain,
    /// MAINTAINER: free text, in which an apostrophe opens nothing.
    Raw,
    /// Not an instruction: left uncolored, so a typo shows.
    Unknown,
}

fn shape_of(word: &str) -> Option<Shape> {
    Some(match word.to_ascii_uppercase().as_str() {
        "FROM" => Shape::From,
        "HEALTHCHECK" => Shape::Healthcheck,
        "ONBUILD" => Shape::Onbuild,
        "RUN" | "COPY" | "ADD" => Shape::Heredocs,
        "MAINTAINER" => Shape::Raw,
        "CMD" | "LABEL" | "EXPOSE" | "ENV" | "ENTRYPOINT" | "VOLUME" | "USER" | "WORKDIR" | "ARG" | "STOPSIGNAL"
        | "SHELL" => Shape::Plain,
        _ => return None,
    })
}

/// An instruction being read, possibly over several lines.
struct Instruction {
    shape: Shape,
    /// No argument has been seen yet, so a `--word` is still a flag.
    in_flags: bool,
    /// Arguments seen after the flags.
    words: usize,
    /// The quote character of a string that is still open.
    quote: Option<u8>,
}

impl Instruction {
    fn new(shape: Shape) -> Self {
        Self {
            shape,
            in_flags: true,
            words: 0,
            quote: None,
        }
    }
}

struct Heredoc {
    name: String,
    /// `<<-`: tabs before the terminator don't count.
    chomp: bool,
}

/// A word such as `<<EOF`, `<<-"EOF"`, `<<\EOF` or `3<<EOF`; `<<<` is not one.
fn heredoc(word: &str) -> Option<Heredoc> {
    let rest = word.trim_start_matches(|c: char| c.is_ascii_digit()).strip_prefix("<<")?;
    let (chomp, rest) = rest.strip_prefix('-').map_or((false, rest), |rest| (true, rest));
    if rest.contains('<') {
        return None;
    }
    // The name as the shell reads it, without its quoting.
    let mut name = String::new();
    let mut chars = rest.chars();
    while let Some(c) = chars.next() {
        match c {
            '"' | '\'' => {}
            '\\' => name.extend(chars.next()),
            c => name.push(c),
        }
    }
    (!name.is_empty()).then_some(Heredoc { name, chomp })
}

/// Name and value of a parser directive such as `# escape=\``.
fn directive(line: &str) -> Option<(&str, &str)> {
    let (name, value) = line.strip_prefix('#')?.split_once('=')?;
    let (name, value) = (name.trim(), value.trim());
    let known = ["syntax", "escape", "check"].iter().any(|k| name.eq_ignore_ascii_case(k));
    (known && !value.is_empty()).then_some((name, value))
}

/// End of the `$NAME` or `${…}` at `at`, if that is what is there.
fn variable_end(bytes: &[u8], at: usize) -> Option<usize> {
    let starts_name = |c: u8| c.is_ascii_alphabetic() || c == b'_';
    match *bytes.get(at + 1)? {
        // The shell's process ID: what follows is not a name.
        b'$' => Some(at + 2),
        b'{' => {
            let mut depth = 0;
            for (i, &c) in bytes.iter().enumerate().skip(at + 1).take(MAX_BRACED_VARIABLE) {
                match c {
                    b'{' => depth += 1,
                    b'}' => {
                        depth -= 1;
                        if depth == 0 {
                            return Some(i + 1);
                        }
                    }
                    _ => {}
                }
            }
            None
        }
        c if starts_name(c) => {
            let tail = bytes[at + 1..].iter().take_while(|&&c| starts_name(c) || c.is_ascii_digit());
            Some(at + 1 + tail.count())
        }
        _ => None,
    }
}

struct Scanner<'a> {
    text: &'a str,
    spans: Vec<Span>,
    /// Escapes a character and continues a line: `\`, or `` ` `` after `# escape=`.
    escape: u8,
    directives_possible: bool,
    /// The instruction whose last line ended in the escape character.
    continued: Option<Instruction>,
    /// Here-documents announced by the instruction, first one first.
    heredocs: VecDeque<Heredoc>,
}

impl Scanner<'_> {
    fn push(&mut self, range: Range<usize>, kind: Kind) {
        if !range.is_empty() {
            self.spans.push(Span { range, kind });
        }
    }

    fn line(&mut self, range: Range<usize>) {
        let text = self.text;
        let line = &text[range.clone()];
        if self.continued.is_none() {
            if let Some(doc) = self.heredocs.front() {
                let candidate = if doc.chomp { line.trim_start_matches('\t') } else { line };
                if candidate.trim_end_matches('\r') == doc.name {
                    self.heredocs.pop_front();
                    self.push(range, Kind::Keyword);
                } else {
                    self.push(range, Kind::Quoted);
                }
                return;
            }
        }

        let content = line.trim_start();
        let from = range.end - content.len();
        if content.starts_with('#') {
            if self.directives_possible {
                if let Some((name, value)) = directive(content) {
                    if name.eq_ignore_ascii_case("escape") && value == "`" {
                        self.escape = b'`';
                    }
                    self.push(from..range.end, Kind::Flag);
                    return;
                }
            }
            self.directives_possible = false;
            self.push(from..range.end, Kind::Comment);
            return;
        }
        self.directives_possible = false;
        if content.is_empty() {
            return; // Blank lines don't end a continued instruction.
        }

        let trimmed = content.trim_end_matches([' ', '\t', '\r']);
        let bytes = trimmed.as_bytes();
        // An escaped escape character at the end is not a continuation.
        let continues = bytes.last() == Some(&self.escape) && bytes.iter().rev().nth(1) != Some(&self.escape);
        let end = from + trimmed.len() - usize::from(continues);

        let (mut instruction, at) = match self.continued.take() {
            Some(instruction) => (instruction, from),
            // Only the escape character: the instruction starts on the next line.
            None if from == end => return,
            None => self.head(from, end),
        };
        if !matches!(instruction.shape, Shape::Unknown | Shape::Raw) {
            self.arguments(&mut instruction, at, end);
        }
        if continues {
            self.continued = Some(instruction);
        }
    }

    /// End of the word at `at`, in which quotes can hold spaces.
    fn word_end(&self, at: usize, end: usize) -> usize {
        let bytes = self.text.as_bytes();
        let mut quote = None;
        let mut i = at;
        while i < end {
            let c = bytes[i];
            if quote.is_none() && c.is_ascii_whitespace() {
                return i;
            }
            if quote == Some(c) {
                quote = None;
            } else if quote.is_none() && (c == b'"' || c == b'\'') {
                quote = Some(c);
            } else if c == self.escape && quote != Some(b'\'') {
                i += 1;
            }
            i += 1;
        }
        end
    }

    /// Reads the instruction's name at `from`. Returns it and where its arguments start.
    fn head(&mut self, from: usize, end: usize) -> (Instruction, usize) {
        let word_end = self.word_end(from, end);
        let shape = shape_of(&self.text[from..word_end]);
        if shape.is_some() {
            self.push(from..word_end, Kind::Keyword);
        }
        (Instruction::new(shape.unwrap_or(Shape::Unknown)), word_end)
    }

    fn arguments(&mut self, instruction: &mut Instruction, mut at: usize, end: usize) {
        let bytes = self.text.as_bytes();
        let mut word_start = true;
        while at < end {
            if instruction.quote.is_some() {
                at = self.quoted(instruction, at, at, end);
                word_start = false;
                continue;
            }
            let c = bytes[at];
            if c.is_ascii_whitespace() {
                word_start = true;
                at += 1;
                continue;
            }
            if std::mem::take(&mut word_start) {
                let next = self.word(instruction, at, end);
                if next > at {
                    at = next;
                    continue;
                }
            }
            if c == self.escape {
                at += 2;
            } else if c == b'"' || c == b'\'' {
                instruction.quote = Some(c);
                at = self.quoted(instruction, at, at + 1, end);
            } else if let Some(stop) = (c == b'$').then(|| variable_end(&bytes[..end], at)).flatten() {
                self.push(at..stop, Kind::Variable);
                at = stop;
            } else {
                at += 1;
            }
        }
    }

    /// Handles what only means something as a whole word at `at`. Returns where to
    /// go on from, which is `at` if there was nothing of the kind.
    fn word(&mut self, instruction: &mut Instruction, at: usize, end: usize) -> usize {
        let word_end = self.word_end(at, end);
        let word = &self.text[at..word_end];
        if instruction.in_flags && word.starts_with("--") {
            let stop = word.find('=').map_or(word_end, |eq| at + eq + 1);
            self.push(at..stop, Kind::Flag);
            // A bare `--` ends the flags: what follows belongs to the command.
            instruction.in_flags = word != "--";
            return stop; // The value is read like any other text.
        }
        instruction.in_flags = false;
        let index = instruction.words;
        instruction.words += 1;
        match instruction.shape {
            Shape::From if index == 1 && word.eq_ignore_ascii_case("AS") => {}
            Shape::Healthcheck
                if index == 0 && (word.eq_ignore_ascii_case("CMD") || word.eq_ignore_ascii_case("NONE")) => {}
            Shape::Onbuild if index == 0 => match shape_of(word) {
                Some(shape) => *instruction = Instruction::new(shape),
                None => return at,
            },
            Shape::Heredocs => match heredoc(word) {
                Some(doc) => self.heredocs.push_back(doc),
                None => return at,
            },
            _ => return at,
        }
        self.push(at..word_end, Kind::Keyword);
        word_end
    }

    /// Reads the open string from `scan` to its closing quote or `end`, whichever
    /// comes first, coloring from `from`. Returns where it stopped.
    fn quoted(&mut self, instruction: &mut Instruction, from: usize, scan: usize, end: usize) -> usize {
        let Some(quote) = instruction.quote else {
            return scan;
        };
        let bytes = self.text.as_bytes();
        let mut piece = from;
        let mut i = scan;
        while i < end {
            let c = bytes[i];
            if c == quote {
                self.push(piece..i + 1, Kind::Quoted);
                instruction.quote = None;
                return i + 1;
            }
            // Nothing is special inside single quotes.
            if quote == b'"' && c == self.escape {
                i += 2;
                continue;
            }
            if quote == b'"' && c == b'$' {
                if let Some(stop) = variable_end(&bytes[..end], i) {
                    self.push(piece..i, Kind::Quoted);
                    self.push(i..stop, Kind::Variable);
                    piece = stop;
                    i = stop;
                    continue;
                }
            }
            i += 1;
        }
        self.push(piece..end, Kind::Quoted);
        end
    }
}

#[cfg(test)]
mod tests {
    use super::Kind::*;
    use super::*;

    fn parts(text: &str) -> Vec<(&str, Kind)> {
        spans(text).into_iter().map(|s| (&text[s.range], s.kind)).collect()
    }

    #[test]
    fn recognises_file_names() {
        for name in [
            "Dockerfile",
            "dockerfile",
            "DOCKERFILE",
            "Dockerfile.dev",
            "Dockerfile.bak",
            "Dockerfile.txt",
            "web.Dockerfile",
            "Containerfile",
            "Containerfile.prod",
            "api.containerfile",
        ] {
            assert!(is_dockerfile(Path::new(name)), "{name}");
            assert!(is_dockerfile(&Path::new(r"C:\프로젝트\앱").join(name)), "{name}");
        }
        for name in [
            "Dockerfile.dockerignore",
            "Dockerfile.md",
            "Dockerfile.json",
            "Containerfile.yml",
            "Dockerfile.LOG",
            ".dockerignore",
            "mydockerfile",
            "Dockerfiles",
            "dockerfile-notes.txt",
            "memo.txt",
        ] {
            assert!(!is_dockerfile(Path::new(name)), "{name}");
        }
    }

    #[test]
    fn comments_are_whole_lines() {
        assert_eq!(parts("# 베이스 이미지"), [("# 베이스 이미지", Comment)]);
        assert_eq!(parts("FROM a\n    # 들여쓴 주석\n"), [("FROM", Keyword), ("# 들여쓴 주석", Comment)]);
        // Anywhere else `#` is an argument (for the shell to deal with).
        assert_eq!(parts("RUN echo a # b"), [("RUN", Keyword)]);
    }

    #[test]
    fn instructions_in_any_case() {
        assert_eq!(parts("from a"), [("from", Keyword)]);
        assert_eq!(parts("  WorkDir /app"), [("WorkDir", Keyword)]);
        assert_eq!(parts("FROMAGE x"), []);
        assert_eq!(parts("ONBUILD RUN make"), [("ONBUILD", Keyword), ("RUN", Keyword)]);
    }

    #[test]
    fn stage_names_and_healthcheck() {
        assert_eq!(parts("FROM node:20 as build"), [("FROM", Keyword), ("as", Keyword)]);
        // An image that happens to be called "as".
        assert_eq!(parts("FROM as"), [("FROM", Keyword)]);
        assert_eq!(
            parts("HEALTHCHECK --interval=5s CMD curl cmd"),
            [("HEALTHCHECK", Keyword), ("--interval=", Flag), ("CMD", Keyword)]
        );
        assert_eq!(parts("HEALTHCHECK NONE"), [("HEALTHCHECK", Keyword), ("NONE", Keyword)]);
    }

    #[test]
    fn flags_only_before_the_arguments() {
        assert_eq!(
            parts("COPY --from=build --link a --b"),
            [("COPY", Keyword), ("--from=", Flag), ("--link", Flag)]
        );
        assert_eq!(
            parts("RUN --mount=type=cache,target=$DIR make"),
            [("RUN", Keyword), ("--mount=", Flag), ("$DIR", Variable)]
        );
        assert_eq!(
            parts("RUN --network=none -- --version --x"),
            [("RUN", Keyword), ("--network=", Flag), ("--", Flag)]
        );
    }

    #[test]
    fn strings_and_variables() {
        assert_eq!(
            parts(r#"CMD ["node", "a b"]"#),
            [("CMD", Keyword), (r#""node""#, Quoted), (r#""a b""#, Quoted)]
        );
        assert_eq!(
            parts(r#"ENV A="x $B y" C='$D' E=${F:-${G}} H=\$I"#),
            [
                ("ENV", Keyword),
                (r#""x "#, Quoted),
                ("$B", Variable),
                (r#" y""#, Quoted),
                ("'$D'", Quoted),
                ("${F:-${G}}", Variable),
            ]
        );
        assert_eq!(parts(r#"ENV A="a\"b""#), [("ENV", Keyword), (r#""a\"b""#, Quoted)]);
        assert_eq!(parts("RUN echo $1 $(date)"), [("RUN", Keyword)]);
        // `$$` is the process ID, followed by the plain text VAR.
        assert_eq!(parts("RUN echo $$VAR"), [("RUN", Keyword), ("$$", Variable)]);
        // Free text: the apostrophe opens no string.
        assert_eq!(parts("MAINTAINER Conan O'Brien"), [("MAINTAINER", Keyword)]);
        // Never closed on this line.
        assert_eq!(parts("ENV A=${B"), [("ENV", Keyword)]);
        assert_eq!(parts(&format!("ENV {}", "${".repeat(100_000))), [("ENV", Keyword)]);
    }

    #[test]
    fn continued_lines_are_arguments() {
        assert_eq!(
            parts("RUN a \\\n  # 설명\n\n  from b \\  \n  c\nFROM d"),
            [("RUN", Keyword), ("# 설명", Comment), ("FROM", Keyword)]
        );
        // The flags go on over the line break, and a string can too.
        assert_eq!(
            parts("COPY --a \\\n --b x\nLABEL t=\"a \\\n# c\n b\" d"),
            [
                ("COPY", Keyword),
                ("--a", Flag),
                ("--b", Flag),
                ("LABEL", Keyword),
                ("\"a ", Quoted),
                ("# c", Comment),
                ("b\"", Quoted),
            ]
        );
        // An escaped backslash at the end does not continue the line.
        assert_eq!(parts("RUN a\\\\\nFROM b"), [("RUN", Keyword), ("FROM", Keyword)]);
        assert_eq!(parts("RUN\\\n a"), [("RUN", Keyword)]);
        // A line of nothing but the escape character joins the next one to it.
        assert_eq!(parts("\\\nFROM a"), [("FROM", Keyword)]);
    }

    #[test]
    fn parser_directives_only_at_the_top() {
        assert_eq!(
            parts("# syntax=docker/dockerfile:1\n#check = skip=all\n# 설명\n# escape=`"),
            [
                ("# syntax=docker/dockerfile:1", Flag),
                ("#check = skip=all", Flag),
                ("# 설명", Comment),
                ("# escape=`", Comment),
            ]
        );
        assert_eq!(parts("\n# syntax=a"), [("# syntax=a", Comment)]);
        assert_eq!(parts("# unknown=a\n# syntax=b"), [("# unknown=a", Comment), ("# syntax=b", Comment)]);
    }

    #[test]
    fn escape_directive_changes_continuation() {
        assert_eq!(
            parts("# escape=`\nRUN a `\n  from b\nWORKDIR c:\\\nRUN d"),
            [("# escape=`", Flag), ("RUN", Keyword), ("WORKDIR", Keyword), ("RUN", Keyword)]
        );
        // Without the directive the backslash would swallow the next RUN.
        assert_eq!(parts("WORKDIR c:\\\nRUN d"), [("WORKDIR", Keyword)]);
    }

    #[test]
    fn heredocs_run_to_their_terminator() {
        assert_eq!(
            parts("RUN <<EOF\n# 셸 주석\nFROM x\nEOF\nFROM y"),
            [
                ("RUN", Keyword),
                ("<<EOF", Keyword),
                ("# 셸 주석", Quoted),
                ("FROM x", Quoted),
                ("EOF", Keyword),
                ("FROM", Keyword),
            ]
        );
        assert_eq!(
            parts("COPY <<-\"A\" <<B /x\n\tA\nb\nB\nCMD c"),
            [
                ("COPY", Keyword),
                ("<<-\"A\"", Keyword),
                ("<<B", Keyword),
                ("\tA", Keyword),
                ("b", Quoted),
                ("B", Keyword),
                ("CMD", Keyword),
            ]
        );
        // The body starts after the whole instruction.
        assert_eq!(
            parts("RUN cat <<E \\\n  a\nb\nE"),
            [("RUN", Keyword), ("<<E", Keyword), ("b", Quoted), ("E", Keyword)]
        );
        assert_eq!(parts("ONBUILD RUN <<E\na\nE").len(), 5);
        // The name is what the shell makes of it.
        assert_eq!(
            parts("RUN <<\\EOF\necho a\nEOF\nFROM b"),
            [
                ("RUN", Keyword),
                ("<<\\EOF", Keyword),
                ("echo a", Quoted),
                ("EOF", Keyword),
                ("FROM", Keyword),
            ]
        );
        assert_eq!(
            parts("COPY <<\"A B\" /x\n본문\nA B\nCMD c"),
            [
                ("COPY", Keyword),
                ("<<\"A B\"", Keyword),
                ("본문", Quoted),
                ("A B", Keyword),
                ("CMD", Keyword),
            ]
        );
        // Not here-documents.
        assert_eq!(parts("RUN cat <<<a\nFROM b"), [("RUN", Keyword), ("FROM", Keyword)]);
        assert_eq!(parts("CMD <<E\nFROM b"), [("CMD", Keyword), ("FROM", Keyword)]);
    }

    #[test]
    fn crlf_changes_nothing() {
        assert_eq!(
            parts("RUN a \\\r\n  from\r\nFROM b\r\n"),
            [("RUN", Keyword), ("FROM", Keyword)]
        );
    }

    #[test]
    fn too_big_stays_plain() {
        assert_eq!(parts(&"# c\n".repeat(MAX_TEXT_LEN)), []);
    }

    /// Text made of awkward pieces in pseudo-random order must give clean spans too.
    #[test]
    fn random_text_gives_clean_spans() {
        const PIECES: &[&str] = &[
            "\n", "\r\n", " ", "\t", "\\", "\\\n", "`", "`\n", "\"", "'", "$", "${", "}", "$가", "#", "# c", "=", "--",
            "--a=", "<<", "<<-", "<<E", "<<-\"E\"", "E", "\tE", "3<<E", "<<<", "FROM", "AS", "RUN", "COPY", "ONBUILD",
            "HEALTHCHECK", "CMD", "NONE", "# escape=`", "# syntax=x", "가", "\u{1F433}", "\u{3000}", "é",
        ];
        let mut seed = 0x9E37_79B9_7F4A_7C15_u64;
        let mut next = move || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed as usize
        };
        for _ in 0..5_000 {
            let text: String = (0..next() % 40).map(|_| PIECES[next() % PIECES.len()]).collect();
            let mut last = 0;
            for span in spans(&text) {
                assert!(span.range.start >= last && span.range.end > span.range.start, "{text:?}");
                assert!(text.get(span.range.clone()).is_some(), "{text:?}");
                last = span.range.end;
            }
        }
    }

    /// Whatever has been typed so far, ranges must be valid to slice with.
    #[test]
    fn every_prefix_gives_clean_spans() {
        let text = concat!(
            "# syntax=docker/dockerfile:1\n# 빌드 단계 \u{1F433}\nFROM --platform=$P node:20 AS 빌드\n",
            "ENV 이름=\"값 ${가:-나} 끝\" B='다\\\n# 줄 사이 주석\n라' C=$가\n",
            "COPY --from=빌드 <<-끝 \"경로 이름\" /앱/\n\t본문\n\t끝\nRUN \"\\가\" \\가 $\n\nCMD [\"a\\",
        );
        for (cut, _) in text.char_indices() {
            let prefix = &text[..cut];
            let mut last = 0;
            for span in spans(prefix) {
                assert!(span.range.start >= last && span.range.end > span.range.start, "{prefix:?}");
                assert!(prefix.is_char_boundary(span.range.start), "{prefix:?}");
                assert!(prefix.is_char_boundary(span.range.end), "{prefix:?}");
                last = span.range.end;
            }
            assert!(last <= cut, "{prefix:?}");
        }
    }
}
