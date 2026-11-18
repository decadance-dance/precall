//! The node of spec §3.1 and the checks on its fields.

use serde::{Deserialize, Serialize};

pub const TITLE_MAX: usize = 80;
pub const BODY_MAX: usize = 600;
pub const STATES: [&str; 3] = ["open", "blocked", "done"];

/// One map node. Field order is the key order of a map line (spec §3.2).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Node {
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent: Option<String>,
    pub title: String,
    pub body: String,
    pub sources: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub date: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub state: Option<String>,
    pub order: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

impl Node {
    /// Checks a stored (non-branch) node; the error names the field.
    pub fn validate(&self) -> Result<(), String> {
        if !is_slug(&self.id) {
            return Err(format!("id `{}` is not a slug (a-z, 0-9, single dashes)", self.id));
        }
        match &self.parent {
            Some(p) if is_slug(p) => {}
            Some(p) => return Err(format!("parent `{p}` is not a slug")),
            None => return Err("parent is missing".into()),
        }
        check_text("title", &self.title, TITLE_MAX)?;
        if self.title.trim().is_empty() {
            return Err("title is empty".into());
        }
        check_text("body", &self.body, BODY_MAX)?;
        if self.sources.is_empty() {
            return Err("sources: at least one source reference is required".into());
        }
        for s in &self.sources {
            check_source(s)?;
        }
        if let Some(d) = &self.date {
            check_date(d)?;
        }
        if let Some(st) = &self.state
            && !STATES.contains(&st.as_str())
        {
            return Err(format!("state `{st}` is not one of {}", STATES.join(", ")));
        }
        Ok(())
    }
}

fn check_text(field: &str, s: &str, max: usize) -> Result<(), String> {
    let n = s.chars().count();
    if n > max {
        return Err(format!("{field} has {n} characters, at most {max} allowed"));
    }
    if s.chars().any(char::is_control) {
        return Err(format!("{field} holds a line break or control character"));
    }
    Ok(())
}

pub fn is_slug(s: &str) -> bool {
    !s.is_empty()
        && s.split('-')
            .all(|part| !part.is_empty() && part.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit()))
}

/// `<kind>:<locator>` (spec §3.3).
pub fn check_source(r: &str) -> Result<(), String> {
    let bad = || Err(format!("source `{r}` is not `<kind>:<locator>`"));
    let Some((kind, locator)) = r.split_once(':') else {
        return bad();
    };
    let kind_ok = kind.starts_with(|c: char| c.is_ascii_lowercase())
        && kind.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-');
    if !kind_ok || locator.is_empty() || locator.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return bad();
    }
    // limit: M1 checks the format only; that the kind has an owning collector comes with plugins (M2).
    Ok(())
}

pub fn check_date(d: &str) -> Result<(), String> {
    let b = d.as_bytes();
    let digits = |r: std::ops::Range<usize>| b[r].iter().all(u8::is_ascii_digit);
    let shape = b.len() == 10 && b[4] == b'-' && b[7] == b'-' && digits(0..4) && digits(5..7) && digits(8..10);
    let in_range = shape && {
        let (m, day): (u32, u32) = (d[5..7].parse().unwrap_or(0), d[8..10].parse().unwrap_or(0));
        (1..=12).contains(&m) && (1..=31).contains(&day)
    };
    if in_range { Ok(()) } else { Err(format!("date `{d}` is not YYYY-MM-DD")) }
}

/// A slug from `title` that `taken` does not hold yet.
pub fn unique_slug(title: &str, taken: impl Fn(&str) -> bool) -> String {
    const MAX: usize = 48;
    let mut slug = String::new();
    for c in title.chars() {
        if c.is_ascii_alphanumeric() {
            slug.push(c.to_ascii_lowercase());
        } else if !slug.is_empty() && !slug.ends_with('-') {
            slug.push('-');
        }
    }
    if slug.len() > MAX {
        let cut = if slug.as_bytes()[MAX] == b'-' { MAX } else { slug[..MAX].rfind('-').unwrap_or(MAX) };
        slug.truncate(cut);
    }
    let slug = slug.trim_end_matches('-');
    let base = if slug.is_empty() { "node" } else { slug };
    if !taken(base) {
        return base.to_string();
    }
    (2..).map(|n| format!("{base}-{n}")).find(|s| !taken(s)).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn node() -> Node {
        Node {
            id: "search-service".into(),
            parent: Some("layout".into()),
            title: "Search service".into(),
            body: "Answers queries.".into(),
            sources: vec!["import:outline.txt".into()],
            date: None,
            state: None,
            order: 0,
            note: None,
        }
    }

    #[test]
    fn valid_node_passes() {
        assert_eq!(node().validate(), Ok(()));
    }

    #[test]
    fn title_limit_counts_chars() {
        let mut n = node();
        n.title = "é".repeat(TITLE_MAX);
        assert_eq!(n.validate(), Ok(()));
        n.title.push('é');
        assert!(n.validate().unwrap_err().contains("title"));
        n.title = String::new();
        assert!(n.validate().is_err());
        n.title = "two\nlines".into();
        assert!(n.validate().is_err());
    }

    #[test]
    fn body_limit() {
        let mut n = node();
        n.body = "x".repeat(BODY_MAX);
        assert_eq!(n.validate(), Ok(()));
        n.body.push('x');
        assert!(n.validate().unwrap_err().contains("body"));
    }

    #[test]
    fn sources_required_and_checked() {
        let mut n = node();
        n.sources.clear();
        assert!(n.validate().unwrap_err().contains("source"));
        for good in [
            "file:src/store.rs#L40-L80@a4586a6",
            "gh-pr:owner/repo#212",
            "slack:C0123ABCDEF/1795033170.000100",
            "config:rrecall.toml",
        ] {
            assert_eq!(check_source(good), Ok(()), "{good}");
        }
        for bad in ["nokind", ":x", "Kind:x", "file:", "file:a b", "1x:y"] {
            assert!(check_source(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn date_and_state() {
        assert_eq!(check_date("2026-02-28"), Ok(()));
        for bad in ["2026-2-28", "2026-13-01", "2026-00-10", "2026-01-32", "26-01-01", "2026-01-01T"] {
            assert!(check_date(bad).is_err(), "{bad}");
        }
        let mut n = node();
        n.state = Some("blocked".into());
        assert_eq!(n.validate(), Ok(()));
        n.state = Some("stuck".into());
        assert!(n.validate().unwrap_err().contains("state"));
    }

    #[test]
    fn ids_must_be_slugs_with_a_parent() {
        let mut n = node();
        n.id = "Search service".into();
        assert!(n.validate().unwrap_err().contains("id"));
        let mut n = node();
        n.parent = None;
        assert!(n.validate().unwrap_err().contains("parent"));
        assert!(is_slug("a-1-b"));
        for bad in ["", "-a", "a-", "a--b", "A", "a_b"] {
            assert!(!is_slug(bad), "{bad}");
        }
    }

    #[test]
    fn unique_slug_from_title() {
        assert_eq!(unique_slug("Search Service (v2)!", |_| false), "search-service-v2");
        assert_eq!(unique_slug("Ünïcode only", |_| false), "n-code-only");
        assert_eq!(unique_slug("!!!", |_| false), "node");
        let taken = ["search", "search-2"];
        assert_eq!(unique_slug("Search", |s| taken.contains(&s)), "search-3");
        let long = "word ".repeat(30);
        let s = unique_slug(&long, |_| false);
        assert!(s.len() <= 48 && is_slug(&s), "{s}");
    }

    #[test]
    fn line_keys_in_fixed_order() {
        let mut n = node();
        n.note = Some("mine".into());
        n.date = Some("2026-01-02".into());
        assert_eq!(
            serde_json::to_string(&n).unwrap(),
            r#"{"id":"search-service","parent":"layout","title":"Search service","body":"Answers queries.","sources":["import:outline.txt"],"date":"2026-01-02","order":0,"note":"mine"}"#
        );
    }
}
