//! Opening the hyphenation dictionaries the Kindle already carries.
//!
//! Ten of them, at `/usr/java/lib/dictionaries/hyphen/`, part of the rootfs on
//! every device karyll runs on rather than something a locale download leaves
//! behind. karyll reads the data and leaves `libhyphen.so` alone, the same
//! bargain [`crate::lexicon`] strikes with the segmenter.
//!
//! **Two of the ten are used**, because they are the two languages karyll is
//! written in. Chinese and Japanese do not hyphenate at all, and no dictionary
//! is offered for a language karyll has no keyboard for: hyphenating French
//! prose with English patterns is worse than leaving it alone.
//!
//! American English rather than British: `hyph_en_GB.dic` carries four
//! patterns that respell the word around the break, which
//! [`karyll_core::hyphen`] refuses rather than silently drop.
//!
//! The German file is the reformed orthography, and states no minimum word
//! length, so it divides words as short as four letters.

use std::path::Path;
use std::sync::OnceLock;
use std::time::Instant;

use karyll_core::Hyphenator;
use karyll_core::hyphen::EN_CURATION;

use crate::Language;

/// Where the firmware keeps each dictionary, and the break decisions that go
/// over it. Empty for a set used exactly as its author wrote it.
const DICTIONARIES: [(Language, &str, &str); 2] = [
    (
        Language::English,
        "/usr/java/lib/dictionaries/hyphen/hyph_en_US.dic",
        EN_CURATION,
    ),
    (
        Language::German,
        "/usr/java/lib/dictionaries/hyphen/hyph_de.dic",
        "",
    ),
];

/// One slot per entry of [`DICTIONARIES`]. A dictionary is a megabyte or so of
/// automaton and the writer can cycle back and forth between sources, so it is
/// built once and held.
static LOADED: [OnceLock<Option<Hyphenator>>; DICTIONARIES.len()] =
    [OnceLock::new(), OnceLock::new()];

/// The dictionary for `language`, or `None` where karyll hyphenates it not at
/// all or the device has not got the file.
///
/// **A missing dictionary is not an error worth stopping for**: the page wraps
/// at spaces, which is what it did before any of this, so the failure costs an
/// evener rag and nothing else.
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

    /// The real firmware files, when a copy of one is to hand.
    ///
    /// `KARYLL_LEXICON_DIR` points at a directory holding the device's `usr`
    /// tree — a system dump will do — and without it this says nothing, because
    /// a development machine has none of these files. The reader is covered
    /// either way by the fixtures in `karyll_core::hyphen`; what this adds is
    /// the only thing a fixture cannot: that the real bytes parse, and divide
    /// words where a typesetter would.
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
                    // The curated decisions reach the firmware's patterns.
                    ("everything", "every-thing"),
                    ("understanding", "under-stand-ing"),
                    ("father", "father"),
                    // Five letters and fewer are left whole.
                    ("table", "table"),
                    // A word that already breaks takes no mark against it.
                    ("well-thumbed", "well-thumbed"),
                ],
            ),
            (
                Language::German,
                &[
                    ("Silbentrennung", "Sil-ben-tren-nung"),
                    ("Bibliothek", "Bi-blio-thek"),
                    ("Geschwindigkeit", "Ge-schwin-dig-keit"),
                    // Reformed orthography, which divides both of these.
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

    /// karyll hyphenates two languages and no others, and asking for one of the
    /// rest is not an error.
    #[test]
    fn a_language_that_does_not_hyphenate_has_no_dictionary() {
        for language in [
            Language::Chinese,
            Language::ChineseTraditional,
            Language::Japanese,
        ] {
            assert!(
                !DICTIONARIES.iter().any(|(l, _, _)| *l == language),
                "{language:?} is offered a dictionary"
            );
        }
    }
}
