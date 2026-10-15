//! The one search rule, shared by the popup and, later, the Manager.

use crate::compose::compose;

/// True when every whitespace-separated word of `query` appears, case-insensitively,
/// somewhere in `file_name` without its extension. Word order does not matter.
/// An empty or all-whitespace query matches everything.
///
/// Letters with accents compare as one character however they are spelled, so
/// `é` finds a name written `e` + U+0301, as macOS saves it.
pub fn matches(file_name: &str, query: &str) -> bool {
    let base = compose(name_without_extension(file_name)).to_lowercase();
    let query = compose(query);
    query.split_whitespace().all(|word| base.contains(&word.to_lowercase()))
}

/// `Happy dance.gif` -> `Happy dance`. A leading dot is not an extension separator.
pub fn name_without_extension(file_name: &str) -> &str {
    match file_name.rfind('.') {
        Some(i) if i > 0 => &file_name[..i],
        _ => file_name,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn words_match_in_any_order() {
        assert!(matches("Happy dance.gif", "dance happy"));
        assert!(matches("Happy dance.gif", "happy dance"));
    }

    #[test]
    fn every_word_must_match() {
        assert!(!matches("Happy dance.gif", "happy party"));
    }

    #[test]
    fn partial_words_match() {
        assert!(matches("Celebration.mp4", "celeb"));
    }

    #[test]
    fn case_is_ignored() {
        assert!(matches("Happy dance.gif", "HAPPY"));
        assert!(matches("ÉTÉ.png", "été"));
    }

    /// An accent typed as its own character after the letter.
    #[test]
    fn accents_match_however_they_are_spelled() {
        assert!(matches("Cafe\u{0301} night.png", "café"), "name from a Mac, typed query");
        assert!(matches("Café night.png", "cafe\u{0301}"), "pasted query, ordinary name");
        assert!(matches("E\u{0301}TE\u{0301}.gif", "été"), "case and accent together");
        assert!(!matches("Cafe night.png", "café"), "an accent still counts as a different letter");
    }

    #[test]
    fn extension_is_not_searched() {
        assert!(!matches("Happy dance.gif", "gif"));
        assert!(!matches("Race clip.mp4", "mp4"));
        assert!(!matches("Square.png", ".png"));
        // ...but the same letters inside the name still count.
        assert!(matches("Gift box.png", "gif"));
    }

    #[test]
    fn empty_or_blank_query_matches_everything() {
        assert!(matches("Square.png", ""));
        assert!(matches("Square.png", "   \t "));
    }

    #[test]
    fn extra_whitespace_is_ignored() {
        assert!(matches("Happy dance.gif", "  dance    happy  "));
    }

    #[test]
    fn only_the_last_extension_is_removed() {
        assert_eq!(name_without_extension("clip.final.mp4"), "clip.final");
        assert!(matches("clip.final.mp4", "final"));
        assert_eq!(name_without_extension(".hidden"), ".hidden");
        assert_eq!(name_without_extension("no extension"), "no extension");
    }
}
