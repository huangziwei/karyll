//! The firmware's hyphenation dictionaries: [`DICTIONARIES`] names one file
//! under `/usr/java/lib/dictionaries/hyphen/` per hyphenated [`Language`],
//! and [`load`] parses it with [`karyll_core::hyphen`] on first use.

use std::path::Path;
use std::sync::OnceLock;
use std::time::Instant;

use karyll_core::Hyphenator;
use karyll_core::hyphen::EN_CURATION;

use crate::Language;

/// Each hyphenated language: the dictionary's device path, and the curation
/// applied over its patterns — empty applies none.
const DICTIONARIES: [(Language, &str, &str); 2] = [
    (
        Language::English,
        "/usr/java/lib/dictionaries/hyphen/hyph_en_US.dic",
        EN_CURATION,
    ),
    (
        // Reformed orthography; the file states no minimum word length.
        Language::German,
        "/usr/java/lib/dictionaries/hyphen/hyph_de.dic",
        "",
    ),
];

/// One parsed dictionary per entry of [`DICTIONARIES`], built once and held.
static LOADED: [OnceLock<Option<Hyphenator>>; DICTIONARIES.len()] =
    [OnceLock::new(), OnceLock::new()];

/// The dictionary for `language`: `None` for a language with no
/// [`DICTIONARIES`] entry, an unreadable file, or bytes that do not parse.
pub fn load(language: Language) -> Option<&'static Hyphenator> {
    let at = DICTIONARIES.iter().position(|(l, _, _)| *l == language)?;
    let (_, path, curation) = DICTIONARIES[at];
    LOADED[at]
        .get_or_init(|| read(Path::new(path), curation))
        .as_ref()
}

fn read(path: &Path, curation: &str) -> Option<Hyphenator> {
    let started = Instant::now();
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(e) => {
            eprintln!("hyphen: {} unreadable: {e}", path.display());
            return None;
        }
    };
    let size = bytes.len();
    let dictionary = match Hyphenator::parse(&bytes) {
        Ok(dictionary) => dictionary.curated(curation),
        Err(e) => {
            eprintln!(
                "hyphen: {} is {size} bytes of something else: {e}",
                path.display()
            );
            return None;
        }
    };
    eprintln!(
        "hyphen: {} states from {} in {} ms",
        dictionary.states(),
        path.display(),
        started.elapsed().as_millis()
    );
    Some(dictionary)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Parses the files under `$KARYLL_LEXICON_DIR` — a directory holding a
    /// device's `usr` tree — and checks their divisions. Returns with the
    /// variable unset.
    #[test]
    fn the_firmware_dictionaries_parse_and_divide_words_as_they_should() {
        let Ok(root) = std::env::var("KARYLL_LEXICON_DIR") else {
            return;
        };
        let probes: [(Language, &[(&str, &str)]); 2] = [
            (
                Language::English,
                &[
                    ("hyphenation", "hy-phen-ation"),
                    ("typography", "ty-pog-ra-phy"),
                    ("algorithm", "al-go-rithm"),
                    // Breaks decided by `EN_CURATION`, over the file's own.
                    ("everything", "every-thing"),
                    ("understanding", "under-stand-ing"),
                    ("father", "father"),
                    // Five letters and fewer are left whole.
                    ("table", "table"),
                    // A word containing a hyphen takes no soft hyphens.
                    ("well-thumbed", "well-thumbed"),
                ],
            ),
            (
                Language::German,
                &[
                    ("Silbentrennung", "Sil-ben-tren-nung"),
                    ("Bibliothek", "Bi-blio-thek"),
                    ("Geschwindigkeit", "Ge-schwin-dig-keit"),
                    // Reformed-orthography divisions.
                    ("Kiste", "Kis-te"),
                    ("Fenster", "Fens-ter"),
                    // Latin-1 in the file, UTF-8 in the word.
                    ("Straßenbahn", "Stra-ßen-bahn"),
                    ("Bäckerei", "Bä-cke-rei"),
                ],
            ),
        ];

        for (language, words) in probes {
            let at = DICTIONARIES
                .iter()
                .position(|(l, _, _)| *l == language)
                .unwrap();
            let (_, path, curation) = DICTIONARIES[at];
            let path = Path::new(&root).join(path.trim_start_matches('/'));
            let dictionary = read(&path, curation).unwrap_or_else(|| panic!("{}", path.display()));
            assert!(
                dictionary.states() > 1000,
                "{language:?}: only {} states",
                dictionary.states()
            );
            for (word, want) in words {
                let got = dictionary.with_soft_hyphens(word).replace('\u{ad}', "-");
                assert_eq!(&got, want, "{language:?} {word}");
            }
        }
    }

    /// The CJK languages have no [`DICTIONARIES`] entry.
    #[test]
    fn a_language_that_does_not_hyphenate_has_no_dictionary() {
        for language in [
            Language::Chinese,
            Language::ChineseTraditional,
            Language::Japanese,
            Language::Korean,
        ] {
            assert!(
                !DICTIONARIES.iter().any(|(l, _, _)| *l == language),
                "{language:?} is offered a dictionary"
            );
        }
    }
}
