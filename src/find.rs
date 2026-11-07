//! Searching the document: plain text or regular expressions, and replacing with
//! IntelliJ-style group references (`$1`, `${name}`) and case escapes.

use std::cell::RefCell;
use std::ops::Range;
use std::rc::Rc;

use fancy_regex::{Captures, Regex, RegexBuilder};

/// Searching stops after this many matches; enough for highlighting and counting.
pub const MAX_MATCHES: usize = 100_000;
/// The case-insensitive searches stop after this many candidates. The whole-word
/// filter may drop some of them; [`MAX_MATCHES`] is applied after it.
const MAX_CANDIDATES: usize = MAX_MATCHES * 2;
/// Least backtracking steps a regular expression may take in one search; past its
/// budget the search stops with an error instead of freezing the window.
const BACKTRACK_LIMIT: usize = 1_000_000;

/// The backtracking budget for a text of `len` bytes. fancy-regex counts the steps
/// of one whole search, including the start positions it tries and gives up on,
/// so a long stretch without a match needs a budget that grows with the text.
/// Rounded up so that typing does not rebuild the expression.
fn backtrack_budget(len: usize) -> usize {
    len.saturating_mul(4)
        .max(BACKTRACK_LIMIT)
        .checked_next_power_of_two()
        .unwrap_or(usize::MAX)
}

fn build_regex(query: &str, opts: FindOptions, budget: usize) -> Result<Regex, fancy_regex::Error> {
    RegexBuilder::new(query)
        .case_insensitive(!opts.match_case)
        .multi_line(true)
        .backtrack_limit(budget)
        .build()
}

/// Does the pattern use `\K` (keep out)? It moves the start of a match back
/// before where the search began, which breaks stepping through the matches; Java
/// (and so IntelliJ) has no `\K` either.
fn uses_keep_out(pattern: &str) -> bool {
    let mut chars = pattern.chars();
    while let Some(c) = chars.next() {
        if c == '\\' && chars.next() == Some('K') {
            return true;
        }
    }
    false
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct FindOptions {
    pub match_case: bool,
    pub whole_word: bool,
    /// The query is a regular expression; `^` and `$` match at line breaks.
    pub regex: bool,
}

/// A query ready to search with: plain text, or a compiled regular expression.
pub struct Search {
    query: String,
    opts: FindOptions,
    /// Compiled with the smallest backtracking budget (which also checks it).
    regex: Option<Rc<Regex>>,
    /// The same, compiled with the budget for the last text size searched.
    sized: RefCell<Option<(usize, Rc<Regex>)>>,
}

impl Search {
    /// Fails with a message for the user if the regular expression is invalid.
    pub fn new(query: &str, opts: FindOptions) -> Result<Self, String> {
        let regex = if opts.regex && !query.is_empty() {
            if uses_keep_out(query) {
                return Err("정규식 오류: \\K는 쓸 수 없습니다. 앞 글자를 조건으로 삼으려면 (?<=…)를 쓰세요.".to_owned());
            }
            let built = build_regex(query, opts, backtrack_budget(0));
            Some(Rc::new(built.map_err(|e| regex_error(query, &e))?))
        } else {
            None
        };
        Ok(Self {
            query: query.to_owned(),
            opts,
            regex,
            sized: RefCell::new(None),
        })
    }

    /// The regular expression with a backtracking budget for a text of `len` bytes.
    fn regex_for(&self, len: usize) -> Option<Rc<Regex>> {
        let base = self.regex.as_ref()?;
        let budget = backtrack_budget(len);
        if budget == backtrack_budget(0) {
            return Some(base.clone());
        }
        let mut sized = self.sized.borrow_mut();
        if let Some((b, re)) = sized.as_ref() {
            if *b == budget {
                return Some(re.clone());
            }
        }
        // It compiled once already; should it fail now, the smaller budget will do.
        let re = build_regex(&self.query, self.opts, budget)
            .map(Rc::new)
            .unwrap_or_else(|_| base.clone());
        *sized = Some((budget, re.clone()));
        Some(re)
    }

    /// Matches in order, at most [`MAX_MATCHES`] (for highlighting and counting).
    pub fn find_all(&self, text: &str) -> Result<Vec<Match>, String> {
        match self.regex_for(text.len()) {
            Some(re) => regex_matches(&re, text, self.opts.whole_word, MAX_MATCHES),
            None => Ok(find_all(text, &self.query, self.opts)),
        }
    }

    /// Every match, however many (for "모두 바꾸기").
    pub fn find_every(&self, text: &str) -> Result<Vec<Match>, String> {
        match self.regex_for(text.len()) {
            Some(re) => regex_matches(&re, text, self.opts.whole_word, usize::MAX),
            None => Ok(find_every(text, &self.query, self.opts)),
        }
    }

    /// Checks a replacement before any match is replaced: with a regular
    /// expression its group references must exist.
    pub fn check_replacement(&self, template: &str) -> Result<(), String> {
        match &self.regex {
            Some(re) => Template::parse(template, re).map(|_| ()),
            None => Ok(()),
        }
    }

    /// What the match `m` in `text` becomes. With a regular expression `$1`,
    /// `${name}`, `\n`, `\t` and the case escapes `\u \l \U \L \E` are filled in;
    /// otherwise the replacement is taken as it is.
    pub fn replacement_for(&self, text: &str, m: &Match, template: &str) -> Result<String, String> {
        let (Some(base), Some(re)) = (&self.regex, self.regex_for(text.len())) else {
            return Ok(template.to_owned());
        };
        let template = Template::parse(template, base)?;
        let mut out = String::new();
        template.render_into(&captures_at(&re, text, m)?, &mut out);
        Ok(out)
    }

    /// Replaces `matches` (this search's, in order) in `text`. Returns the new text
    /// and where each replacement ended up in it (byte ranges).
    pub fn replace_all(
        &self,
        text: &str,
        matches: &[Match],
        template: &str,
    ) -> Result<(String, Vec<Range<usize>>), String> {
        let parsed = match (&self.regex, self.regex_for(text.len())) {
            (Some(base), Some(re)) => Some((re, Template::parse(template, base)?)),
            _ => None,
        };
        let mut out = String::with_capacity(text.len());
        let mut spans = Vec::with_capacity(matches.len());
        let mut last = 0;
        for m in matches {
            out.push_str(&text[last..m.bytes.start]);
            let start = out.len();
            match &parsed {
                Some((re, template)) => template.render_into(&captures_at(re, text, m)?, &mut out),
                None => out.push_str(template),
            }
            spans.push(start..out.len());
            last = m.bytes.end;
        }
        out.push_str(&text[last..]);
        Ok((out, spans))
    }
}

/// The byte after the character at `i` (past the end when `i` is the end).
fn next_char(text: &str, i: usize) -> usize {
    text[i..].chars().next().map_or(text.len() + 1, |c| i + c.len_utf8())
}

/// Matches of a regular expression, with character positions (at most `limit`).
/// Steps through the text like Java's `Matcher.find`: the next search starts where
/// a match ended, or one character on after an empty match (so `b*` in "abc" also
/// matches the empty string right after "b"). A candidate that is not a whole word
/// does not hide a word starting inside it.
fn regex_matches(re: &Regex, text: &str, whole_word: bool, limit: usize) -> Result<Vec<Match>, String> {
    let mut out = Vec::new();
    let mut char_pos = 0;
    let mut byte_pos = 0;
    let mut from = 0;
    while from <= text.len() && out.len() < limit {
        let found = re.find_from_pos(text, from).map_err(|e| regex_error("", &e))?;
        let Some(found) = found else {
            break;
        };
        let bytes = found.start()..found.end();
        if bytes.start < from || bytes.end < bytes.start {
            // Cannot happen without \K (refused); never loop over the same text.
            return Err("정규식이 찾은 곳을 다시 찾아 찾기를 멈췄습니다.".to_owned());
        }
        if whole_word && !is_whole_word(text, &bytes) {
            from = next_char(text, bytes.start);
            continue;
        }
        char_pos += text[byte_pos..bytes.start].chars().count();
        let len = text[bytes.clone()].chars().count();
        out.push(Match {
            chars: char_pos..char_pos + len,
            bytes: bytes.clone(),
            searched_from: from,
        });
        char_pos += len;
        byte_pos = bytes.end;
        from = if bytes.is_empty() { next_char(text, bytes.end) } else { bytes.end };
    }
    Ok(out)
}

/// The groups of the match `m`, found again by the same search that found it
/// (look-behind, `^` and `\G` see what they saw then).
fn captures_at<'t>(re: &Regex, text: &'t str, m: &Match) -> Result<Captures<'t, str>, String> {
    if m.searched_from > m.bytes.start || !text.is_char_boundary(m.searched_from) {
        return Err("찾은 내용이 바뀌었습니다. 다시 찾아 주세요.".to_owned());
    }
    let found = re
        .captures_from_pos(text, m.searched_from)
        .map_err(|e| regex_error("", &e))?;
    match found {
        Some(caps) if caps.get(0).is_some_and(|g| g.start() == m.bytes.start && g.end() == m.bytes.end) => Ok(caps),
        _ => Err("찾은 내용이 바뀌었습니다. 다시 찾아 주세요.".to_owned()),
    }
}

/// A user-facing message for a regular expression that cannot be used.
fn regex_error(pattern: &str, e: &fancy_regex::Error) -> String {
    use fancy_regex::{CompileError as C, Error, ParseError as P};
    let message = match e {
        Error::ParseError(pos, kind) => {
            // Some errors point just past the end; no position is shown then.
            let at = pattern
                .get(..*pos)
                .filter(|_| *pos < pattern.len())
                .map(|s| s.chars().count() + 1);
            let what = match kind {
                P::UnclosedOpenParen => "여는 괄호 ( 를 닫지 않았습니다".to_owned(),
                P::InvalidRepeat | P::TargetNotRepeatable => "반복 기호(* + ? {})를 쓸 수 없는 자리입니다".to_owned(),
                P::TrailingBackslash => "\\ 뒤에 글자가 없습니다".to_owned(),
                P::InvalidEscape(s) => format!("알 수 없는 이스케이프 {s}"),
                P::InvalidClass => "[ ] 문자 집합이 잘못되었습니다".to_owned(),
                P::InvalidBackref => "역참조가 잘못되었습니다".to_owned(),
                P::InvalidGroupName | P::InvalidGroupNameBackref(_) => "그룹 이름이 잘못되었습니다".to_owned(),
                P::UnknownFlag(s) => format!("알 수 없는 플래그 {s}"),
                P::InvalidHex | P::InvalidCodepointValue | P::UnclosedUnicodeName => {
                    "문자 코드가 잘못되었습니다".to_owned()
                }
                P::ChangingUnicodeModeUnsupported => "유니코드 모드(u 플래그)는 끌 수 없습니다".to_owned(),
                P::GeneralParseError(s) if s.contains("end of string not reached") => {
                    "닫는 괄호 ) 에 맞는 여는 괄호가 없습니다".to_owned()
                }
                other => other.to_string(),
            };
            match at {
                Some(at) => format!("정규식 오류 ({at}번째 글자): {what}"),
                None => format!("정규식 오류: {what}"),
            }
        }
        Error::CompileError(kind) => {
            let what = match kind.as_ref() {
                C::InvalidBackref(g) => format!("{g}번 그룹이 없어 역참조할 수 없습니다"),
                C::InvalidGroupName | C::InvalidGroupNameBackref(_) => "그룹 이름이 잘못되었습니다".to_owned(),
                C::LookBehindNotConst | C::VariableLookBehindRequiresFeature => {
                    "뒤쪽 살피기 (?<= ) 안에는 길이가 정해진 식만 쓸 수 있습니다".to_owned()
                }
                C::InnerError(inner) => {
                    if inner.size_limit().is_some() {
                        "식이 너무 커서 만들 수 없습니다. 반복 횟수를 줄여 보세요".to_owned()
                    } else if let Some(syntax) = inner.syntax_error() {
                        syntax_error_text(&syntax.to_string())
                    } else {
                        inner.to_string()
                    }
                }
                other => other.to_string(),
            };
            format!("정규식 오류: {what}")
        }
        Error::RuntimeError(_) => "정규식이 너무 복잡해 찾기를 멈췄습니다. 식을 더 좁혀 보세요.".to_owned(),
        other => format!("정규식 오류: {other}"),
    };
    message.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// The gist of a regex-syntax error, whose text draws the pattern with a caret and
/// ends with "error: …". (Its position counts in a rewritten pattern, so it is left out.)
fn syntax_error_text(full: &str) -> String {
    let what = full
        .lines()
        .rev()
        .find_map(|line| line.trim().strip_prefix("error: "))
        .unwrap_or(full);
    if what.contains("class range") {
        "[ ] 안의 범위가 거꾸로입니다 (예: z-a)".to_owned()
    } else if what.contains("Unicode property") || what.contains("unicode property") {
        "알 수 없는 유니코드 속성입니다".to_owned()
    } else {
        what.to_owned()
    }
}

/// A replacement for a regular expression: text, group references and case changes.
struct Template {
    pieces: Vec<Piece>,
}

enum Piece {
    Text(String),
    /// A group; a name may stand for several groups (`(?<y>…)|(?<y>…)`), and the
    /// first of them that took part in the match is used.
    Group(Vec<usize>),
    Case(Case),
}

#[derive(Clone, Copy)]
enum Case {
    /// `\l`: the next character in lower case.
    NextLower,
    /// `\u`: the next character in upper case.
    NextUpper,
    /// `\L`: lower case until `\E`.
    Lower,
    /// `\U`: upper case until `\E`.
    Upper,
    /// `\E`.
    End,
}

impl Template {
    /// Reads `$n` (Java's rule: more digits are taken while they still name a
    /// group), `${name}` and backslash escapes; any other escaped character is
    /// itself, so `\$` and `\\` give `$` and `\`.
    fn parse(template: &str, re: &Regex) -> Result<Self, String> {
        let names: Vec<Option<&str>> = re.capture_names().collect();
        let groups = names.len();
        let no_group = |n: usize| format!("바꿀 내용의 ${n}: {n}번 그룹이 없습니다.");
        let chars: Vec<char> = template.chars().collect();
        let mut pieces = Vec::new();
        let mut text = String::new();
        fn piece(pieces: &mut Vec<Piece>, text: &mut String, p: Piece) {
            if !text.is_empty() {
                pieces.push(Piece::Text(std::mem::take(text)));
            }
            pieces.push(p);
        }
        let mut i = 0;
        while i < chars.len() {
            let c = chars[i];
            i += 1;
            match c {
                '\\' => {
                    let Some(&next) = chars.get(i) else {
                        return Err("바꿀 내용이 \\ 로 끝납니다. \\ 를 넣으려면 \\\\ 로 쓰세요.".to_owned());
                    };
                    i += 1;
                    let case = match next {
                        'n' => {
                            text.push('\n');
                            None
                        }
                        // The text keeps line breaks as \n; \r\n or a lone \r is one break.
                        'r' => {
                            if chars.get(i) == Some(&'\\') && chars.get(i + 1) == Some(&'n') {
                                i += 2;
                            }
                            text.push('\n');
                            None
                        }
                        't' => {
                            text.push('\t');
                            None
                        }
                        'l' => Some(Case::NextLower),
                        'u' => Some(Case::NextUpper),
                        'L' => Some(Case::Lower),
                        'U' => Some(Case::Upper),
                        'E' => Some(Case::End),
                        other => {
                            text.push(other);
                            None
                        }
                    };
                    if let Some(case) = case {
                        piece(&mut pieces, &mut text, Piece::Case(case));
                    }
                }
                '$' => {
                    let group = match chars.get(i) {
                        Some('{') => {
                            let close = chars[i..]
                                .iter()
                                .position(|&c| c == '}')
                                .ok_or("바꿀 내용의 ${ 를 } 로 닫지 않았습니다.")?;
                            let name: String = chars[i + 1..i + close].iter().collect();
                            i += close + 1;
                            match name.parse::<usize>() {
                                Ok(n) if n < groups => vec![n],
                                Ok(n) => return Err(no_group(n)),
                                Err(_) => {
                                    let ids: Vec<usize> = (0..groups)
                                        .filter(|&i| names[i] == Some(name.as_str()))
                                        .collect();
                                    if ids.is_empty() {
                                        return Err(format!(
                                            "바꿀 내용의 ${{{name}}}: 이름이 '{name}'인 그룹이 없습니다."
                                        ));
                                    }
                                    ids
                                }
                            }
                        }
                        Some(d) if d.is_ascii_digit() => {
                            let mut n = d.to_digit(10).unwrap_or(0) as usize;
                            i += 1;
                            if n >= groups {
                                return Err(no_group(n));
                            }
                            while let Some(d) = chars.get(i).and_then(|c| c.to_digit(10)) {
                                let more = n * 10 + d as usize;
                                if more >= groups {
                                    break;
                                }
                                n = more;
                                i += 1;
                            }
                            vec![n]
                        }
                        _ => {
                            return Err(
                                "바꿀 내용의 $ 뒤에는 그룹 번호나 {이름}이 와야 합니다. $ 를 넣으려면 \\$ 로 쓰세요."
                                    .to_owned(),
                            );
                        }
                    };
                    piece(&mut pieces, &mut text, Piece::Group(group));
                }
                c => text.push(c),
            }
        }
        if !text.is_empty() {
            pieces.push(Piece::Text(text));
        }
        Ok(Self { pieces })
    }

    fn render_into(&self, caps: &Captures<'_, str>, out: &mut String) {
        // Some(true) = upper case, Some(false) = lower case.
        let mut next: Option<bool> = None;
        let mut rest: Option<bool> = None;
        for piece in &self.pieces {
            let s = match piece {
                Piece::Text(s) => s.as_str(),
                Piece::Group(ids) => ids.iter().find_map(|&i| caps.get(i)).map_or("", |m| m.as_str()),
                Piece::Case(case) => {
                    match case {
                        Case::NextLower => next = Some(false),
                        Case::NextUpper => next = Some(true),
                        Case::Lower => rest = Some(false),
                        Case::Upper => rest = Some(true),
                        Case::End => rest = None,
                    }
                    continue;
                }
            };
            for c in s.chars() {
                match next.take().or(rest) {
                    Some(true) => out.extend(c.to_uppercase()),
                    Some(false) => out.extend(c.to_lowercase()),
                    None => out.push(c),
                }
            }
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Match {
    pub chars: Range<usize>,
    pub bytes: Range<usize>,
    /// Byte where the search that found it began. A regular expression's groups
    /// are found again from there, since `\G` and the like depend on it.
    pub searched_from: usize,
}

fn is_word_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// Is `bytes` of `text` not part of a longer word?
fn is_whole_word(text: &str, bytes: &Range<usize>) -> bool {
    let before = text[..bytes.start].chars().next_back();
    let after = text[bytes.end..].chars().next();
    !before.is_some_and(is_word_char) && !after.is_some_and(is_word_char)
}

/// Lower-cases a character without changing the number of characters.
fn fold(c: char) -> char {
    let mut lower = c.to_lowercase();
    match (lower.next(), lower.next()) {
        (Some(l), None) => l,
        _ => c,
    }
}

/// All non-overlapping matches of `query`, in order (at most [`MAX_MATCHES`]).
pub fn find_all(text: &str, query: &str, opts: FindOptions) -> Vec<Match> {
    find_from(text, 0, query, opts).0
}

/// Every match, however many (for "모두 바꾸기").
pub fn find_every(text: &str, query: &str, opts: FindOptions) -> Vec<Match> {
    let mut all = Vec::new();
    let mut from = 0;
    loop {
        let (batch, more) = find_from(text, from, query, opts);
        all.extend(batch);
        match more {
            Some(next) if next > from => from = next,
            _ => return all,
        }
    }
}

/// Like [`find_all`], from byte `from` on; positions still count from the start of
/// `text`, and whole words are judged with the text before `from`. Also returns
/// where to go on if the search stopped at a limit.
fn find_from(text: &str, from: usize, query: &str, opts: FindOptions) -> (Vec<Match>, Option<usize>) {
    if query.is_empty() || from >= text.len() {
        return (Vec::new(), None);
    }
    let rest = &text[from..];
    let byte_matches = if opts.match_case {
        rest.match_indices(query)
            .take(MAX_CANDIDATES)
            .map(|(start, m)| start..start + m.len())
            .collect::<Vec<_>>()
    } else if query.is_ascii() {
        find_ascii_case_insensitive(rest, query)
    } else {
        find_folded(rest, query)
    };
    // The candidate list itself may have been cut short.
    let candidates_cut = byte_matches.len() >= MAX_CANDIDATES;
    let last_candidate = byte_matches.last().map(|b| b.end + from);

    let mut out = Vec::new();
    let mut char_pos = text[..from].chars().count();
    let mut byte_pos = from;
    for bytes in byte_matches {
        let bytes = bytes.start + from..bytes.end + from;
        if opts.whole_word && !is_whole_word(text, &bytes) {
            continue;
        }
        char_pos += text[byte_pos..bytes.start].chars().count();
        let len = text[bytes.clone()].chars().count();
        out.push(Match {
            chars: char_pos..char_pos + len,
            bytes: bytes.clone(),
            searched_from: bytes.start,
        });
        char_pos += len;
        byte_pos = bytes.end;
        if out.len() >= MAX_MATCHES {
            return (out, Some(bytes.end));
        }
    }
    let more = if candidates_cut { last_candidate } else { None };
    (out, more)
}
/// Byte-wise search; an ASCII needle can only match ASCII bytes, so this is exact.
fn find_ascii_case_insensitive(text: &str, query: &str) -> Vec<Range<usize>> {
    let hay = text.as_bytes();
    let needle: Vec<u8> = query.bytes().map(|b| b.to_ascii_lowercase()).collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i + needle.len() <= hay.len() {
        if hay[i].to_ascii_lowercase() == needle[0]
            && hay[i..i + needle.len()]
                .iter()
                .zip(&needle)
                .all(|(a, b)| a.to_ascii_lowercase() == *b)
        {
            out.push(i..i + needle.len());
            i += needle.len();
            if out.len() >= MAX_CANDIDATES {
                break;
            }
        } else {
            i += 1;
        }
    }
    out
}

fn find_folded(text: &str, query: &str) -> Vec<Range<usize>> {
    let needle: Vec<char> = query.chars().map(fold).collect();
    let hay: Vec<(usize, char)> = text.char_indices().map(|(i, c)| (i, fold(c))).collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i + needle.len() <= hay.len() {
        if hay[i..i + needle.len()]
            .iter()
            .zip(&needle)
            .all(|((_, a), b)| a == b)
        {
            let start = hay[i].0;
            let end = hay.get(i + needle.len()).map_or(text.len(), |(b, _)| *b);
            out.push(start..end);
            i += needle.len();
            if out.len() >= MAX_CANDIDATES {
                break;
            }
        } else {
            i += 1;
        }
    }
    out
}

/// Index of the match to go to from a selection, searching forward or backward.
/// `past_selection`: the selection is the match found last, so an empty match
/// right there is passed over (repeated F3 advances); from a plain cursor an
/// empty match at the cursor (`^`, `$`, `\b`) is found.
pub fn next_match(
    matches: &[Match],
    selection: Range<usize>,
    forward: bool,
    wrap: bool,
    past_selection: bool,
) -> Option<usize> {
    if matches.is_empty() {
        return None;
    }
    let other = |m: &Match| !past_selection || m.chars != selection;
    if forward {
        let from = selection.end;
        match matches.iter().position(|m| m.chars.start >= from && other(m)) {
            Some(i) => Some(i),
            None => wrap.then_some(0),
        }
    } else {
        let from = selection.start;
        match matches.iter().rposition(|m| m.chars.end <= from && other(m)) {
            Some(i) => Some(i),
            None => wrap.then(|| matches.len() - 1),
        }
    }
}

/// Escapes `text` so a regular expression finds it as it is.
pub fn escape_regex(text: &str) -> String {
    fancy_regex::escape(text).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn starts(ms: &[Match]) -> Vec<usize> {
        ms.iter().map(|m| m.chars.start).collect()
    }

    #[test]
    fn case_and_words() {
        let text = "Apple apple 사과 APPLEpie 사과를";
        let ci = FindOptions::default();
        assert_eq!(starts(&find_all(text, "apple", ci)), vec![0, 6, 15]);
        let cs = FindOptions {
            match_case: true,
            ..ci
        };
        assert_eq!(starts(&find_all(text, "apple", cs)), vec![6]);
        let ww = FindOptions {
            whole_word: true,
            ..ci
        };
        assert_eq!(starts(&find_all(text, "apple", ww)), vec![0, 6]);
        assert_eq!(starts(&find_all(text, "사과", ci)), vec![12, 24]);
        assert_eq!(starts(&find_all(text, "사과", ww)), vec![12]);
    }

    #[test]
    fn non_ascii_case_insensitive() {
        let text = "ÄBC äbc";
        let m = find_all(text, "äbc", FindOptions::default());
        assert_eq!(starts(&m), vec![0, 4]);
        assert_eq!(&text[m[1].bytes.clone()], "äbc");
    }

    #[test]
    fn navigation_wraps() {
        let m = find_all("a a a", "a", FindOptions::default());
        assert_eq!(next_match(&m, 0..1, true, true, true), Some(1));
        assert_eq!(next_match(&m, 4..5, true, true, true), Some(0));
        assert_eq!(next_match(&m, 4..5, true, false, true), None);
        assert_eq!(next_match(&m, 0..1, false, true, true), Some(2));
        assert_eq!(next_match(&m, 2..2, false, false, true), Some(0));
        // Empty matches ("^"): from the cursor the one at the cursor is found;
        // from that match, the next one.
        let lines = Search::new("^", RE).unwrap().find_all("ab\ncd").unwrap();
        assert_eq!(next_match(&lines, 0..0, true, false, false), Some(0));
        assert_eq!(next_match(&lines, 0..0, true, false, true), Some(1));
        assert_eq!(next_match(&lines, 3..3, false, false, false), Some(1));
    }

    #[test]
    fn finds_every_match_past_the_limit() {
        let text = "a ".repeat(MAX_MATCHES + 1234);
        assert_eq!(find_all(&text, "a", FindOptions::default()).len(), MAX_MATCHES);
        let every = find_every(&text, "a", FindOptions::default());
        assert_eq!(every.len(), MAX_MATCHES + 1234);
        let last = every.last().unwrap();
        assert_eq!(last.chars, (MAX_MATCHES + 1233) * 2..(MAX_MATCHES + 1233) * 2 + 1);
        // Whole words over a cut-off candidate list: "ab" is a candidate but no word.
        let text = "ab a ".repeat(MAX_MATCHES + 10);
        let words = FindOptions { whole_word: true, match_case: true, ..Default::default() };
        assert_eq!(find_every(&text, "a", words).len(), MAX_MATCHES + 10);
    }

    const RE: FindOptions = FindOptions { match_case: false, whole_word: false, regex: true };

    /// Replaces every match of `query` (a regular expression) in `text`.
    fn regex_replace(text: &str, query: &str, template: &str) -> Result<String, String> {
        let search = Search::new(query, RE)?;
        let matches = search.find_every(text)?;
        Ok(search.replace_all(text, &matches, template)?.0)
    }

    #[test]
    fn replaces() {
        let text = "가 나 가";
        let search = Search::new("가", FindOptions::default()).unwrap();
        let m = search.find_all(text).unwrap();
        let (out, spans) = search.replace_all(text, &m, "다라").unwrap();
        assert_eq!(out, "다라 나 다라");
        assert_eq!(spans, vec![0..6, 11..17]);
        // Without regular expressions "$1" is just text.
        assert_eq!(search.replacement_for(text, &m[0], "$1\\n").unwrap(), "$1\\n");
    }

    #[test]
    fn regex_search() {
        let starts_of = |text: &str, query: &str, opts| starts(&Search::new(query, opts).unwrap().find_all(text).unwrap());
        assert_eq!(starts_of("a1 b22 c333", r"\d+", RE), vec![1, 4, 8]);
        assert_eq!(starts_of("AB ab", "ab", RE), vec![0, 3]);
        assert_eq!(starts_of("AB ab", "ab", FindOptions { match_case: true, ..RE }), vec![3]);
        // ^ and $ work per line.
        assert_eq!(starts_of("가나\n가다", "^가", RE), vec![0, 3]);
        assert_eq!(starts_of("가나\n가다", "[나다]$", RE), vec![1, 4]);
        // Look-around and back references, as in IntelliJ (Java).
        assert_eq!(starts_of("값 ₩500, $7", r"(?<=₩)\d+", RE), vec![3]);
        assert_eq!(starts_of("aabb cd", r"(\w)\1", RE), vec![0, 2]);
        assert_eq!(starts_of("cat cats", "cats?", FindOptions { whole_word: true, ..RE }), vec![0, 4]);
        assert_eq!(starts_of("cat concat", "cat", FindOptions { whole_word: true, ..RE }), vec![0]);
        // \w knows Hangul.
        assert_eq!(starts_of("이름: 홍길동", r"\w+$", RE), vec![4]);
    }

    #[test]
    fn regex_errors() {
        let error = Search::new("(abc", RE).err().unwrap();
        assert!(error.starts_with("정규식 오류"), "{error}");
        let error = Search::new("a**", RE).err().unwrap();
        assert!(error.contains("3번째 글자"), "{error}");
        assert!(Search::new("(abc", FindOptions::default()).is_ok());
        let search = Search::new(r"(\w+)@(?<host>\w+)", RE).unwrap();
        assert!(search.check_replacement("$1 $2 ${host} $0").is_ok());
        assert!(search.check_replacement("$3").unwrap_err().contains("3번 그룹"));
        assert!(search.check_replacement("${port}").unwrap_err().contains("port"));
        assert!(search.check_replacement("cost $").is_err());
        assert!(search.check_replacement("${host").is_err());
        assert!(search.check_replacement("끝\\").is_err());
    }

    #[test]
    fn group_replacement() {
        assert_eq!(regex_replace("kim@korea", r"(\w+)@(\w+)", "$2 at $1").unwrap(), "korea at kim");
        assert_eq!(
            regex_replace("2026-11-07", r"(?<y>\d{4})-(?<m>\d\d)-(?<d>\d\d)", "${y}년 ${m}월 ${d}일").unwrap(),
            "2026년 11월 07일"
        );
        // $12 with two groups is group 1 and then "2" (Java's rule); ${0} is the match.
        assert_eq!(regex_replace("ab", "(a)(b)", "$12|${0}").unwrap(), "a2|ab");
        // Escapes: \n, \t, \$, \\; \r\n is one line break.
        assert_eq!(regex_replace("a,b", ",", "\\n").unwrap(), "a\nb");
        assert_eq!(regex_replace("a,b", ",", "\\r\\n\\t\\$\\\\").unwrap(), "a\n\t$\\b");
        // Case changes.
        assert_eq!(regex_replace("hELLO wORLD", r"(\w)(\w*)", r"\u$1\L$2").unwrap(), "Hello World");
        assert_eq!(regex_replace("get_name", r"_(\w)", r"\U$1\E").unwrap(), "getName");
        assert_eq!(regex_replace("Title", r"\w+", r"\l$0").unwrap(), "title");
        assert_eq!(regex_replace("abc", r"\w+", r"\U$0-x\Ey").unwrap(), "ABC-Xy");
        // An optional group that did not take part is empty.
        assert_eq!(regex_replace("a", "(a)(b)?", "[$2]").unwrap(), "[]");
        // Empty matches: put "// " in front of every line.
        assert_eq!(regex_replace("a\nb\n", "^", "// ").unwrap(), "// a\n// b\n// ");
        assert_eq!(regex_replace("a\nb", "(?m)^(?=.)", "> ").unwrap(), "> a\n> b");
    }

    #[test]
    fn review_cases() {
        // \K is refused instead of panicking or looping.
        for query in [r"(?<=\K\w\w)\w", r"(?<=\K.)", r"a\Kb"] {
            assert!(Search::new(query, RE).err().unwrap().contains("\\K"), "{query}");
        }
        assert!(Search::new(r"a\\Kb", RE).is_ok());
        // A long stretch without a match is no "too complex" error.
        let long = "a".repeat(1_100_000);
        assert_eq!(Search::new(r"(?<=x)y", RE).unwrap().find_all(&long).unwrap().len(), 0);
        // A runaway pattern still stops.
        let runaway = Search::new(r"(\w+)*\1x", RE).unwrap().find_all(&"ab".repeat(30));
        assert!(runaway.unwrap_err().contains("너무 복잡"));
        // Look-behind of any length is refused (as in Java), so it cannot freeze the window.
        assert!(Search::new(r"(?<=x.*)a", RE).err().unwrap().contains("길이가 정해진"));
        // Alternatives of different fixed lengths are fine, as in Java.
        let fixed = Search::new(r"(?<=a|bc)x", RE).unwrap().find_all("ax bcx cx").unwrap();
        assert_eq!(starts(&fixed), vec![1, 5]);
        // A name used by two groups takes the one that matched.
        let text = "2026-11 11/2027";
        let both = r"(?<y>\d{4})-(\d\d)|(\d\d)/(?<y>\d{4})";
        assert_eq!(regex_replace(text, both, "${y}").unwrap(), "2026 2027");
        // Empty matches right after a match, as in Java.
        assert_eq!(regex_replace("abc", "b*", "-").unwrap(), "-a--c-");
        assert_eq!(regex_replace("a12b", r"\d*", "#").unwrap(), "#a##b#");
        // A candidate that is not a word does not hide one inside it.
        let words = FindOptions { whole_word: true, ..RE };
        let m = Search::new("a b|b", words).unwrap().find_all("xa b").unwrap();
        assert_eq!(starts(&m), vec![3]);
        // \G: the groups are found again from where the search began.
        assert_eq!(regex_replace("xab", r"\Gab|a", "[$0]").unwrap(), "x[a]b");
        // Messages say what is wrong.
        assert!(Search::new("abc)", RE).err().unwrap().contains("여는 괄호가 없습니다"));
        assert!(Search::new("[z-a]", RE).err().unwrap().contains("거꾸로"));
        assert!(Search::new(r"\p{Foo}", RE).err().unwrap().contains("유니코드 속성"));
    }

    #[test]
    fn replacement_for_one_match() {
        let text = "x=1, y=22";
        let search = Search::new(r"(\w)=(\d+)", RE).unwrap();
        let m = search.find_all(text).unwrap();
        assert_eq!(search.replacement_for(text, &m[1], "$2=$1").unwrap(), "22=y");
        // Look-behind still sees the text before the match.
        let search = Search::new(r"(?<=, )\w", RE).unwrap();
        let m = search.find_all(text).unwrap();
        assert_eq!(search.replacement_for(text, &m[0], r"\U$0").unwrap(), "Y");
        assert_eq!(escape_regex("a.b*(c)"), r"a\.b\*\(c\)");
    }
}
