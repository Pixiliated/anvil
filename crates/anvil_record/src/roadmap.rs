//! ROADMAP.md to milestones. A `## M1: Title` heading is milestone `M1`; each `- [ ]` or `- [x]`
//! checkbox under it is `M1.1`, `M1.2`, ... in order, with indented continuation lines joined
//! into its title. A heading is done when all its checkboxes are; the first heading that is not
//! is `active`, following the roadmap's "first unchecked milestone is current work" rule.

#[derive(Debug, Clone, PartialEq)]
pub struct Item {
    pub id: String,
    pub title: String,
    pub status: &'static str,
    pub parent: Option<String>,
}

pub fn parse(text: &str) -> Result<Vec<Item>, String> {
    let mut items: Vec<Item> = Vec::new();
    let mut heading: Option<usize> = None;
    let mut children = 0;
    let mut last_box: Option<usize> = None;
    for (n, line) in text.lines().enumerate() {
        let trimmed = line.trim_start();
        if let Some(rest) = line.strip_prefix("## ") {
            last_box = None;
            let Some((id, title)) = rest.split_once(':') else {
                heading = None;
                continue;
            };
            let id = id.trim();
            if !(id.starts_with('M') && id.len() > 1 && id[1..].chars().all(|c| c.is_ascii_digit())) {
                heading = None;
                continue;
            }
            if items.iter().any(|i| i.id == id) {
                return Err(format!("ROADMAP.md line {}: milestone {id} appears twice", n + 1));
            }
            items.push(Item { id: id.into(), title: title.trim().into(), status: "open", parent: None });
            heading = Some(items.len() - 1);
            children = 0;
        } else if let Some(rest) = trimmed.strip_prefix("- [") {
            let (done, title) = match (rest.get(..2), rest.get(2..)) {
                (Some("x]") | Some("X]"), Some(t)) => (true, t),
                (Some(" ]"), Some(t)) => (false, t),
                _ => return Err(format!("ROADMAP.md line {}: a checkbox must be `- [ ]` or `- [x]`", n + 1)),
            };
            let Some(h) = heading else {
                return Err(format!("ROADMAP.md line {}: checkbox outside a `## M<n>: Title` milestone", n + 1));
            };
            children += 1;
            let parent = items[h].id.clone();
            items.push(Item {
                id: format!("{parent}.{children}"),
                title: title.trim().into(),
                status: if done { "done" } else { "open" },
                parent: Some(parent),
            });
            last_box = Some(items.len() - 1);
        } else if let (Some(b), true) = (last_box, line.starts_with("  ") && !trimmed.is_empty()) {
            items[b].title.push(' ');
            items[b].title.push_str(trimmed);
        } else {
            last_box = None;
        }
    }
    if items.is_empty() {
        return Err("no `## M<n>: Title` milestones found in the roadmap".into());
    }
    let mut active_set = false;
    for i in 0..items.len() {
        if items[i].parent.is_some() {
            continue;
        }
        let id = items[i].id.clone();
        let kids: Vec<_> = items.iter().filter(|c| c.parent.as_deref() == Some(id.as_str())).collect();
        let done = !kids.is_empty() && kids.iter().all(|c| c.status == "done");
        items[i].status = if done {
            "done"
        } else if !active_set {
            active_set = true;
            "active"
        } else {
            "open"
        };
    }
    Ok(items)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn headings_checkboxes_and_continuations() {
        let text = "# Roadmap\n\nintro\n\n## M0: Core\n- [x] Lexer\n- [ ] Review of\n      the spec\n\n## M1: Data\n- [x] Arrays\n\n## M2: Next\n- [ ] Formatter\n";
        let items = parse(text).unwrap();
        let ids: Vec<_> = items.iter().map(|i| (i.id.as_str(), i.status)).collect();
        assert_eq!(
            ids,
            [
                ("M0", "active"),
                ("M0.1", "done"),
                ("M0.2", "open"),
                ("M1", "done"),
                ("M1.1", "done"),
                ("M2", "open"),
                ("M2.1", "open")
            ]
        );
        assert_eq!(items[2].title, "Review of the spec");
    }

    #[test]
    fn loud_on_bad_roadmaps() {
        assert!(parse("- [x] orphan\n").unwrap_err().contains("outside"));
        assert!(parse("## M1: a\n- [y] bad\n").unwrap_err().contains("`- [ ]`"));
        assert!(parse("## M1: a\n## M1: b\n").unwrap_err().contains("twice"));
        assert!(parse("# nothing here\n").is_err());
    }
}
