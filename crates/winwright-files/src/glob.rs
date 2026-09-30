//! Case-insensitive file-name glob: `*` matches any run of characters, `?` exactly one.

use winwright_contracts::{WinwrightError, WinwrightResult};

#[derive(Debug)]
pub(crate) struct Glob(Vec<char>);

impl Glob {
    pub(crate) fn new(pattern: &str) -> WinwrightResult<Self> {
        let pattern = pattern.trim();
        if pattern.is_empty() {
            return Err(WinwrightError::invalid("search pattern is empty"));
        }
        if pattern.contains(['\\', '/']) {
            return Err(WinwrightError::invalid(
                "search patterns match file names only; put the folder in `root`",
            ));
        }
        if pattern.chars().any(char::is_control) {
            return Err(WinwrightError::invalid(
                "search pattern contains control characters",
            ));
        }
        Ok(Self(fold(pattern)))
    }

    pub(crate) fn matches(&self, name: &str) -> bool {
        wildcard(&self.0, &fold(name))
    }
}

fn fold(text: &str) -> Vec<char> {
    text.chars().flat_map(char::to_lowercase).collect()
}

/// Iterative matcher with single-star backtracking: linear for typical patterns, never
/// exponential.
fn wildcard(pattern: &[char], text: &[char]) -> bool {
    let (mut p, mut t) = (0usize, 0usize);
    // Position after the most recent `*`, and the text index it currently absorbs up to.
    let mut star: Option<(usize, usize)> = None;
    while t < text.len() {
        match pattern.get(p) {
            Some('*') => {
                star = Some((p + 1, t));
                p += 1;
            }
            Some(&c) if c == '?' || c == text[t] => {
                p += 1;
                t += 1;
            }
            _ => match star {
                Some((after_star, absorbed)) => {
                    p = after_star;
                    t = absorbed + 1;
                    star = Some((after_star, absorbed + 1));
                }
                None => return false,
            },
        }
    }
    pattern[p..].iter().all(|&c| c == '*')
}

#[cfg(test)]
mod tests {
    use super::*;

    fn glob(pattern: &str) -> Glob {
        Glob::new(pattern).unwrap()
    }

    #[test]
    fn stars_and_question_marks() {
        assert!(glob("*.png").matches("photo.png"));
        assert!(glob("*.png").matches(".png"));
        assert!(!glob("*.png").matches("photo.png.bak"));
        assert!(glob("*.png*").matches("photo.png.bak"));
        assert!(glob("IMG_????.jpg").matches("IMG_0042.jpg"));
        assert!(!glob("IMG_????.jpg").matches("IMG_42.jpg"));
        assert!(glob("*").matches("anything"));
        assert!(glob("a*b*c").matches("aXXbYYc"));
        assert!(!glob("a*b*c").matches("aXXbYY"));
        assert!(glob("**x").matches("x"));
        assert!(glob("report").matches("REPORT"));
        assert!(!glob("report").matches("report2"));
    }

    #[test]
    fn matching_ignores_case_including_unicode() {
        assert!(glob("*.PNG").matches("Photo.png"));
        assert!(glob("ÉTÉ*").matches("été 2024.txt"));
    }

    #[test]
    fn backtracking_stays_bounded() {
        let name = "a".repeat(200);
        assert!(!glob(&format!("{}b", "*a".repeat(20))).matches(&name));
    }

    #[test]
    fn folder_patterns_are_rejected() {
        assert!(Glob::new("").is_err());
        assert!(Glob::new("   ").is_err());
        assert!(Glob::new(r"sub\*.txt").is_err());
        assert!(Glob::new("sub/*.txt").is_err());
    }
}
