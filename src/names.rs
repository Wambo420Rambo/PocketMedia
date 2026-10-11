//! The one name validator, shared by the import
//! review and, later, the Manager's Rename. The extension is never part of the
//! edited name; it is fixed and only used for collision checks.

use std::collections::HashSet;

/// Longest name we accept, in characters, without the extension.
pub const MAX_LEN: usize = 120;
const ILLEGAL: &[char] = &['\\', '/', ':', '*', '?', '"', '<', '>', '|'];
const RESERVED: &[&str] = &[
    "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7", "COM8", "COM9", "LPT1",
    "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NameError {
    Empty,
    IllegalChar,
    TrailingSpaceOrPeriod,
    Reserved,
    TooLong,
    Taken,
}

impl NameError {
    /// Wording from the wireframe where it has one.
    pub fn message(self) -> &'static str {
        match self {
            NameError::Empty => "Enter a name.",
            NameError::IllegalChar => "A name can't contain \\ / : * ? \" < > |",
            NameError::TrailingSpaceOrPeriod => "A name can't end with a space or period.",
            NameError::Reserved => "Windows reserves that name. Choose another.",
            NameError::TooLong => "Use 120 characters or fewer.",
            NameError::Taken => "That name is already used. Choose another.",
        }
    }
}

/// Checks a name on its own (without the extension). Leading spaces are
/// ignored; everything else must be valid as typed.
pub fn check(base: &str) -> Result<(), NameError> {
    let name = base.trim_start();
    if name.trim().is_empty() {
        return Err(NameError::Empty);
    }
    if name.chars().any(|c| ILLEGAL.contains(&c) || c.is_control()) {
        return Err(NameError::IllegalChar);
    }
    if name.ends_with(' ') || name.ends_with('.') {
        return Err(NameError::TrailingSpaceOrPeriod);
    }
    // Windows reserves these names with or without an extension: "nul.tar" too.
    let stem = name.split('.').next().unwrap_or(name).trim_end();
    if RESERVED.iter().any(|r| r.eq_ignore_ascii_case(stem)) {
        return Err(NameError::Reserved);
    }
    if name.chars().count() > MAX_LEN {
        return Err(NameError::TooLong);
    }
    Ok(())
}

/// The final file name for a valid base name: leading spaces dropped, extension kept.
pub fn final_name(base: &str, ext: &str) -> String {
    format!("{}{ext}", base.trim_start())
}

/// Validates a batch in order. A name that collides (ignoring case, extension
/// included) with the library or with an earlier row in the batch is `Taken`;
/// the first row keeps its name, as in the wireframe.
pub fn check_batch<'a>(rows: impl IntoIterator<Item = (&'a str, &'a str)>, library: &[&str]) -> Vec<Option<NameError>> {
    let mut used: HashSet<String> = library.iter().map(|n| n.to_lowercase()).collect();
    rows.into_iter()
        .map(|(base, ext)| {
            if let Err(e) = check(base) {
                return Some(e);
            }
            let full = final_name(base, ext).to_lowercase();
            if used.insert(full) { None } else { Some(NameError::Taken) }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_names_are_fine() {
        assert_eq!(check("Birthday confetti"), Ok(()));
        assert_eq!(check("Clip (2)"), Ok(()));
        assert_eq!(check("été 2024"), Ok(()));
    }

    #[test]
    fn empty_or_blank_is_rejected() {
        assert_eq!(check(""), Err(NameError::Empty));
        assert_eq!(check("   "), Err(NameError::Empty));
    }

    #[test]
    fn every_illegal_character_is_rejected() {
        for c in ['\\', '/', ':', '*', '?', '"', '<', '>', '|'] {
            assert_eq!(check(&format!("Cat{c}photo")), Err(NameError::IllegalChar), "{c}");
        }
        assert_eq!(check("Cat/photo"), Err(NameError::IllegalChar));
    }

    #[test]
    fn trailing_space_or_period_is_rejected() {
        assert_eq!(check("Cat "), Err(NameError::TrailingSpaceOrPeriod));
        assert_eq!(check("Cat."), Err(NameError::TrailingSpaceOrPeriod));
        assert_eq!(check("  Cat"), Ok(()), "leading spaces are ignored");
    }

    #[test]
    fn reserved_names_are_rejected_with_or_without_extension() {
        for r in ["CON", "con", "Prn", "AUX", "nul", "COM1", "com9", "LPT1", "lpt9", "NUL.tar", "con .x"] {
            assert_eq!(check(r), Err(NameError::Reserved), "{r}");
        }
        assert_eq!(check("CONSOLE"), Ok(()));
        assert_eq!(check("COM10"), Ok(()));
        assert_eq!(check("my nul"), Ok(()));
    }

    #[test]
    fn length_is_limited() {
        assert_eq!(check(&"a".repeat(MAX_LEN)), Ok(()));
        assert_eq!(check(&"a".repeat(MAX_LEN + 1)), Err(NameError::TooLong));
    }

    #[test]
    fn collisions_in_the_batch_ignore_case_and_keep_the_first() {
        let rows = [("Birthday confetti", ".png"), ("Wave", ".gif"), ("birthday CONFETTI", ".png")];
        assert_eq!(check_batch(rows, &[]), vec![None, None, Some(NameError::Taken)]);
    }

    #[test]
    fn same_name_with_a_different_extension_is_not_a_collision() {
        let rows = [("Wave", ".gif"), ("Wave", ".png")];
        assert_eq!(check_batch(rows, &[]), vec![None, None]);
    }

    #[test]
    fn collisions_with_the_library_ignore_case() {
        let rows = [("wave", ".gif"), ("Clip (2)", ".mp4"), ("Cat", ".png")];
        assert_eq!(
            check_batch(rows, &["Wave.gif", "CLIP (2).MP4"]),
            vec![Some(NameError::Taken), Some(NameError::Taken), None]
        );
    }

    #[test]
    fn the_extension_is_kept() {
        assert_eq!(final_name("  Birthday confetti", ".png"), "Birthday confetti.png");
    }
}
