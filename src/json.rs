//! JSON pretty printing for 서식 > JSON 정리.
//!
//! The text is checked with serde_json first, then re-indented token by token:
//! strings, numbers and literals are copied as written, so key order, number
//! spelling (`1.0e10`, very long integers), escapes and duplicate keys survive.

use serde::de::IgnoredAny;

const INDENT: &str = "  ";

/// Re-indents valid JSON with two spaces per level. Empty `{}` and `[]` stay on
/// one line, and a trailing line break is kept if the text had one.
pub fn pretty(text: &str) -> Result<String, serde_json::Error> {
    serde_json::from_str::<IgnoredAny>(text)?;

    let mut out = String::with_capacity(text.len() + text.len() / 4);
    let mut depth = 0;
    let mut in_string = false;
    let mut escaped = false;
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if in_string {
            out.push(c);
            if escaped {
                escaped = false;
            } else if c == '\\' {
                escaped = true;
            } else if c == '"' {
                in_string = false;
            }
            continue;
        }
        match c {
            '"' => {
                in_string = true;
                out.push(c);
            }
            '{' | '[' => {
                out.push(c);
                while chars.next_if(|c| c.is_ascii_whitespace()).is_some() {}
                let close = if c == '{' { '}' } else { ']' };
                if chars.next_if_eq(&close).is_some() {
                    out.push(close);
                } else {
                    depth += 1;
                    new_line(&mut out, depth);
                }
            }
            '}' | ']' => {
                depth -= 1;
                new_line(&mut out, depth);
                out.push(c);
            }
            ',' => {
                out.push(c);
                new_line(&mut out, depth);
            }
            ':' => out.push_str(": "),
            // Validated JSON has only spaces, tabs and line breaks between tokens.
            c if c.is_ascii_whitespace() => {}
            c => out.push(c),
        }
    }
    if text.ends_with('\n') {
        out.push('\n');
    }
    Ok(out)
}

fn new_line(out: &mut String, depth: usize) {
    out.push('\n');
    for _ in 0..depth {
        out.push_str(INDENT);
    }
}

/// Character index of a serde_json error position (one-based line, and column
/// counted in bytes within that line).
pub fn error_char_index(text: &str, line: usize, column: usize) -> usize {
    let mut index = 0;
    for (i, l) in text.split('\n').enumerate() {
        if i + 1 == line {
            let byte = column.saturating_sub(1).min(l.len());
            return index + l.char_indices().take_while(|&(b, _)| b < byte).count();
        }
        index += l.chars().count() + 1;
    }
    text.chars().count()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn indents() {
        let text = r#"{"a":1,"b":[true,null,{"c":"x"}],"d":{}}"#;
        let expected = "{\n  \"a\": 1,\n  \"b\": [\n    true,\n    null,\n    {\n      \"c\": \"x\"\n    }\n  ],\n  \"d\": {}\n}";
        assert_eq!(pretty(text).unwrap(), expected);
        // Already pretty text comes back unchanged.
        assert_eq!(pretty(expected).unwrap(), expected);
    }

    #[test]
    fn keeps_values_as_written() {
        let text = "{ \"z\" : 12345678901234567890, \"a\":1.0e10,\"가\":\"{[,:]} \\\"따옴표\\\" \\u0041\", \"z\":[ ] }\n";
        let expected = "{\n  \"z\": 12345678901234567890,\n  \"a\": 1.0e10,\n  \"가\": \"{[,:]} \\\"따옴표\\\" \\u0041\",\n  \"z\": []\n}\n";
        assert_eq!(pretty(text).unwrap(), expected);
    }

    #[test]
    fn scalars_and_errors() {
        assert_eq!(pretty("  42 ").unwrap(), "42");
        let err = pretty("{\n  \"가\": 1,\n}").unwrap_err();
        assert_eq!(err.line(), 3);
        let text = "{\n  \"가\": x\n}";
        let err = pretty(text).unwrap_err();
        let at = error_char_index(text, err.line(), err.column());
        assert_eq!(text.chars().nth(at), Some('x'));
    }
}
