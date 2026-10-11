//! The one search rule, shared by the popup and, later, the Manager.

/// True when every whitespace-separated word of `query` appears, case-insensitively,
/// somewhere in `file_name` without its extension. Word order does not matter.
/// An empty or all-whitespace query matches everything.
pub fn matches(file_name: &str, query: &str) -> bool {
    let base = name_without_extension(file_name).to_lowercase();
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
