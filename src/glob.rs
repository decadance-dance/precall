//! The `include` and `exclude` globs of the `files` collector (spec §10): `*` and `?` match
//! within one path segment, `**` as a whole segment matches any number of segments, none
//! included. Paths use `/`.

/// Whether `path` matches `pattern`.
pub fn matches(pattern: &str, path: &str) -> bool {
    let pat: Vec<&str> = pattern.split('/').collect();
    let segs: Vec<&str> = path.split('/').collect();
    segments(&pat, &segs)
}

fn segments(pat: &[&str], path: &[&str]) -> bool {
    match pat.split_first() {
        None => path.is_empty(),
        Some((&"**", rest)) => (0..=path.len()).any(|i| segments(rest, &path[i..])),
        Some((p, rest)) => path
            .split_first()
            .is_some_and(|(s, tail)| segment(p.as_bytes(), s.as_bytes()) && segments(rest, tail)),
    }
}

fn segment(p: &[u8], s: &[u8]) -> bool {
    match p.split_first() {
        None => s.is_empty(),
        Some((b'*', rest)) => (0..=s.len()).any(|i| segment(rest, &s[i..])),
        Some((b'?', rest)) => !s.is_empty() && segment(rest, &s[1..]),
        Some((c, rest)) => s.first() == Some(c) && segment(rest, &s[1..]),
    }
}

#[cfg(test)]
mod tests {
    use super::matches;

    #[test]
    fn stars_stay_in_a_segment_and_double_stars_cross_them() {
        for (pat, path, want) in [
            ("**", "a.md", true),
            ("**", "docs/deep/a.md", true),
            ("*.md", "a.md", true),
            ("*.md", "docs/a.md", false),
            ("docs/*.md", "docs/a.md", true),
            ("docs/*.md", "docs/x/a.md", false),
            ("docs/**/*.md", "docs/a.md", true),
            ("docs/**/*.md", "docs/x/y/a.md", true),
            ("docs/**", "docs/x/y/a.md", true),
            ("docs/**", "src/a.md", false),
            ("**/*.lock", "Cargo.lock", true),
            ("**/target/**", "a/target/b/c", true),
            ("?.rs", "a.rs", true),
            ("?.rs", "ab.rs", false),
            ("a*b*c", "aXXbYc", true),
            ("a*b*c", "aXXbY", false),
            ("README.md", "README.md", true),
            ("README.md", "readme.md", false),
        ] {
            assert_eq!(matches(pat, path), want, "{pat} {path}");
        }
    }
}
