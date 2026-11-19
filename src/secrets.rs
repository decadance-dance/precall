//! What the built-in `secrets` check looks for (spec §10): common token and key formats,
//! private key blocks, passwords in URLs and high-entropy assignments. It names the pattern a
//! text matches and never returns the matched text; [`redact`] replaces every match in what the
//! core keeps of a plugin's output (spec §6.3).
//!
//! Every scan reads a bounded window from each candidate position and skips candidates inside
//! a token it already read, so the time is linear in the text: a check runs over every
//! proposal of a run before it answers (spec §6.5).

use std::ops::Range;

/// The most characters a value or token is read for; real credentials are shorter.
const WINDOW: usize = 256;

/// A token that starts with a fixed prefix at a word boundary and goes on with at least `min`
/// characters of its alphabet (exactly `min` when `exact`, ended by a non-word character).
/// A token of open length must also be credential-shaped: upper and lower case and a digit,
/// which generated keys have and paths and words (`sk-learn-tests`) do not.
struct Prefixed {
    name: &'static str,
    prefixes: &'static [&'static str],
    min: usize,
    exact: bool,
    alphabet: fn(u8) -> bool,
}

fn alnum(b: u8) -> bool {
    b.is_ascii_alphanumeric()
}
fn upper_digit(b: u8) -> bool {
    b.is_ascii_uppercase() || b.is_ascii_digit()
}
fn word(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_' || b == b'-'
}
fn word_underscore(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}
fn label_char(b: u8) -> bool {
    b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'.')
}

/// More specific prefixes first: `sk-ant-` before `sk-`.
const PREFIXED: &[Prefixed] = &[
    Prefixed {
        name: "aws-access-key",
        prefixes: &["AKIA", "ASIA"],
        min: 16,
        exact: true,
        alphabet: upper_digit,
    },
    Prefixed {
        name: "github-token",
        prefixes: &["ghp_", "gho_", "ghu_", "ghs_", "ghr_"],
        min: 36,
        exact: false,
        alphabet: alnum,
    },
    Prefixed {
        name: "github-token",
        prefixes: &["github_pat_"],
        min: 22,
        exact: false,
        alphabet: word_underscore,
    },
    Prefixed {
        name: "gitlab-token",
        prefixes: &["glpat-"],
        min: 20,
        exact: false,
        alphabet: word,
    },
    Prefixed {
        name: "slack-token",
        prefixes: &["xoxa-", "xoxb-", "xoxp-", "xoxr-", "xoxs-", "xapp-"],
        min: 10,
        exact: false,
        alphabet: word,
    },
    Prefixed {
        name: "anthropic-key",
        prefixes: &["sk-ant-"],
        min: 20,
        exact: false,
        alphabet: word,
    },
    Prefixed {
        name: "openai-key",
        prefixes: &["sk-"],
        min: 20,
        exact: false,
        alphabet: word,
    },
    Prefixed {
        name: "google-api-key",
        prefixes: &["AIza"],
        min: 35,
        exact: true,
        alphabet: word,
    },
    Prefixed {
        name: "stripe-key",
        prefixes: &["sk_live_", "rk_live_"],
        min: 20,
        exact: false,
        alphabet: alnum,
    },
    Prefixed {
        name: "npm-token",
        prefixes: &["npm_"],
        min: 36,
        exact: false,
        alphabet: alnum,
    },
];

/// Labels that make the value after a `=` or `:` a credential when the value looks random.
const SECRET_WORDS: [&str; 8] = [
    "password", "passwd", "pwd", "secret", "token", "api_key", "apikey", "api-key",
];

/// Words between a label and its separator in prose: `the deploy token is: ...`.
const LINKING_WORDS: [&str; 3] = ["is", "was", "are"];

/// The matches of one pattern, lazily and in text order (each starts at or after the one
/// before), so a first-match query stops early and redaction merges them in one pass.
type Spans<'a> = Box<dyn Iterator<Item = Range<usize>> + 'a>;

/// The name of the first pattern `text` matches.
pub fn find(text: &str) -> Option<&'static str> {
    let lower = text.to_ascii_lowercase();
    detectors(text, &lower)
        .into_iter()
        .find_map(|(name, mut spans)| spans.next().map(|_| name))
}

/// `text` with every match replaced by `[redacted:<pattern>]`; overlapping matches leave one
/// marker. What the core keeps of a plugin's output goes through this (spec §6.3). One pass:
/// it merges the patterns' matches in text order and holds only the next one of each.
pub fn redact(text: &str) -> String {
    let lower = text.to_ascii_lowercase();
    let mut heads: Vec<(&'static str, Spans, Option<Range<usize>>)> = detectors(text, &lower)
        .into_iter()
        .map(|(name, mut spans)| {
            let next = spans.next();
            (name, spans, next)
        })
        .collect();
    let mut out = String::with_capacity(text.len());
    let mut at = 0;
    // The match that starts first; at one start the longest, then the pattern tried first.
    while let Some(k) = heads
        .iter()
        .enumerate()
        .filter_map(|(k, h)| h.2.as_ref().map(|r| (k, r.start, std::cmp::Reverse(r.end))))
        .min_by_key(|&(_, start, end)| (start, end))
        .map(|(k, _, _)| k)
    {
        let (name, spans, head) = &mut heads[k];
        let Some(r) = std::mem::replace(head, spans.next()) else {
            break;
        };
        if r.start >= at {
            out += &text[at..r.start];
            out += &format!("[redacted:{name}]");
        }
        at = at.max(r.end);
    }
    out += &text[at..];
    out
}

/// Every pattern with its matches, in the order [`find`] tries them. `lower` is `text` in ASCII
/// lower case.
fn detectors<'a>(text: &'a str, lower: &'a str) -> Vec<(&'static str, Spans<'a>)> {
    let mut d: Vec<(&'static str, Spans<'a>)> = Vec::new();
    for p in PREFIXED {
        for prefix in p.prefixes {
            d.push((p.name, Box::new(prefixed(text, p, prefix))));
        }
    }
    d.push(("slack-webhook", Box::new(slack_webhook(text))));
    d.push(("jwt", Box::new(jwt(text))));
    d.push(("private-key", Box::new(private_key(text))));
    d.push(("password-in-url", Box::new(password_in_url(text))));
    for word in ["Bearer ", "bearer "] {
        d.push(("bearer-token", Box::new(bearer(text, word))));
    }
    for word in SECRET_WORDS {
        d.push((
            "high-entropy-assignment",
            Box::new(assignment(text, lower, word)),
        ));
    }
    d
}

/// The tokens of one prefix of [`PREFIXED`], prefix included.
fn prefixed<'a>(
    text: &'a str,
    p: &'static Prefixed,
    prefix: &'static str,
) -> impl Iterator<Item = Range<usize>> + 'a {
    let b = text.as_bytes();
    text.match_indices(prefix).filter_map(move |(i, _)| {
        if i > 0 && word(b[i - 1]) {
            return None;
        }
        let start = i + prefix.len();
        let n = run(b, start, p.alphabet, WINDOW);
        let ok = if p.exact {
            n == p.min && b.get(start + p.min).is_none_or(|c| !word(*c))
        } else {
            n >= p.min && shaped(&b[start..start + n])
        };
        ok.then_some(i..start + n)
    })
}

/// How many bytes from `start` on belong to `alphabet`, counting at most `cap`.
fn run(b: &[u8], start: usize, alphabet: fn(u8) -> bool, cap: usize) -> usize {
    b.get(start..).map_or(0, |rest| {
        rest.iter().take(cap).take_while(|c| alphabet(**c)).count()
    })
}

/// Upper case, lower case and a digit.
fn shaped(tok: &[u8]) -> bool {
    tok.iter().any(u8::is_ascii_uppercase)
        && tok.iter().any(u8::is_ascii_lowercase)
        && tok.iter().any(u8::is_ascii_digit)
}

/// `hooks.slack.com/services/T<team>/B<bot>/<secret>`, the whole path.
fn slack_webhook(text: &str) -> impl Iterator<Item = Range<usize>> + '_ {
    let b = text.as_bytes();
    let path_end = move |mut at: usize| {
        for (lead, min) in [(Some(b'T'), 2), (Some(b'B'), 2), (None, 16)] {
            if lead.is_some_and(|l| b.get(at) != Some(&l)) {
                return None;
            }
            let n = run(b, at, alnum, WINDOW);
            if n < min {
                return None;
            }
            at += n;
            if lead.is_some() {
                if b.get(at) != Some(&b'/') {
                    return None;
                }
                at += 1;
            }
        }
        Some(at)
    };
    text.match_indices("hooks.slack.com/services/")
        .filter_map(move |(i, m)| path_end(i + m.len()).map(|end| i..end))
}

/// Three dot-separated base64url segments of at least 10 characters, the first two starting
/// with `eyJ` (a JSON object). One pass over the maximal segments, each read once and never
/// capped: a token with many claims has a long payload.
fn jwt(text: &str) -> impl Iterator<Item = Range<usize>> + '_ {
    let b = text.as_bytes();
    // The last two segments seen, as (start, end), while each followed the one before by a
    // single `.`.
    let mut chain: Vec<(usize, usize)> = Vec::with_capacity(2);
    let mut i = 0;
    std::iter::from_fn(move || {
        while i < b.len() {
            if !word(b[i]) {
                i += 1;
                continue;
            }
            let start = i;
            while i < b.len() && word(b[i]) {
                i += 1;
            }
            let joined = chain
                .last()
                .is_some_and(|&(_, end)| end + 1 == start && b[end] == b'.');
            if !joined {
                chain.clear();
            }
            let long = i - start >= 10;
            let object = b[start..i].starts_with(b"eyJ");
            match chain.len() {
                2 if long => {
                    let found = chain[0].0..i;
                    chain.clear();
                    return Some(found);
                }
                0 | 1 if long && object => chain.push((start, i)),
                _ => {
                    chain.clear();
                    if long && object {
                        chain.push((start, i));
                    }
                }
            }
        }
        None
    })
}

/// A PEM or PGP armor line that opens a private key, and its armor: through the
/// `-----END <same label>-----` line when only armor lines (`Name: value` headers, blank lines,
/// base64 lines) come between; else the header and the base64 lines of at least 16 characters
/// right after it. Text that is not armor (the next log line, another block's end) stays.
fn private_key(text: &str) -> impl Iterator<Item = Range<usize>> + '_ {
    let mut done = 0;
    text.match_indices("-----BEGIN ").filter_map(move |(i, m)| {
        if i < done {
            return None;
        }
        let from = i + m.len();
        let b = &text.as_bytes()[from..];
        let close = (0..b.len().min(64)).find(|&k| b[k..].starts_with(b"-----"))?;
        let label = &text[from..from + close];
        if !label.contains("PRIVATE KEY") || label.contains(['\n', '\r', '\\']) {
            return None;
        }
        let end = armor_end(text, from + close + 5, label);
        done = end;
        Some(i..end)
    })
}

/// Where the armor that starts at `at`, right after a `-----BEGIN <label>-----` line, ends
/// (see [`private_key`]). Line breaks are real or escaped as in a JSON log (`\n`, `\r\n`).
/// Every byte is read once: an armor ends at the first byte that is not armor.
fn armor_end(text: &str, mut at: usize, label: &str) -> usize {
    let b = text.as_bytes();
    let base64 = |c: u8| c.is_ascii_alphanumeric() || matches!(c, b'+' | b'/' | b'=');
    let name = |c: u8| c.is_ascii_alphanumeric() || c == b'-';
    let mut taken = at;
    let mut plain = true;
    while let Some(n) = line_break(b, at) {
        at += n;
        let rest = &text[at..];
        if let Some(tail) = rest.strip_prefix("-----END ") {
            if tail
                .strip_prefix(label)
                .is_some_and(|t| t.starts_with("-----"))
            {
                return at + "-----END ".len() + label.len() + 5;
            }
            break;
        }
        let len = run(b, at, base64, usize::MAX);
        if line_break(b, at + len).is_some() {
            if len > 0 && len < 16 {
                plain = false;
            }
            if plain && len > 0 {
                taken = at + len;
            }
            at += len;
            continue;
        }
        // A `Name: value` header line, before the base64 lines.
        let k = run(b, at, name, 64);
        if k > 0 && rest[k..].starts_with(": ") {
            let value = at + k + 2;
            let mut j = value;
            while j < b.len() && j - value < WINDOW && line_break(b, j).is_none() {
                j += 1;
            }
            if line_break(b, j).is_none() {
                break;
            }
            plain = false;
            at = j;
            continue;
        }
        break;
    }
    taken
}

/// The length of the line break at `at`: `\n`, `\r\n`, or one escaped as in JSON.
fn line_break(b: &[u8], at: usize) -> Option<usize> {
    let rest = b.get(at..)?;
    [&b"\r\n"[..], b"\n", b"\\r\\n", b"\\n"]
        .iter()
        .find(|nl| rest.starts_with(nl))
        .map(|nl| nl.len())
}

/// `scheme://user:password@host` with a password that is not a placeholder; the match is the
/// password. The authority ends at the first byte that cannot be inside a URL literal (space,
/// quote, `\`, `<`, `>`, `)`) or ends the authority (`/`, `?`, `#`), so it never reaches into
/// the next field of a JSON log, and two candidates never share a byte.
fn password_in_url(text: &str) -> impl Iterator<Item = Range<usize>> + '_ {
    text.match_indices("://").filter_map(move |(i, m)| {
        let at = i + m.len();
        let authority = text[at..]
            .split(|c: char| c.is_whitespace() || "/?#\"'`\\<>)".contains(c))
            .next()
            .unwrap_or_default();
        let (userinfo, _host) = authority.rsplit_once('@')?;
        let colon = userinfo.find(':')?;
        let pw = &userinfo[colon + 1..];
        (!pw.is_empty() && !placeholder(pw)).then_some(at + colon + 1..at + userinfo.len())
    })
}

/// Template syntax (`${X}`, `<x>`, `{x}`, `[x]`, `%X%`), a mask (`***`) or one repeated
/// character; `[x]` also covers a redaction marker. A URL escape such as `%74` is not a
/// placeholder.
fn placeholder(v: &str) -> bool {
    v.starts_with(['$', '<', '{', '[', '*'])
        || (v.len() > 2 && v.starts_with('%') && v.ends_with('%'))
        || v.bytes().all(|b| b == v.as_bytes()[0])
}

/// `Bearer <token>` (as spelled by `word`) with a random-looking token; the match is the
/// token.
fn bearer<'a>(text: &'a str, word: &'static str) -> impl Iterator<Item = Range<usize>> + 'a {
    text.match_indices(word).filter_map(move |(i, m)| {
        value_at(text, i + m.len())
            .filter(|v| random(v))
            .map(|v| span(text, v))
    })
}

/// A label with the secret word `w` right before a `=` or `:` (`DB_PASSWORD=...`,
/// `api_key: "..."`, or in prose `the deploy token is: ...`) and a random-looking literal after
/// it, which is the match. Only the label counts: in `the token parser landed in commit: <sha>`
/// the label is `commit`. The scan starts at each secret word, so separators with no label near
/// them cost nothing.
fn assignment<'a>(
    text: &'a str,
    lower: &'a str,
    w: &'static str,
) -> impl Iterator<Item = Range<usize>> + 'a {
    let b = lower.as_bytes();
    lower.match_indices(w).filter_map(move |(i, _)| {
        // The rest of the label word, bounded: a longer word is no label.
        let mut at = i + w.len();
        let n = run(b, at, label_char, 33);
        if n > 32 {
            return None;
        }
        at += n;
        at += run(b, at, |c| matches!(c, b' ' | b'\t' | b'"' | b'\''), 8);
        let link = run(b, at, label_char, 4);
        if link > 0 {
            if !LINKING_WORDS
                .iter()
                .any(|l| l.as_bytes() == &b[at..at + link])
            {
                return None;
            }
            at += link;
            at += run(b, at, |c| matches!(c, b' ' | b'\t'), 8);
        }
        if !matches!(b.get(at), Some(b'=' | b':')) {
            return None;
        }
        at += 1;
        at += run(b, at, |c| matches!(c, b'=' | b'>'), 1);
        value_at(text, at)
            .filter(|v| random(v))
            .map(|v| span(text, v))
    })
}

/// Where `sub`, a slice of `text`, lies in it.
fn span(text: &str, sub: &str) -> Range<usize> {
    let start = sub.as_ptr() as usize - text.as_ptr() as usize;
    start..start + sub.len()
}

/// The literal that starts at `at` after spaces and an optional quote (also one escaped as in
/// a JSON log, `\"`), read for at most [`WINDOW`] characters, or `None` when what follows is
/// code (a call or an index).
fn value_at(text: &str, at: usize) -> Option<&str> {
    let rest = text.get(at..)?.trim_start_matches([' ', '\t']);
    let rest = rest
        .strip_prefix('\\')
        .filter(|r| r.starts_with(['"', '\'']))
        .unwrap_or(rest);
    let rest = rest.strip_prefix(['"', '\'', '`']).unwrap_or(rest);
    let mut end = rest.len();
    for (n, (i, c)) in rest.char_indices().enumerate() {
        if n == WINDOW || c.is_whitespace() || "\"'`,;:=()[]{}<>\\".contains(c) {
            end = i;
            break;
        }
    }
    if rest[end..].starts_with(['(', '[']) {
        return None;
    }
    Some(rest[..end].trim_end_matches('.'))
}

/// At least 16 characters, letters and digits, no placeholder, and at least 3.5 bits of
/// Shannon entropy per character: what generated keys look like and words and ids do not.
fn random(v: &str) -> bool {
    if v.len() < 16
        || placeholder(v)
        || !v.bytes().any(|b| b.is_ascii_alphabetic())
        || !v.bytes().any(|b| b.is_ascii_digit())
    {
        return false;
    }
    let mut counts = std::collections::BTreeMap::new();
    for c in v.chars() {
        *counts.entry(c).or_insert(0usize) += 1;
    }
    let n = v.chars().count() as f64;
    let bits: f64 = counts
        .values()
        .map(|&k| {
            let p = k as f64 / n;
            -p * p.log2()
        })
        .sum();
    bits >= 3.5
}

#[cfg(test)]
mod tests {
    use super::{find, redact};

    /// Secret-shaped strings are built from parts, so this file holds none whole and a failing
    /// assertion prints only the case name and the pattern name.
    fn j(parts: &[&str]) -> String {
        parts.concat()
    }

    #[test]
    fn credentials_are_found_by_pattern_name() {
        let r20 = "Zq3vK9mW2xR7tY5pL8nB";
        let r36 = "Zq3vK9mW2xR7tY5pL8nB4cD6fG1hJ0kAa2Ss";
        for (case, text, want) in [
            (
                "aws",
                j(&["key AK", "IAZQ3VK9MW2XR7TY5P here"]),
                "aws-access-key",
            ),
            (
                "aws temp",
                j(&["AS", "IAZQ3VK9MW2XR7TY5P"]),
                "aws-access-key",
            ),
            ("github classic", j(&["gh", "p_", r36]), "github-token"),
            (
                "github app",
                j(&["token gh", "s_", r36, "."]),
                "github-token",
            ),
            (
                "github fine",
                j(&["github", "_pat_", r20, "_abc"]),
                "github-token",
            ),
            ("gitlab", j(&["gl", "pat-", r20]), "gitlab-token"),
            (
                "slack bot",
                j(&["xo", "xb-1234567890-", r20]),
                "slack-token",
            ),
            (
                "slack webhook",
                j(&["https://hooks.slack", ".com/services/T0123/B0456/", r20]),
                "slack-webhook",
            ),
            ("anthropic", j(&["sk-", "ant-api03-", r36]), "anthropic-key"),
            ("openai", j(&["sk-", "proj-", r36]), "openai-key"),
            (
                "google",
                j(&["AI", "za", r36[..35].as_ref()]),
                "google-api-key",
            ),
            ("stripe", j(&["sk_", "live_", r20]), "stripe-key"),
            ("npm", j(&["np", "m_", r36]), "npm-token"),
            (
                "jwt",
                j(&[
                    "ey",
                    "JhbGciOiJIUzI1NiJ9.ey",
                    "JzdWIiOiIxMjM0NTY3ODkwIn0.",
                    r20,
                ]),
                "jwt",
            ),
            (
                "rsa key",
                j(&["-----BEGIN RSA ", "PRIVATE KEY-----\nMIIE"]),
                "private-key",
            ),
            (
                "openssh key",
                j(&["-----BEGIN OPENSSH ", "PRIVATE KEY-----"]),
                "private-key",
            ),
            (
                "pgp key",
                j(&["-----BEGIN PGP ", "PRIVATE KEY BLOCK-----"]),
                "private-key",
            ),
            (
                "url password",
                j(&["postgres://app:", "s3cr3tPw@db.internal:5432/app"]),
                "password-in-url",
            ),
            (
                "assignment",
                j(&["DB_PASSWORD=", r20]),
                "high-entropy-assignment",
            ),
            (
                "quoted assignment",
                j(&["api_key: \"", r20, "\""]),
                "high-entropy-assignment",
            ),
            (
                "prose assignment",
                j(&["the deploy token is: ", r20, "."]),
                "high-entropy-assignment",
            ),
            (
                "bearer",
                j(&["Authorization: Bearer ", r36]),
                "bearer-token",
            ),
        ] {
            assert_eq!(find(&text), Some(want), "case `{case}`");
            let red = redact(&text);
            assert_eq!(find(&red), None, "case `{case}` after redaction");
            assert!(
                red.contains(&format!("[redacted:{want}]")),
                "case `{case}` names its pattern"
            );
        }
    }

    /// Only the credential goes: the text around it, the user and host of a URL, a label and
    /// the JSON a log keeps stay, so the log still reads. Overlapping matches leave one marker.
    #[test]
    fn redaction_replaces_each_match_and_keeps_the_rest() {
        let r20 = "Zq3vK9mW2xR7tY5pL8nB";
        let r36 = "Zq3vK9mW2xR7tY5pL8nB4cD6fG1hJ0kAa2Ss";
        let ghp = j(&["gh", "p_", r36]);
        for (case, text, want) in [
            (
                "two in prose",
                j(&["a ", &ghp, " b DB_PASSWORD=", r20, " c"]),
                "a [redacted:github-token] b DB_PASSWORD=[redacted:high-entropy-assignment] c"
                    .to_string(),
            ),
            (
                "url",
                j(&["postgres://app:", "s3cr3tPw@db.internal/app"]),
                "postgres://app:[redacted:password-in-url]@db.internal/app".to_string(),
            ),
            (
                "json log with an escaped quote",
                j(&[r#"{"key":""#, &ghp, r#"","body":"api_key: ""#, r20, r#"""}"#]),
                r#"{"key":"[redacted:github-token]","body":"api_key: "[redacted:high-entropy-assignment]""}"#
                    .to_string(),
            ),
            (
                "key block up to its end line",
                j(&["x -----BEGIN RSA ", "PRIVATE KEY-----\nMIIE\n-----END RSA PRIVATE KEY----- y"]),
                "x [redacted:private-key] y".to_string(),
            ),
            (
                "key header and a short line",
                j(&["x -----BEGIN RSA ", "PRIVATE KEY-----\nMIIE y"]),
                "x [redacted:private-key]\nMIIE y".to_string(),
            ),
            (
                "key header and text on its line",
                j(&["x -----BEGIN RSA ", "PRIVATE KEY----- ", &"y".repeat(9000), " tail"]),
                format!("x [redacted:private-key] {} tail", "y".repeat(9000)),
            ),
            // Review round 1: only the armor goes, never the diagnostics around it.
            (
                "key header, then the next log line",
                j(&["Documentation: -----BEGIN RSA ", "PRIVATE KEY-----\nnext log line: migration completed"]),
                "Documentation: [redacted:private-key]\nnext log line: migration completed".to_string(),
            ),
            (
                "key body without an end line, then the next log line",
                j(&["-----BEGIN RSA ", "PRIVATE KEY-----\nMIIEowIBAAKCAQEAx1mZ\nAAKCAQEAx1mZMIIEow==\nnext: done\n"]),
                "[redacted:private-key]\nnext: done\n".to_string(),
            ),
            (
                "an end line of another label",
                j(&["-----BEGIN RSA ", "PRIVATE KEY-----\nMIIE\n-----END CERTIFICATE-----\nlater"]),
                "[redacted:private-key]\nMIIE\n-----END CERTIFICATE-----\nlater".to_string(),
            ),
            (
                "a JSON-escaped block with headers, between fields",
                j(&[r#"{"body":"x -----BEGIN RSA "#, r#"PRIVATE KEY-----\nProc-Type: 4,ENCRYPTED\n\nMIIEowIBAAKCAQEAx1mZ\nAx==\n-----END RSA PRIVATE KEY----- y","next":"z"}"#]),
                r#"{"body":"x [redacted:private-key] y","next":"z"}"#.to_string(),
            ),
            (
                "url with a port, then a JSON field with an address",
                r#"{"url":"https://example.org:8443","contact":"hello@example.org"}"#.to_string(),
                r#"{"url":"https://example.org:8443","contact":"hello@example.org"}"#.to_string(),
            ),
            (
                "url with a port in parentheses, then an address",
                "(see https://example.org:8443) or write to hello@example.org".to_string(),
                "(see https://example.org:8443) or write to hello@example.org".to_string(),
            ),
            (
                "overlap",
                j(&["Bearer ", &ghp, " z"]),
                "Bearer [redacted:github-token] z".to_string(),
            ),
            ("nothing", "plain text, sk-learn".to_string(), "plain text, sk-learn".to_string()),
        ] {
            assert_eq!(redact(&text), want, "case `{case}`");
        }
    }

    /// Segments of any length: a token with many claims has a payload far over any window.
    #[test]
    fn a_jwt_with_a_long_payload_is_a_jwt() {
        let header = "eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9";
        for claims in [10, 444, 5000] {
            let payload = [
                "ey",
                "J",
                &"c2NvcGUiOiJyZWFk".repeat(claims / 16 + 1)[..claims - 3],
            ]
            .concat();
            let text = [
                "token ",
                header,
                ".",
                &payload,
                ".",
                "Zq3vK9mW2xR7tY5pL8nB4cD6fG1hJ0kAa2Ss",
                " end",
            ]
            .concat();
            assert_eq!(find(&text), Some("jwt"), "payload of {claims} characters");
        }
        // Near-misses: two segments, or a second segment that is no JSON object.
        assert_eq!(
            find(&["ey", "JhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxMjM0NTY3ODkwIn0"].concat()),
            None
        );
        assert_eq!(
            find(&["ey", "JhbGciOiJIUzI1NiJ9.abcdefghijklmnop.qrstuvwxyz0123"].concat()),
            None
        );
    }

    #[test]
    fn a_percent_encoded_url_password_is_a_password() {
        assert_eq!(
            find("https://app:%74est123@example.org/path"),
            Some("password-in-url")
        );
    }

    /// Every scan is linear: 1 MiB of separator- and prefix-heavy text finishes well within a
    /// bound a quadratic scan (seconds per 256 KiB in a release build) cannot meet.
    #[test]
    fn scanning_is_linear_in_the_input() {
        let mib = 1 << 20;
        for (case, unit) in [
            ("colons", ":"),
            ("equals", "="),
            ("labelled colons", "token:"),
            ("labelled words", "password is: "),
            ("bearer", "Bearer "),
            ("urls", "://a:"),
            ("aws prefix", "AKIA"),
            ("sk prefix", " sk-"),
            ("jwt", "eyJ."),
            ("jwt segments", "eyJabcdefghij."),
            ("key header", "-----BEGIN "),
            // Review round 1: dense matches, each one redacted.
            ("dense tokens", j(&["AK", "IAZQ3VK9MW2XR7TY5P "]).as_str()),
            (
                "dense assignments",
                j(&["DB_PASSWORD=", "Zq3vK9mW2xR7tY5pL8nB "]).as_str(),
            ),
            ("dense key headers", "-----BEGIN RSA PRIVATE KEY-----\n"),
        ] {
            let text = unit.repeat(mib / unit.len());
            let started = std::time::Instant::now();
            let _ = find(&text);
            let red = redact(&text);
            let took = started.elapsed();
            assert!(
                took < std::time::Duration::from_secs(3),
                "case `{case}` took {took:?}"
            );
            assert_eq!(find(&red), None, "case `{case}` after redaction");
        }
    }

    #[test]
    fn near_misses_are_not_secrets() {
        for (case, text) in [
            (
                "prose",
                "The deploy token lives in Vault; ask the platform team.",
            ),
            ("short password", "password: hunter2"),
            (
                "placeholder url",
                "postgres://app:${DB_PASSWORD}@db.internal/app",
            ),
            ("angle placeholder", "https://user:<password>@example.com"),
            (
                "url without password",
                "https://deploy@example.com/repo.git",
            ),
            ("url with port", "https://example.com:8443/api"),
            (
                "scp remote",
                "git@github.com:lanternfish-dev/lanternfish.git",
            ),
            ("public key", "-----BEGIN PUBLIC KEY-----"),
            ("certificate", "-----BEGIN CERTIFICATE-----"),
            (
                "word before sk",
                "task-scheduler-for-nightly-builds-and-reports",
            ),
            ("short sk", "uses sk-learn for ranking"),
            // Review round 1: ordinary references and prose.
            (
                "sk path",
                "file:crates/sk-learn-integration-tests/main.rs@a1",
            ),
            (
                "sha after a non-secret label",
                "The token parser landed in commit: 9f86d081884c7d659a2feaa0c55ad015a3bf4f1b",
            ),
            (
                "id after a non-secret label",
                "The token parser tracks request ID: req-03fE8B2b6A7dD9c1a",
            ),
            ("webhook stub", "https://hooks.slack.com/services/TEST"),
            ("env placeholder", "password = %DB_PASSWORD%"),
            ("aws too short", "AKIA1234"),
            ("aws inside word", "XAKIAZQ3VK9MW2XR7TY5PQ"),
            ("ghp short", "ghp_abc"),
            ("code call", "secret = config.get_secret_value_v2()"),
            (
                "env lookup",
                "api_key = os.environ[\"API_KEY_2024_PRIMARY\"]",
            ),
            ("letters only", "token = refresh_token_expiry_seconds_total"),
            ("digits only", "token_ttl = 36000000000000000000"),
            ("low entropy", "password = aaaaaaaaaaaaaaaa1111"),
            ("masked", "password = ****************"),
            (
                "bearer word",
                "Authorization uses a Bearer token from the identity provider",
            ),
            ("commit ref", "commit:lanternfish-dev/lanternfish@06aff55"),
            ("file ref", "file:src/store.rs#L40-L80@a4586a6"),
            (
                "sha in prose",
                "fixed in 9f86d081884c7d659a2feaa0c55ad015a3bf4f1b",
            ),
        ] {
            assert_eq!(find(text), None, "case `{case}`");
            assert_eq!(redact(text), text, "case `{case}` is kept as it is");
        }
    }
}
