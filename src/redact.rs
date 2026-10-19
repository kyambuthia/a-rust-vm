//! Redaction of secret-looking values in tool output.
//!
//! Tool output goes to the model provider, the screen, and saved transcripts.
//! A file such as `.env`, a command such as `git remote -v`, or an MCP server
//! can print credentials, so known secret shapes are replaced with
//! `[redacted]` before the output leaves the tool loop. This is a safety net,
//! not a guarantee: an unknown secret format still gets through.

use std::borrow::Cow;

pub const REDACTED: &str = "[redacted]";

/// Prefixes of well-known token formats and the minimum number of token
/// characters that must follow, so ordinary words are not matched.
const TOKEN_PREFIXES: &[(&str, usize)] = &[
    ("sk-or-v1-", 16),
    ("sk-ant-", 16),
    ("sk-proj-", 16),
    ("sk-", 20),
    ("ghp_", 20),
    ("gho_", 20),
    ("ghu_", 20),
    ("ghs_", 20),
    ("ghr_", 20),
    ("github_pat_", 20),
    ("glpat-", 16),
    ("xoxb-", 16),
    ("xoxp-", 16),
    ("xoxa-", 16),
    ("xoxs-", 16),
    ("AIza", 30),
];

/// Words that mark an upper-case variable name as holding a secret.
const SECRET_NAME_MARKERS: &[&str] = &[
    "SECRET",
    "TOKEN",
    "PASSWORD",
    "PASSWD",
    "API_KEY",
    "APIKEY",
    "PRIVATE_KEY",
    "ACCESS_KEY",
    "CREDENTIAL",
];

const MIN_LITERAL_LENGTH: usize = 8;
const MIN_ASSIGNED_VALUE_LENGTH: usize = 6;

/// Replaces secret-looking values in text.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Redactor {
    /// Exact values to hide, longest first.
    literals: Vec<String>,
}

impl Redactor {
    /// A redactor for known token shapes only.
    pub fn new() -> Self {
        Self::default()
    }

    /// Also hide the values of host environment variables whose names mark
    /// them as secrets, such as `OPENROUTER_API_KEY` or `DB_PASSWORD`.
    pub fn from_environment() -> Self {
        let mut redactor = Self::new();
        for (name, value) in std::env::vars_os() {
            let (Some(name), Some(value)) = (name.to_str(), value.to_str()) else {
                continue;
            };
            if is_secret_name(name) {
                redactor.add_literal(value);
            }
        }
        redactor
    }

    /// Hide every occurrence of `value` (ignored when shorter than 8 bytes).
    pub fn add_literal(&mut self, value: &str) {
        let value = value.trim();
        if value.len() >= MIN_LITERAL_LENGTH && !self.literals.iter().any(|known| known == value) {
            self.literals.push(value.to_owned());
            self.literals
                .sort_by_key(|literal| std::cmp::Reverse(literal.len()));
        }
    }

    pub fn redact<'a>(&self, text: &'a str) -> Cow<'a, str> {
        let mut output = Cow::Borrowed(text);
        for literal in &self.literals {
            if output.contains(literal.as_str()) {
                output = Cow::Owned(output.replace(literal.as_str(), REDACTED));
            }
        }
        for step in [
            redact_private_keys as fn(&str) -> Option<String>,
            redact_token_prefixes,
            redact_aws_key_ids,
            redact_bearer_tokens,
            redact_secret_assignments,
        ] {
            if let Some(changed) = step(&output) {
                output = Cow::Owned(changed);
            }
        }
        output
    }
}

/// Whether an environment variable name marks a secret.
pub fn is_secret_name(name: &str) -> bool {
    let upper = name.to_ascii_uppercase();
    SECRET_NAME_MARKERS
        .iter()
        .any(|marker| upper.contains(marker))
}

fn is_token_char(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-')
}

/// Replace `text[start..end]` ranges (sorted, non-overlapping) with the marker.
fn replace_ranges(text: &str, ranges: &[(usize, usize)]) -> Option<String> {
    if ranges.is_empty() {
        return None;
    }
    let mut output = String::with_capacity(text.len());
    let mut cursor = 0;
    for &(start, end) in ranges {
        output.push_str(&text[cursor..start]);
        output.push_str(REDACTED);
        cursor = end;
    }
    output.push_str(&text[cursor..]);
    Some(output)
}

fn redact_private_keys(text: &str) -> Option<String> {
    const BEGIN: &str = "-----BEGIN ";
    const END: &str = "-----END ";
    let mut ranges = Vec::new();
    let mut search = 0;
    while let Some(found) = text[search..].find(BEGIN) {
        let start = search + found;
        let header_end = match text[start + BEGIN.len()..].find("-----") {
            Some(offset) => start + BEGIN.len() + offset + 5,
            None => break,
        };
        if !text[start..header_end].contains("PRIVATE KEY") {
            search = header_end;
            continue;
        }
        let end = match text[header_end..].find(END) {
            Some(offset) => {
                let footer = header_end + offset + END.len();
                text[footer..]
                    .find("-----")
                    .map(|close| footer + close + 5)
                    .unwrap_or(text.len())
            }
            None => text.len(),
        };
        ranges.push((start, end));
        search = end;
    }
    replace_ranges(text, &ranges)
}

fn redact_token_prefixes(text: &str) -> Option<String> {
    let bytes = text.as_bytes();
    let mut ranges: Vec<(usize, usize)> = Vec::new();
    let mut index = 0;
    while index < bytes.len() {
        let boundary = index == 0 || !is_token_char(bytes[index - 1]);
        let matched = boundary
            .then(|| {
                TOKEN_PREFIXES.iter().find_map(|(prefix, minimum)| {
                    // Compare bytes: `index` walks bytes and may sit inside
                    // a multi-byte character, where slicing `text` panics.
                    if !bytes[index..].starts_with(prefix.as_bytes()) {
                        return None;
                    }
                    let body = bytes[index + prefix.len()..]
                        .iter()
                        .take_while(|byte| is_token_char(**byte))
                        .count();
                    (body >= *minimum).then_some(index + prefix.len() + body)
                })
            })
            .flatten();
        match matched {
            Some(end) => {
                ranges.push((index, end));
                index = end;
            }
            None => index += 1,
        }
    }
    replace_ranges(text, &ranges)
}

/// AWS access key IDs: `AKIA` or `ASIA` and 16 upper-case letters or digits.
fn redact_aws_key_ids(text: &str) -> Option<String> {
    let bytes = text.as_bytes();
    let mut ranges = Vec::new();
    let mut index = 0;
    while index + 20 <= bytes.len() {
        let candidate = &bytes[index..index + 20];
        let boundary_before = index == 0 || !bytes[index - 1].is_ascii_alphanumeric();
        let boundary_after = bytes
            .get(index + 20)
            .is_none_or(|byte| !byte.is_ascii_alphanumeric());
        if boundary_before
            && boundary_after
            && (candidate.starts_with(b"AKIA") || candidate.starts_with(b"ASIA"))
            && candidate[4..]
                .iter()
                .all(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit())
        {
            ranges.push((index, index + 20));
            index += 20;
        } else {
            index += 1;
        }
    }
    replace_ranges(text, &ranges)
}

fn redact_bearer_tokens(text: &str) -> Option<String> {
    let lower = text.to_ascii_lowercase();
    let bytes = text.as_bytes();
    let mut ranges = Vec::new();
    let mut search = 0;
    while let Some(found) = lower[search..].find("bearer ") {
        let start = search + found + "bearer ".len();
        let length = bytes[start..]
            .iter()
            .take_while(|byte| {
                byte.is_ascii_alphanumeric()
                    || matches!(byte, b'-' | b'_' | b'.' | b'~' | b'+' | b'/' | b'=')
            })
            .count();
        if length >= 16 {
            ranges.push((start, start + length));
        }
        search = start + length.max(1);
        if search >= text.len() {
            break;
        }
    }
    replace_ranges(text, &ranges)
}

/// `DB_PASSWORD=value`, `export API_TOKEN="value"`, `"AWS_SECRET_ACCESS_KEY": "value"`.
/// Only upper-case names are matched, so code such as `token: &str` is left alone.
fn redact_secret_assignments(text: &str) -> Option<String> {
    let bytes = text.as_bytes();
    let mut ranges = Vec::new();
    let mut index = 0;
    while index < bytes.len() {
        let starts_name = (bytes[index].is_ascii_uppercase() || bytes[index] == b'_')
            && (index == 0
                || !(bytes[index - 1].is_ascii_alphanumeric() || bytes[index - 1] == b'_'));
        if !starts_name {
            index += 1;
            continue;
        }
        let name_length = bytes[index..]
            .iter()
            .take_while(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit() || **byte == b'_')
            .count();
        let name = &text[index..index + name_length];
        let mut cursor = index + name_length;
        if !is_secret_name(name) {
            index = cursor.max(index + 1);
            continue;
        }
        if bytes
            .get(cursor)
            .is_some_and(|byte| matches!(byte, b'"' | b'\''))
        {
            cursor += 1;
        }
        while bytes.get(cursor) == Some(&b' ') {
            cursor += 1;
        }
        if !bytes
            .get(cursor)
            .is_some_and(|byte| matches!(byte, b'=' | b':'))
        {
            index = cursor.max(index + 1);
            continue;
        }
        cursor += 1;
        while bytes.get(cursor) == Some(&b' ') {
            cursor += 1;
        }
        let quote = bytes
            .get(cursor)
            .copied()
            .filter(|byte| matches!(byte, b'"' | b'\''));
        if quote.is_some() {
            cursor += 1;
        }
        let value_start = cursor;
        let value_length = bytes[value_start..]
            .iter()
            .take_while(|byte| match quote {
                Some(quote) => **byte != quote && **byte != b'\n',
                None => {
                    !byte.is_ascii_whitespace()
                        && !matches!(byte, b'"' | b'\'' | b',' | b'}' | b';')
                }
            })
            .count();
        let value = &text[value_start..value_start + value_length];
        let looks_like_reference =
            value.starts_with(['&', '<', '(', '$', '{', '[']) || value == REDACTED;
        if value_length >= MIN_ASSIGNED_VALUE_LENGTH && !looks_like_reference {
            ranges.push((value_start, value_start + value_length));
        }
        index = value_start + value_length.max(1);
    }
    replace_ranges(text, &ranges)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn redact(text: &str) -> String {
        Redactor::new().redact(text).into_owned()
    }

    #[test]
    fn multibyte_text_never_panics_and_secrets_stay_hidden() {
        let secrets = [
            "sk-or-v1-0123456789abcdef0123",
            "ghp_0123456789abcdefghij0123",
            "AKIAABCDEFGHIJKLMNOP",
            "Bearer abcdefghijklmnop0123",
            "DB_PASSWORD=hunter22hunter",
            "-----BEGIN PRIVATE KEY-----\nabc\n-----END PRIVATE KEY-----",
        ];
        let fillers = ["·", "é", "日本", "🦀", "a·b", "\u{200d}"];
        let mut redactor = Redactor::new();
        redactor.add_literal("literal·secret");
        for secret in secrets {
            for filler in fillers {
                for split in 0..=filler.len() {
                    let Some((left, right)) = filler
                        .is_char_boundary(split)
                        .then(|| filler.split_at(split))
                    else {
                        continue;
                    };
                    let text = format!("{left}{filler} {secret} {right}{filler}·");
                    let output = redactor.redact(&text);
                    assert!(output.contains(REDACTED), "{text:?} -> {output:?}");
                    let glued = format!("{filler}{secret}{filler}literal·secret");
                    let output = redactor.redact(&glued);
                    assert!(!output.contains("literal·secret"), "{glued:?}");
                }
            }
        }
        // The shape that crashed `read_file` on README.md.
        let readme = "`[usage] 2 model steps · 2,291 in (1,925 cached) · 91 out · $0.000085`";
        assert_eq!(redactor.redact(readme), readme);
    }

    #[test]
    fn hides_known_token_formats() {
        for (input, expected) in [
            (
                "key sk-or-v1-0123456789abcdef0123 end",
                "key [redacted] end",
            ),
            (
                "ANTHROPIC sk-ant-api03-abcdefghijklmnop",
                "ANTHROPIC [redacted]",
            ),
            ("ghp_abcdefghijklmnopqrstuvwxyz0123", "[redacted]"),
            (
                "https://x:glpat-abcdefghijklmnop12@gitlab.com",
                "https://x:[redacted]@gitlab.com",
            ),
            ("id AKIAIOSFODNN7EXAMPLE.", "id [redacted]."),
            (
                "Authorization: Bearer eyJhbGciOiJIUzI1NiJ9.abc.def",
                "Authorization: Bearer [redacted]",
            ),
        ] {
            assert_eq!(redact(input), expected, "input: {input}");
        }
    }

    #[test]
    fn hides_env_style_assignments_but_not_code() {
        assert_eq!(
            redact("DB_PASSWORD=hunter2hunter\nPORT=8080\nexport API_TOKEN=\"abc def ghi\""),
            "DB_PASSWORD=[redacted]\nPORT=8080\nexport API_TOKEN=\"[redacted]\""
        );
        assert_eq!(
            redact(r#"{"AWS_SECRET_ACCESS_KEY": "wJalrXUtnFEMI/K7MDENG"}"#),
            r#"{"AWS_SECRET_ACCESS_KEY": "[redacted]"}"#
        );
        for code in [
            "fn login(token: &str, password: String)",
            "const API_KEY: &str = env!(\"KEY\");",
            "TOKEN=${GITHUB_TOKEN}",
            "let sk = desk-lamp; task-list",
            "SECRET=short",
        ] {
            assert_eq!(redact(code), code);
        }
    }

    #[test]
    fn hides_private_key_blocks_and_environment_values() {
        let pem = "a\n-----BEGIN OPENSSH PRIVATE KEY-----\nb3BlbnNzaA\n-----END OPENSSH PRIVATE KEY-----\nz";
        assert_eq!(redact(pem), "a\n[redacted]\nz");
        let certificate = "-----BEGIN CERTIFICATE-----\nMIIB\n-----END CERTIFICATE-----";
        assert_eq!(redact(certificate), certificate);

        let mut redactor = Redactor::new();
        redactor.add_literal("custom-secret-value");
        redactor.add_literal("short");
        assert_eq!(
            redactor.redact("x=custom-secret-value short").into_owned(),
            "x=[redacted] short"
        );
        assert!(is_secret_name("OPENROUTER_API_KEY"));
        assert!(is_secret_name("db_password"));
        assert!(!is_secret_name("PATH"));
    }

    #[test]
    fn leaves_clean_text_borrowed() {
        assert!(matches!(
            Redactor::new().redact("plain output\nexit_code=0"),
            Cow::Borrowed(_)
        ));
    }
}
