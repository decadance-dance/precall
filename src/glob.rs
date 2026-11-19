//! The `include` and `exclude` globs of the `files` collector (spec §10): `*` and `?` match
//! within one path segment, `**` as a whole segment matches any number of segments, none
//! included. Paths use `/`.

/// Whether `path` matches `pattern`. Each pair of positions is decided once, so the work is
/// bounded by the pattern's length times the path's, whatever the stars.
pub fn matches(pattern: &str, path: &str) -> bool {
    let pat: Vec<&str> = pattern.split('/').collect();
    let segs: Vec<&str> = path.split('/').collect();
    let mut memo = vec![None; (pat.len() + 1) * (segs.len() + 1)];
    segments(&pat, &segs, 0, 0, &mut memo)
}

fn segments(p: &[&str], s: &[&str], pi: usize, si: usize, memo: &mut [Option<bool>]) -> bool {
    let at = pi * (s.len() + 1) + si;
    if let Some(r) = memo[at] {
        return r;
    }
    let r = match p.get(pi) {
        None => si == s.len(),
        // `**` matches no segment, or one more and stays.
        Some(&"**") => {
            segments(p, s, pi + 1, si, memo) || (si < s.len() && segments(p, s, pi, si + 1, memo))
        }
        Some(seg) => {
            si < s.len()
                && segment(seg.as_bytes(), s[si].as_bytes())
                && segments(p, s, pi + 1, si + 1, memo)
        }
    };
    memo[at] = Some(r);
    r
}

fn segment(p: &[u8], s: &[u8]) -> bool {
    let mut memo = vec![None; (p.len() + 1) * (s.len() + 1)];
    bytes(p, s, 0, 0, &mut memo)
}

fn bytes(p: &[u8], s: &[u8], pi: usize, si: usize, memo: &mut [Option<bool>]) -> bool {
    let at = pi * (s.len() + 1) + si;
    if let Some(r) = memo[at] {
        return r;
    }
    let r = match p.get(pi) {
        None => si == s.len(),
        // `*` matches nothing, or one more byte and stays.
        Some(b'*') => {
            bytes(p, s, pi + 1, si, memo) || (si < s.len() && bytes(p, s, pi, si + 1, memo))
        }
        Some(b'?') => si < s.len() && bytes(p, s, pi + 1, si + 1, memo),
        Some(c) => s.get(si) == Some(c) && bytes(p, s, pi + 1, si + 1, memo),
    };
    memo[at] = Some(r);
    r
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

    #[test]
    fn many_stars_on_a_long_name_stay_fast() {
        // Review round 1, finding 4: this took longer than two seconds.
        let name = format!("{}.md", "a".repeat(32));
        let deep = vec!["d"; 40].join("/") + "/x.md";
        let started = std::time::Instant::now();
        assert!(!matches(&format!("{}b.md", "*".repeat(16)), &name));
        assert!(!matches(&format!("{}y.md", "**/".repeat(20)), &deep));
        assert!(matches(&format!("{}x.md", "**/".repeat(20)), &deep));
        assert!(started.elapsed() < std::time::Duration::from_millis(200));
    }
}
