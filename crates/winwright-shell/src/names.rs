//! Picking the app a person named among the Start menu's entries.

use winwright_contracts::{WinwrightError, WinwrightResult};

fn words(text: &str) -> Vec<String> {
    text.split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .map(str::to_lowercase)
        .collect()
}

/// The entry `wanted` names: the same name (the first, so earlier entries win), else the only
/// one whose name holds every word of it as a whole word ("Lightroom" finds "Adobe Lightroom
/// Classic"; "Photos" never finds "Photoshop"). Uninstallers never count. `Err` lists the
/// candidates when several fit.
pub(crate) fn pick<'a, T>(
    wanted: &str,
    entries: &'a [(String, T)],
) -> WinwrightResult<Option<&'a T>> {
    let want = wanted.trim().to_lowercase();
    if let Some((_, exact)) = entries
        .iter()
        .find(|(name, _)| name.trim().to_lowercase() == want)
    {
        return Ok(Some(exact));
    }
    let wanted_words = words(wanted);
    if wanted_words.is_empty() {
        return Ok(None);
    }
    let mut fits: Vec<&(String, T)> = entries
        .iter()
        .filter(|(name, _)| {
            let have = words(name);
            have.first().is_none_or(|w| w != "uninstall")
                && wanted_words.iter().all(|w| have.contains(w))
        })
        .collect();
    // The same app listed twice (a user and an all-users shortcut) is one candidate.
    let mut seen = std::collections::HashSet::new();
    fits.retain(|(name, _)| seen.insert(name.to_lowercase()));
    match fits.as_slice() {
        [] => Ok(None),
        [(_, one)] => Ok(Some(one)),
        many => Err(WinwrightError::invalid(format!(
            "`{wanted}` matches several apps: {}; use the full name",
            many.iter()
                .map(|(name, _)| name.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entries(names: &[&str]) -> Vec<(String, usize)> {
        names
            .iter()
            .enumerate()
            .map(|(i, n)| ((*n).to_owned(), i))
            .collect()
    }

    #[test]
    fn exact_names_win_then_whole_words() {
        let all = entries(&[
            "Discord",
            "Discord PTB",
            "Adobe Photoshop 2026",
            "Uninstall Adobe Lightroom",
            "Adobe Lightroom Classic",
            "Photos",
        ]);
        assert_eq!(pick("discord", &all).unwrap(), Some(&0));
        assert_eq!(pick("Lightroom", &all).unwrap(), Some(&4));
        assert_eq!(pick("lightroom classic", &all).unwrap(), Some(&4));
        // A word inside another word is no match: Photos is the Store app, not Photoshop.
        assert_eq!(pick("Photos", &all).unwrap(), Some(&5));
        assert_eq!(pick("Photo", &all).unwrap(), None);
        assert_eq!(pick("Spotify", &all).unwrap(), None);
        let err = pick("Adobe", &all).unwrap_err().to_string();
        assert!(
            err.contains("Adobe Photoshop 2026, Adobe Lightroom Classic"),
            "{err}"
        );
    }

    #[test]
    fn the_same_app_twice_is_one_candidate() {
        let twice = entries(&["Zoom Workplace", "Zoom Workplace"]);
        assert_eq!(pick("Zoom", &twice).unwrap(), Some(&0));
    }
}
