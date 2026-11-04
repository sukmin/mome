//! Searching the document.

use std::ops::Range;

/// Searching stops after this many matches; enough for highlighting and counting.
pub const MAX_MATCHES: usize = 100_000;
/// The case-insensitive searches stop after this many candidates. The whole-word
/// filter may drop some of them; [`MAX_MATCHES`] is applied after it.
const MAX_CANDIDATES: usize = MAX_MATCHES * 2;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct FindOptions {
    pub match_case: bool,
    pub whole_word: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Match {
    pub chars: Range<usize>,
    pub bytes: Range<usize>,
}

fn is_word_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// Lower-cases a character without changing the number of characters.
fn fold(c: char) -> char {
    let mut lower = c.to_lowercase();
    match (lower.next(), lower.next()) {
        (Some(l), None) => l,
        _ => c,
    }
}

/// All non-overlapping matches of `query`, in order.
pub fn find_all(text: &str, query: &str, opts: FindOptions) -> Vec<Match> {
    if query.is_empty() || text.is_empty() {
        return Vec::new();
    }
    let byte_matches = if opts.match_case {
        text.match_indices(query)
            .map(|(start, m)| start..start + m.len())
            .collect::<Vec<_>>()
    } else if query.is_ascii() {
        find_ascii_case_insensitive(text, query)
    } else {
        find_folded(text, query)
    };

    let mut out = Vec::new();
    let mut char_pos = 0;
    let mut byte_pos = 0;
    for bytes in byte_matches {
        if opts.whole_word {
            let before = text[..bytes.start].chars().next_back();
            let after = text[bytes.end..].chars().next();
            if before.is_some_and(is_word_char) || after.is_some_and(is_word_char) {
                continue;
            }
        }
        char_pos += text[byte_pos..bytes.start].chars().count();
        let len = text[bytes.clone()].chars().count();
        out.push(Match {
            chars: char_pos..char_pos + len,
            bytes: bytes.clone(),
        });
        char_pos += len;
        byte_pos = bytes.end;
        if out.len() >= MAX_MATCHES {
            break;
        }
    }
    out
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
pub fn next_match(
    matches: &[Match],
    selection: Range<usize>,
    forward: bool,
    wrap: bool,
) -> Option<usize> {
    if matches.is_empty() {
        return None;
    }
    if forward {
        // Start after the current selection so repeated F3 advances.
        let from = selection.end;
        match matches.iter().position(|m| m.chars.start >= from && m.chars != selection) {
            Some(i) => Some(i),
            None => wrap.then_some(0),
        }
    } else {
        let from = selection.start;
        match matches.iter().rposition(|m| m.chars.end <= from && m.chars != selection) {
            Some(i) => Some(i),
            None => wrap.then(|| matches.len() - 1),
        }
    }
}

/// Replaces every match, returning the new text.
pub fn replace_all(text: &str, matches: &[Match], replacement: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut last = 0;
    for m in matches {
        out.push_str(&text[last..m.bytes.start]);
        out.push_str(replacement);
        last = m.bytes.end;
    }
    out.push_str(&text[last..]);
    out
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
        assert_eq!(next_match(&m, 0..1, true, true), Some(1));
        assert_eq!(next_match(&m, 4..5, true, true), Some(0));
        assert_eq!(next_match(&m, 4..5, true, false), None);
        assert_eq!(next_match(&m, 0..1, false, true), Some(2));
        assert_eq!(next_match(&m, 2..2, false, false), Some(0));
    }

    #[test]
    fn replaces() {
        let text = "가 나 가";
        let m = find_all(text, "가", FindOptions::default());
        assert_eq!(replace_all(text, &m, "다라"), "다라 나 다라");
    }
}
