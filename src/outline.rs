//! The outline `rrecall import` reads: one `title | detail` per line, nesting by leading spaces.

#[derive(Debug, PartialEq)]
pub struct Item {
    /// 1-based line number, for error messages.
    pub line: usize,
    pub title: String,
    pub detail: String,
    pub children: Vec<Item>,
}

/// The top-level items; an error names the line.
pub fn parse(text: &str) -> Result<Vec<Item>, String> {
    let mut roots = Vec::new();
    // Open items with their indent, outermost first.
    let mut stack: Vec<(usize, Item)> = Vec::new();
    fn close(stack: &mut Vec<(usize, Item)>, roots: &mut Vec<Item>) {
        if let Some((_, item)) = stack.pop() {
            match stack.last_mut() {
                Some((_, parent)) => parent.children.push(item),
                None => roots.push(item),
            }
        }
    }
    for (i, raw) in text.lines().enumerate() {
        let line = i + 1;
        let content = raw.trim_start_matches(' ');
        if content.trim().is_empty() {
            continue;
        }
        if content.starts_with(char::is_whitespace) {
            return Err(format!("line {line}: indent with spaces only"));
        }
        let indent = raw.len() - content.len();
        let mut sibling_indent = None;
        while stack.last().is_some_and(|(d, _)| *d >= indent) {
            sibling_indent = stack.last().map(|(d, _)| *d);
            close(&mut stack, &mut roots);
        }
        if sibling_indent.is_some_and(|d| d != indent) {
            return Err(format!("line {line}: indent of {indent} spaces matches no level above it"));
        }
        let (title, detail) = content.split_once('|').unwrap_or((content, ""));
        let (title, detail) = (title.trim(), detail.trim());
        if title.is_empty() {
            return Err(format!("line {line}: the title before `|` is empty"));
        }
        stack.push((indent, Item { line, title: title.into(), detail: detail.into(), children: Vec::new() }));
    }
    while !stack.is_empty() {
        close(&mut stack, &mut roots);
    }
    Ok(roots)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(line: usize, title: &str, detail: &str, children: Vec<Item>) -> Item {
        Item { line, title: title.into(), detail: detail.into(), children }
    }

    #[test]
    fn nesting_details_and_blank_lines() {
        let text = "Layout\n  API | HTTP API | v2\n    Auth|Tokens\n\n  Web\nServices | What runs?\n";
        assert_eq!(
            parse(text).unwrap(),
            vec![
                item(
                    1,
                    "Layout",
                    "",
                    vec![item(2, "API", "HTTP API | v2", vec![item(3, "Auth", "Tokens", vec![])]), item(5, "Web", "", vec![])]
                ),
                item(6, "Services", "What runs?", vec![]),
            ]
        );
    }

    #[test]
    fn errors_name_the_line() {
        for (text, want) in [
            ("a\n    b\n  c\n", "line 3"),
            ("a\n\tb\n", "line 2"),
            ("a\n  | detail only\n", "line 2"),
            ("  a\nb\n", "line 2"),
        ] {
            let e = parse(text).unwrap_err();
            assert!(e.contains(want), "{text:?}: {e}");
        }
    }
}
