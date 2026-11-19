//! What the built-in `secrets` check looks for (spec §10): common token and key formats,
//! private key blocks, passwords in URLs and high-entropy assignments. It names the pattern a
//! text matches and never returns the matched text; [`redact`] replaces every match in what the
//! core keeps of a plugin's output (spec §6.3).
//!
//! Every scan reads a bounded window from each candidate position and skips candidates inside
//! a token it already read, so the time is linear in the text: a check runs over every
//! proposal of a run before it answers (spec §6.5).

/// The most characters a value or token is read for; real credentials are shorter.
use std::ops::Range;

const WINDOW: usize = 256;

/// How far a private key without an end line is redacted: more than an RSA-8192 PEM block.
const KEY_SPAN: usize = 8 << 10;

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

/// One match: the bytes it covers and the pattern's name.
type Hit = (Range<usize>, &'static str);

/// The name of the first pattern `text` matches.
pub fn find(text: &str) -> Option<&'static str> {
    hits(text).first().map(|h| h.1)
}

/// `text` with every match replaced by `[redacted:<pattern>]`; overlapping matches leave one
/// marker. What the core keeps of a plugin's output goes through this (spec §6.3).
pub fn redact(text: &str) -> String {
    let mut hits = hits(text);
    if hits.is_empty() {
        return text.to_string();
    }
    // Stable: at one start the pattern tried first names the marker.
    hits.sort_by_key(|h| (h.0.start, std::cmp::Reverse(h.0.end)));
    let mut out = String::with_capacity(text.len());
    let mut at = 0;
    for (r, name) in hits {
        if r.start >= at {
            out += &text[at..r.start];
            out += &format!("[redacted:{name}]");
        }
        at = at.max(r.end);
    }
    out += &text[at..];
    out
}

/// Every match, pattern by pattern in the order [`find`] tries them.
fn hits(text: &str) -> Vec<Hit> {
    let mut out = Vec::new();
    prefixed(text, &mut out);
    slack_webhook(text, &mut out);
    jwt(text, &mut out);
    private_key(text, &mut out);
    password_in_url(text, &mut out);
    bearer(text, &mut out);
    assignment(text, &mut out);
    out
}

/// The tokens of [`PREFIXED`], prefix included.
fn prefixed(text: &str, out: &mut Vec<Hit>) {
    let b = text.as_bytes();
    for p in PREFIXED {
        for prefix in p.prefixes {
            for (i, _) in text.match_indices(prefix) {
                if i > 0 && word(b[i - 1]) {
                    continue;
                }
                let start = i + prefix.len();
                let tok = &b[start..start + run(b, start, p.alphabet, WINDOW)];
                let ok = if p.exact {
                    tok.len() == p.min && b.get(start + p.min).is_none_or(|c| !word(*c))
                } else {
                    tok.len() >= p.min && shaped(tok)
                };
                if ok {
                    out.push((i..start + tok.len(), p.name));
                }
            }
        }
    }
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
fn slack_webhook(text: &str, out: &mut Vec<Hit>) {
    let b = text.as_bytes();
    let path_end = |mut at: usize| {
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
    for (i, m) in text.match_indices("hooks.slack.com/services/") {
        if let Some(end) = path_end(i + m.len()) {
            out.push((i..end, "slack-webhook"));
        }
    }
}

/// Three dot-separated base64url segments of at least 10 characters, the first two starting
/// with `eyJ` (a JSON object). One pass over the maximal segments, each read once and never
/// capped: a token with many claims has a long payload.
fn jwt(text: &str, out: &mut Vec<Hit>) {
    let b = text.as_bytes();
    // The last two segments seen, as (start, end), while each followed the one before by a
    // single `.`.
    let mut chain: Vec<(usize, usize)> = Vec::with_capacity(2);
    let mut i = 0;
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
                out.push((chain[0].0..i, "jwt"));
                chain.clear();
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
}

/// A PEM or PGP armor line that opens a private key, through its `-----END ...-----` line, or
/// for [`KEY_SPAN`] bytes when none follows: what comes after the header is the key, and a
/// header alone (a title cut short in a report) does not take the rest of the text with it.
fn private_key(text: &str, out: &mut Vec<Hit>) {
    let mut done = 0;
    // Once no end line follows, none follows a later header either: search no more, so many
    // headers without one stay linear.
    let mut ends = true;
    for (i, m) in text.match_indices("-----BEGIN ") {
        if i < done {
            continue;
        }
        let label: String = text[i + m.len()..].chars().take(64).collect();
        let opens = label
            .split("-----")
            .next()
            .is_some_and(|l| l.contains("PRIVATE KEY") && !l.contains('\n'));
        if !opens {
            continue;
        }
        let end = ends
            .then(|| text[i..].find("-----END "))
            .flatten()
            .map(|e| i + e + "-----END ".len())
            .and_then(|from| text[from..].find("-----").map(|f| from + f + 5))
            .unwrap_or_else(|| {
                ends = false;
                let mut end = text.len().min(i + KEY_SPAN);
                while !text.is_char_boundary(end) {
                    end += 1;
                }
                end
            });
        out.push((i..end, "private-key"));
        done = end;
    }
}

/// `scheme://user:password@host` with a password that is not a placeholder; the match is the
/// password. The authority ends at the next `/`, so two candidates never share a byte.
fn password_in_url(text: &str, out: &mut Vec<Hit>) {
    for (i, m) in text.match_indices("://") {
        let at = i + m.len();
        let authority = text[at..]
            .split(|c: char| c.is_whitespace() || matches!(c, '/' | '?' | '#'))
            .next()
            .unwrap_or_default();
        let Some((userinfo, _host)) = authority.rsplit_once('@') else {
            continue;
        };
        let Some(colon) = userinfo.find(':') else {
            continue;
        };
        let pw = &userinfo[colon + 1..];
        if !pw.is_empty() && !placeholder(pw) {
            out.push((at + colon + 1..at + userinfo.len(), "password-in-url"));
        }
    }
}

/// Template syntax (`${X}`, `<x>`, `{x}`, `[x]`, `%X%`), a mask (`***`) or one repeated
/// character; `[x]` also covers a redaction marker. A URL escape such as `%74` is not a
/// placeholder.
fn placeholder(v: &str) -> bool {
    v.starts_with(['$', '<', '{', '[', '*'])
        || (v.len() > 2 && v.starts_with('%') && v.ends_with('%'))
        || v.bytes().all(|b| b == v.as_bytes()[0])
}

/// `Bearer <token>` with a random-looking token; the match is the token.
fn bearer(text: &str, out: &mut Vec<Hit>) {
    for (i, m) in text
        .match_indices("Bearer ")
        .chain(text.match_indices("bearer "))
    {
        if let Some(v) = value_at(text, i + m.len()).filter(|v| random(v)) {
            out.push((span(text, v), "bearer-token"));
        }
    }
}

/// A secret-named label right before a `=` or `:` (`DB_PASSWORD=...`, `api_key: "..."`, or in
/// prose `the deploy token is: ...`) and a random-looking literal after it, which is the
/// match. Only the label counts: in `the token parser landed in commit: <sha>` the label is
/// `commit`. The scan starts at each secret word, so separators with no label near them cost
/// nothing.
fn assignment(text: &str, out: &mut Vec<Hit>) {
    let lower = text.to_ascii_lowercase();
    let b = lower.as_bytes();
    let value = |i: usize, w: &str| {
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
        value_at(text, at).filter(|v| random(v))
    };
    for w in SECRET_WORDS {
        for (i, _) in lower.match_indices(w) {
            if let Some(v) = value(i, w) {
                out.push((span(text, v), "high-entropy-assignment"));
            }
        }
    }
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
                "key block without an end line",
                j(&["x -----BEGIN RSA ", "PRIVATE KEY-----\nMIIE y"]),
                "x [redacted:private-key]".to_string(),
            ),
            (
                "key header without an end line, then more text",
                j(&["x -----BEGIN RSA ", "PRIVATE KEY----- ", &"y".repeat(9000), " tail"]),
                // The header is 32 characters; the redaction stops 8 KiB after it starts.
                format!("x [redacted:private-key]{} tail", "y".repeat(9032 - 8192)),
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
        ] {
            let text = unit.repeat(mib / unit.len());
            let started = std::time::Instant::now();
            let _ = find(&text);
            let _ = redact(&text);
            let took = started.elapsed();
            assert!(
                took < std::time::Duration::from_secs(3),
                "case `{case}` took {took:?}"
            );
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
