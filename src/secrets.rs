//! What the built-in `secrets` check looks for (spec §10): common token and key formats,
//! private key blocks, passwords in URLs and high-entropy assignments. It names the pattern a
//! text matches and never returns the matched text.

/// A token that starts with a fixed prefix at a word boundary and goes on with at least `min`
/// characters of its alphabet (exactly `min` when `exact`, ended by a non-word character).
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

/// Words that make the value after a `=` or `:` a credential when the value looks random.
const SECRET_WORDS: [&str; 8] = [
    "password", "passwd", "pwd", "secret", "token", "api_key", "apikey", "api-key",
];

/// The name of the first pattern `text` matches.
pub fn find(text: &str) -> Option<&'static str> {
    let b = text.as_bytes();
    for p in PREFIXED {
        for prefix in p.prefixes {
            for (i, _) in text.match_indices(prefix) {
                if i > 0 && word(b[i - 1]) {
                    continue;
                }
                let start = i + prefix.len();
                let n = run(b, start, p.alphabet);
                let ok = if p.exact {
                    n == p.min && b.get(start + n).is_none_or(|c| !word(*c))
                } else {
                    n >= p.min
                };
                if ok {
                    return Some(p.name);
                }
            }
        }
    }
    if text.contains("hooks.slack.com/services/T") {
        return Some("slack-webhook");
    }
    if jwt(text) {
        return Some("jwt");
    }
    if private_key(text) {
        return Some("private-key");
    }
    if password_in_url(text) {
        return Some("password-in-url");
    }
    if bearer(text) {
        return Some("bearer-token");
    }
    if assignment(text) {
        return Some("high-entropy-assignment");
    }
    None
}

/// How many bytes from `start` on belong to `alphabet`.
fn run(b: &[u8], start: usize, alphabet: fn(u8) -> bool) -> usize {
    b.get(start..)
        .map_or(0, |rest| rest.iter().take_while(|c| alphabet(**c)).count())
}

/// Three dot-separated base64url segments, the first two starting with `eyJ` (a JSON object).
fn jwt(text: &str) -> bool {
    let b = text.as_bytes();
    text.match_indices("eyJ").any(|(i, _)| {
        if i > 0 && word(b[i - 1]) {
            return false;
        }
        let mut at = i;
        for seg in 0..3 {
            if seg == 1 && !b[at..].starts_with(b"eyJ") {
                return false;
            }
            let n = run(b, at, word);
            if n < 10 {
                return false;
            }
            at += n;
            if seg < 2 {
                if b.get(at) != Some(&b'.') {
                    return false;
                }
                at += 1;
            }
        }
        true
    })
}

/// A PEM or PGP armor line that opens a private key.
fn private_key(text: &str) -> bool {
    text.match_indices("-----BEGIN ").any(|(i, m)| {
        let rest = &text[i + m.len()..];
        rest.split("-----")
            .next()
            .is_some_and(|label| label.contains("PRIVATE KEY") && !label.contains('\n'))
    })
}

/// `scheme://user:password@host` with a password that is not a placeholder.
fn password_in_url(text: &str) -> bool {
    text.match_indices("://").any(|(i, m)| {
        let authority = text[i + m.len()..]
            .split(|c: char| c.is_whitespace() || matches!(c, '/' | '?' | '#'))
            .next()
            .unwrap_or_default();
        let Some((userinfo, _host)) = authority.rsplit_once('@') else {
            return false;
        };
        userinfo
            .split_once(':')
            .is_some_and(|(_, pw)| !pw.is_empty() && !placeholder(pw))
    })
}

fn placeholder(v: &str) -> bool {
    v.starts_with(['$', '<', '{', '%', '*']) || v.bytes().all(|b| b == v.as_bytes()[0])
}

/// `Bearer <token>` with a random-looking token.
fn bearer(text: &str) -> bool {
    text.match_indices("Bearer ")
        .chain(text.match_indices("bearer "))
        .any(|(i, m)| value_at(text, i + m.len()).is_some_and(random))
}

/// A secret word shortly before a `=` or `:` on the same line, and a random-looking literal
/// after it (`DB_PASSWORD=...`, `api_key: "..."`, `the deploy token is: ...`).
fn assignment(text: &str) -> bool {
    text.match_indices(['=', ':']).any(|(i, _)| {
        let line_start = text[..i].rfind('\n').map_or(0, |n| n + 1);
        let mut from = i.saturating_sub(32).max(line_start);
        while !text.is_char_boundary(from) {
            from += 1;
        }
        let key = text[from..i].to_ascii_lowercase();
        SECRET_WORDS.iter().any(|w| key.contains(w)) && value_at(text, i + 1).is_some_and(random)
    })
}

/// The literal that starts at `at` after spaces and an optional quote, or `None` when what
/// follows is code (a call or an index) rather than a literal.
fn value_at(text: &str, at: usize) -> Option<&str> {
    let rest = text.get(at..)?.trim_start_matches([' ', '\t']);
    let rest = rest.strip_prefix(['"', '\'', '`']).unwrap_or(rest);
    let end = rest
        .find(|c: char| c.is_whitespace() || "\"'`,;()[]{}<>".contains(c))
        .unwrap_or(rest.len());
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
    use super::find;

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
        }
    }
}
