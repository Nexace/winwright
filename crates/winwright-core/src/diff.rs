//! Snapshot diff (spec §41, §42). Works on the rendered compact lines keyed by ref: refs are
//! stable across snapshots for the same element, so "same ref, different line" is a change.

use std::collections::HashMap;

/// One rendered snapshot line, minus indentation, keyed by its ref.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Line {
    pub reference: String,
    pub text: String,
}

/// Extracts `(ref, line)` pairs from compact snapshot text.
pub fn lines(tree: &str) -> Vec<Line> {
    tree.lines()
        .filter_map(|raw| {
            let text = raw.trim_start();
            let open = text.rfind(" [e")?;
            let close = text[open..].find(']')? + open;
            Some(Line {
                reference: text[open + 2..close].to_owned(),
                text: text.to_owned(),
            })
        })
        .collect()
}

fn without_ref(text: &str, reference: &str) -> String {
    text.replacen(&format!(" [{reference}]"), "", 1)
}

fn is_focused(text: &str, reference: &str) -> bool {
    without_ref(text, reference)
        .split_whitespace()
        .any(|w| w == "focused")
}

/// Renders the difference. Order: additions and changes in new document order, then removals,
/// then the focus move. Returns `(text, changed)`.
pub fn render(
    from_generation: &str,
    to_generation: &str,
    old: &[Line],
    new: &[Line],
) -> (String, bool) {
    let old_by_ref: HashMap<&str, &Line> = old.iter().map(|l| (l.reference.as_str(), l)).collect();
    let new_refs: std::collections::HashSet<&str> =
        new.iter().map(|l| l.reference.as_str()).collect();
    let mut out = format!("DIFF {from_generation} -> {to_generation}\n");
    let mut changed = false;
    for line in new {
        match old_by_ref.get(line.reference.as_str()) {
            None => {
                out.push_str(&format!("+ {}\n", line.text));
                changed = true;
            }
            Some(prev) if prev.text != line.text => {
                out.push_str(&format!(
                    "~ {} -> {}\n",
                    without_ref(&prev.text, &prev.reference),
                    line.text
                ));
                changed = true;
            }
            Some(_) => {}
        }
    }
    for line in old {
        if !new_refs.contains(line.reference.as_str()) {
            out.push_str(&format!("- {}\n", line.text));
            changed = true;
        }
    }
    let old_focus = old.iter().find(|l| is_focused(&l.text, &l.reference));
    let new_focus = new.iter().find(|l| is_focused(&l.text, &l.reference));
    if let Some(f) = new_focus
        && old_focus.map(|o| &o.reference) != Some(&f.reference)
    {
        out.push_str(&format!("focus -> {}\n", f.text));
        changed = true;
    }
    if !changed {
        out.push_str("(no changes)\n");
    }
    (out, changed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_refs_and_ignores_suffixes() {
        let ls = lines("WINDOW \"W\" [e1]\n  LIST \"Files\" [e7] children=384 showing=20\n");
        assert_eq!(ls.len(), 2);
        assert_eq!(ls[1].reference, "e7");
        assert_eq!(ls[1].text, "LIST \"Files\" [e7] children=384 showing=20");
    }

    #[test]
    fn reports_additions_changes_removals_and_focus() {
        let old = lines(
            "WINDOW \"Notepad\" [e1]\n  DOCUMENT \"Text\" focused [e2]\n  CHECKBOX \"Wrap\" unchecked [e3]\n  BUTTON \"Old\" [e4]\n",
        );
        let new = lines(
            "WINDOW \"Notepad\" [e1]\n  DOCUMENT \"Text\" [e2]\n  CHECKBOX \"Wrap\" checked [e3]\n  DIALOG \"Save As\" [e9]\n    EDIT \"File name:\" value=\"\" focused [e10]\n",
        );
        let (text, changed) = render("s_1", "s_2", &old, &new);
        assert!(changed);
        assert_eq!(
            text,
            "DIFF s_1 -> s_2\n\
             ~ DOCUMENT \"Text\" focused -> DOCUMENT \"Text\" [e2]\n\
             ~ CHECKBOX \"Wrap\" unchecked -> CHECKBOX \"Wrap\" checked [e3]\n\
             + DIALOG \"Save As\" [e9]\n\
             + EDIT \"File name:\" value=\"\" focused [e10]\n\
             - BUTTON \"Old\" [e4]\n\
             focus -> EDIT \"File name:\" value=\"\" focused [e10]\n"
        );
    }

    #[test]
    fn identical_snapshots_say_so() {
        let a = lines("WINDOW \"W\" [e1]\n");
        let (text, changed) = render("s_3", "s_4", &a, &a);
        assert!(!changed);
        assert_eq!(text, "DIFF s_3 -> s_4\n(no changes)\n");
    }
}
