//! Characters that cannot be seen or are easily mistaken for something else:
//! zero-width characters, special spaces, control characters, bidirectional
//! controls, Hangul fillers, and tag/variation-selector characters that can carry
//! hidden text.

/// What cleaning does with a character.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Fix {
    Remove,
    /// Replace with an ordinary space.
    Space,
    /// Replace with a line break.
    Newline,
}

impl Fix {
    pub fn label(self) -> &'static str {
        match self {
            Fix::Remove => "지움",
            Fix::Space => "일반 공백으로",
            Fix::Newline => "줄 바꿈으로",
        }
    }

    fn replacement(self) -> &'static str {
        match self {
            Fix::Remove => "",
            Fix::Space => " ",
            Fix::Newline => "\n",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Kind {
    /// Korean name; also identifies the kind.
    pub name: &'static str,
    pub fix: Fix,
    /// Ticked for cleaning by default. Off for characters often there on purpose.
    pub default_on: bool,
    /// Marked in the text area. Off for characters that are visible as a gap anyway
    /// and common in Korean text (전각 공백).
    pub mark: bool,
}

impl Kind {
    /// Characters that can encode text (see [`hidden_messages`]).
    pub fn can_hide_text(&self) -> bool {
        self.name == TAG.name || self.name == VARIATION_RUN.name
    }
}

const fn kind(name: &'static str, fix: Fix) -> Kind {
    Kind { name, fix, default_on: true, mark: true }
}

const CONTROL: Kind = kind("제어 문자", Fix::Remove);
const BIDI: Kind = kind("양방향 텍스트 제어 문자 (글자 순서를 뒤집어 보이게 함)", Fix::Remove);
const TAG: Kind = kind("태그 문자 (숨은 글자를 담을 수 있음)", Fix::Remove);
const VARIATION_RUN: Kind = kind("연속된 이체자 선택자 (숨은 데이터를 담을 수 있음)", Fix::Remove);

/// The kind of `c`, if it is one we look for. `prev` is the character before it
/// (variation selectors are only suspicious in runs or with nothing to modify).
pub fn classify(c: char, prev: Option<char>) -> Option<Kind> {
    let k = match c {
        '\t' | '\n' => return None,
        '\u{0}'..='\u{1F}' | '\u{7F}'..='\u{9F}' => CONTROL,
        '\u{A0}' => kind("줄 바꿈 없는 공백 (NBSP)", Fix::Space),
        '\u{AD}' => kind("소프트 하이픈", Fix::Remove),
        '\u{34F}' => kind("결합 문자 결합자 (CGJ)", Fix::Remove),
        '\u{61C}' => kind("아랍 문자 표시 (ALM)", Fix::Remove),
        '\u{115F}' => kind("한글 초성 채움 문자", Fix::Remove),
        '\u{1160}' => kind("한글 중성 채움 문자", Fix::Remove),
        '\u{17B4}' | '\u{17B5}' => kind("보이지 않는 크메르 모음", Fix::Remove),
        '\u{180E}' => kind("몽골 모음 구분자", Fix::Remove),
        '\u{2000}' => kind("엔 쿼드 공백", Fix::Space),
        '\u{2001}' => kind("엠 쿼드 공백", Fix::Space),
        '\u{2002}' => kind("엔 공백", Fix::Space),
        '\u{2003}' => kind("엠 공백", Fix::Space),
        '\u{2004}' => kind("1/3 엠 공백", Fix::Space),
        '\u{2005}' => kind("1/4 엠 공백", Fix::Space),
        '\u{2006}' => kind("1/6 엠 공백", Fix::Space),
        '\u{2007}' => kind("숫자 너비 공백", Fix::Space),
        '\u{2008}' => kind("문장 부호 너비 공백", Fix::Space),
        '\u{2009}' => kind("가는 공백", Fix::Space),
        '\u{200A}' => kind("아주 가는 공백", Fix::Space),
        '\u{200B}' => kind("폭 없는 공백 (ZWSP)", Fix::Remove),
        '\u{200C}' => kind("폭 없는 비결합자 (ZWNJ)", Fix::Remove),
        '\u{200D}' => match prev {
            // Joins emoji into one (👨‍👩‍👧): normal.
            Some(p) if is_emoji_part(p) => return None,
            _ => kind("폭 없는 결합자 (ZWJ)", Fix::Remove),
        },
        '\u{200E}' => kind("왼쪽→오른쪽 표시 (LRM)", Fix::Remove),
        '\u{200F}' => kind("오른쪽→왼쪽 표시 (RLM)", Fix::Remove),
        '\u{2028}' => kind("줄 구분자", Fix::Newline),
        '\u{2029}' => kind("문단 구분자", Fix::Newline),
        '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}' => BIDI,
        '\u{202F}' => kind("좁은 줄 바꿈 없는 공백", Fix::Space),
        '\u{205F}' => kind("수학용 중간 공백", Fix::Space),
        '\u{2060}' => kind("단어 결합자 (WJ)", Fix::Remove),
        '\u{2061}'..='\u{2064}' => kind("보이지 않는 수학 연산자", Fix::Remove),
        '\u{206A}'..='\u{206F}' => kind("쓰이지 않는 서식 문자", Fix::Remove),
        '\u{3000}' => Kind {
            default_on: false,
            mark: false,
            ..kind("전각 공백", Fix::Space)
        },
        '\u{3164}' => kind("한글 채움 문자", Fix::Remove),
        '\u{FE00}'..='\u{FE0F}' | '\u{E0100}'..='\u{E01EF}' => {
            // One after an emoji or a letter is normal (❤️, or a variant form of a
            // Chinese character); a run of them, or one with nothing before it, is
            // how data gets hidden.
            match prev {
                Some(p) if !is_variation_selector(p) && !p.is_whitespace() => return None,
                _ => VARIATION_RUN,
            }
        }
        '\u{FEFF}' => kind("폭 없는 줄 바꿈 없는 공백 (BOM)", Fix::Remove),
        '\u{FFA0}' => kind("반각 한글 채움 문자", Fix::Remove),
        '\u{FFF9}'..='\u{FFFB}' => kind("행간 주석 문자", Fix::Remove),
        '\u{1D173}'..='\u{1D17A}' => kind("악보 서식 문자", Fix::Remove),
        '\u{E0000}'..='\u{E007F}' => TAG,
        _ => return None,
    };
    Some(k)
}

/// Emoji, skin tones and the emoji variation selector: what a ZWJ normally follows.
fn is_emoji_part(c: char) -> bool {
    matches!(c as u32, 0x2300..=0x23FF | 0x2600..=0x27BF | 0x2B00..=0x2BFF | 0x1F000..=0x1FAFF | 0xFE0F)
}

fn is_variation_selector(c: char) -> bool {
    matches!(c, '\u{FE00}'..='\u{FE0F}' | '\u{E0100}'..='\u{E01EF}')
}

/// One invisible character in a text.
#[derive(Clone, Debug, PartialEq)]
pub struct Found {
    /// Character index.
    pub at: usize,
    /// Byte index.
    pub byte: usize,
    pub ch: char,
    pub kind: Kind,
}

/// All invisible characters in `text`, in order.
pub fn scan(text: &str) -> Vec<Found> {
    let mut found = Vec::new();
    let mut prev = None;
    for (at, (byte, ch)) in text.char_indices().enumerate() {
        if let Some(kind) = classify(ch, prev) {
            found.push(Found { at, byte, ch, kind });
        }
        prev = Some(ch);
    }
    found
}

/// `text` with the characters in `found` for which `pick` is true removed or
/// replaced, and where character index `cursor` ends up.
pub fn clean(text: &str, found: &[Found], cursor: usize, pick: impl Fn(&Kind) -> bool) -> (String, usize) {
    let mut out = String::with_capacity(text.len());
    let mut last = 0;
    let mut new_cursor = cursor;
    for f in found.iter().filter(|f| pick(&f.kind)) {
        out.push_str(&text[last..f.byte]);
        let replacement = f.kind.fix.replacement();
        out.push_str(replacement);
        last = f.byte + f.ch.len_utf8();
        if f.at < cursor && replacement.is_empty() {
            new_cursor -= 1;
        }
    }
    out.push_str(&text[last..]);
    (out, new_cursor)
}

/// Text hidden in runs of tag characters (U+E0020–E007E stand for ASCII) or of
/// variation selectors (one byte each, as "emoji smuggling" does): (character
/// index where the run starts, decoded text). Runs that decode to nothing
/// readable are left out.
pub fn hidden_messages(text: &str) -> Vec<(usize, String)> {
    let mut out = Vec::new();
    let mut tag = (0, String::new());
    let mut bytes = (0, Vec::new());
    // A NUL at the end flushes the last run.
    for (i, c) in text.chars().chain(std::iter::once('\0')).enumerate() {
        let code = c as u32;
        if (0xE0020..=0xE007E).contains(&code) {
            if tag.1.is_empty() {
                tag.0 = i;
            }
            tag.1.push(char::from((code - 0xE0000) as u8));
        } else if !tag.1.is_empty() {
            out.push(std::mem::take(&mut tag));
        }
        let byte = match code {
            0xFE00..=0xFE0F => Some((code - 0xFE00) as u8),
            0xE0100..=0xE01EF => Some((code - 0xE0100 + 16) as u8),
            _ => None,
        };
        match byte {
            Some(b) => {
                if bytes.1.is_empty() {
                    bytes.0 = i;
                }
                bytes.1.push(b);
            }
            None if bytes.1.len() >= 2 => {
                let (at, data) = std::mem::take(&mut bytes);
                if let Ok(s) = String::from_utf8(data) {
                    if s.chars().any(|c| !c.is_control()) {
                        out.push((at, s));
                    }
                }
            }
            None => bytes.1.clear(),
        }
    }
    out.sort_by_key(|(at, _)| *at);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(text: &str) -> Vec<&'static str> {
        scan(text).iter().map(|f| f.kind.name).collect()
    }

    #[test]
    fn finds_invisible_characters() {
        assert!(scan("평범한 글\t탭과\n줄 바꿈 ").is_empty());
        let found = scan("가\u{200B}나\u{A0}다\u{3164}");
        assert_eq!(found.len(), 3);
        assert_eq!((found[0].at, found[0].byte), (1, 3));
        assert_eq!((found[1].at, found[1].byte), (3, 9));
        assert_eq!(found[2].kind.name, "한글 채움 문자");
        assert_eq!(names("\u{202E}\u{2066}"), [BIDI.name, BIDI.name]);
        assert_eq!(names("\u{1}\u{7F}\u{85}"), [CONTROL.name; 3]);
    }

    #[test]
    fn variation_selectors() {
        // A heart with its emoji selector is normal text.
        assert!(scan("❤\u{FE0F}").is_empty());
        // A run of selectors, or one after a space, is suspicious.
        assert_eq!(scan("❤\u{FE0F}\u{FE01}").len(), 1);
        assert_eq!(scan(" \u{FE0F}").len(), 1);
        assert!(scan("葛\u{E0100}").is_empty());
        // Joiners inside emoji are normal; elsewhere they are not.
        assert!(scan("👨\u{200D}👩\u{200D}👧 ❤\u{FE0F}\u{200D}🔥").is_empty());
        assert_eq!(scan("가\u{200D}나").len(), 1);
        assert_eq!(scan("a\u{E0100}\u{E0101}").len(), 1);
    }

    #[test]
    fn cleans_and_moves_cursor() {
        let text = "가\u{200B}나\u{A0}다\u{2028}라";
        let found = scan(text);
        let (out, cursor) = clean(text, &found, 6, |_| true);
        assert_eq!(out, "가나 다\n라");
        assert_eq!(cursor, 5);
        let (out, _) = clean(text, &found, 0, |k| k.fix == Fix::Space);
        assert_eq!(out, "가\u{200B}나 다\u{2028}라");
        let wide = "\u{3000}들여쓰기";
        let (out, _) = clean(wide, &scan(wide), 0, |k| k.default_on);
        assert_eq!(out, wide);
    }

    #[test]
    fn decodes_hidden_text() {
        let tags: String = "hi!".chars().map(|c| char::from_u32(0xE0000 + c as u32).unwrap()).collect();
        let text = format!("안녕{tags}하세요");
        assert_eq!(hidden_messages(&text), [(2, "hi!".to_owned())]);

        // "가" (EA B0 80) hidden after an emoji as variation selectors 17+.
        let vs: String = [0xEAu32, 0xB0, 0x80]
            .iter()
            .map(|b| char::from_u32(0xE0100 + b - 16).unwrap())
            .collect();
        assert_eq!(hidden_messages(&format!("😀{vs}끝")), [(1, "가".to_owned())]);
        assert!(hidden_messages("❤\u{FE0F} 평범").is_empty());
    }
}
