//! Opening the word lists the Kindle already carries.
//!
//! The three files are part of the firmware on every device karyll runs on —
//! the same bytes on all of them — and belong to the segmenter the framework
//! uses for its own word selection. karyll reads the data and leaves the
//! libraries alone: a word list is a word list, while the libraries around it
//! are C++ with an ICU version in their symbol names that differs per device.
//!
//! Which list applies follows the regional convention the Han faces already
//! follow, because Han unification leaves nothing in the characters themselves
//! to decide it — 干 is a word in all three conventions and a different one in
//! each.

use std::path::Path;
use std::time::Instant;

use karyll_core::script::Region;
use karyll_core::{Dict, Layout};

/// Where the firmware keeps each list, and which layout it is written in.
const LISTS: [(Region, &str, Layout); 3] = [
    (
        Region::Simplified,
        "/usr/lib/mmseg/data_mmap",
        Layout::Mmseg,
    ),
    (
        Region::Traditional,
        "/usr/lib/mmseg/tcn/data_mmap",
        Layout::Mmseg,
    ),
    (
        Region::Japanese,
        "/usr/lib/resegmenter/words_list.mem",
        Layout::Words,
    ),
];

/// Read the list for `region`, or `None` when the device has not got it.
///
/// A missing or unreadable list is not an error worth stopping for: word
/// selection falls back to whole runs of Han, which is what it did before any
/// of this, so the failure costs precision and nothing else.
pub fn load(region: Region) -> Option<Dict> {
    let &(_, path, layout) = LISTS.iter().find(|(r, _, _)| *r == region)?;
    read(Path::new(path), layout)
}

fn read(path: &Path, layout: Layout) -> Option<Dict> {
    let started = Instant::now();
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(e) => {
            eprintln!("lexicon: {} unreadable: {e}", path.display());
            return None;
        }
    };
    let size = bytes.len();
    let Some(dict) = Dict::parse(bytes, layout) else {
        eprintln!(
            "lexicon: {} is {size} bytes of something else",
            path.display()
        );
        return None;
    };
    eprintln!(
        "lexicon: {} words from {} in {} ms",
        dict.len(),
        path.display(),
        started.elapsed().as_millis()
    );
    Some(dict)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The real firmware files, when a copy of one is to hand.
    ///
    /// `KARYLL_LEXICON_DIR` points at a directory holding the device's `usr`
    /// tree — a system dump will do — and without it this says nothing, because
    /// a development machine has none of these files. The parser is covered
    /// either way by the fixtures in `karyll_core::dict`; what this adds is the
    /// only thing a fixture cannot: that the layout matches the real bytes.
    #[test]
    fn the_firmware_lists_parse_and_hold_the_words_they_should() {
        let Ok(root) = std::env::var("KARYLL_LEXICON_DIR") else {
            return;
        };
        let probes: [(Region, &[&str], &[&str]); 3] = [
            (
                Region::Simplified,
                &["今天", "天气", "研究", "研究生", "生命", "起源", "的"],
                &["命起", "气很"],
            ),
            (Region::Traditional, &["天氣", "台灣", "研究"], &["气很"]),
            (Region::Japanese, &["東京", "勉強", "連文節"], &["書いた"]),
        ];

        for (region, present, absent) in probes {
            let &(_, path, layout) = LISTS.iter().find(|(r, _, _)| *r == region).unwrap();
            let path = Path::new(&root).join(path.trim_start_matches('/'));
            let dict = read(&path, layout).unwrap_or_else(|| panic!("{}", path.display()));

            assert!(
                dict.len() > 100_000,
                "{region:?}: only {} words",
                dict.len()
            );
            for word in present {
                let chars: Vec<char> = word.chars().collect();
                assert!(dict.contains(&chars), "{region:?} is missing {word}");
            }
            for word in absent {
                let chars: Vec<char> = word.chars().collect();
                assert!(!dict.contains(&chars), "{region:?} claims {word}");
            }
        }
    }

    /// What the firmware's own dictionary makes of the sentences the coarse
    /// classifier could not divide at all.
    #[test]
    fn the_firmware_list_segments_real_sentences() {
        let Ok(root) = std::env::var("KARYLL_LEXICON_DIR") else {
            return;
        };
        let path = Path::new(&root).join("usr/lib/mmseg/data_mmap");
        let dict = read(&path, Layout::Mmseg).expect("simplified list");

        for (text, want) in [
            // The example MMSEG is named for: longest match reads 研究生.
            ("研究生命起源", "研究 生命 起源"),
            ("今天天气很好", "今天 天气 很好"),
            ("我喜欢学习中文", "我 喜欢 学习 中文"),
            // Held whole: the dictionary has it, at seven characters.
            ("中华人民共和国", "中华人民共和国"),
            // Two readings a segmenter is usually shown failing — 北京大学 and
            // 和尚 are both words, and neither is the one meant here.
            ("北京大学生活动中心", "北京 大学生 活动中心"),
            ("结婚的和尚未结婚的", "结婚 的 和 尚未 结婚 的"),
        ] {
            let chars: Vec<char> = text.chars().collect();
            let got: Vec<String> = karyll_core::segment::cuts(&chars, &dict)
                .windows(2)
                .map(|w| chars[w[0]..w[1]].iter().collect())
                .collect();
            assert_eq!(got.join(" "), want, "{text}");
        }
    }

    /// Where the firmware's list stops, recorded so it is not mistaken later
    /// for something broken: it holds no names, so a sentence that turns on one
    /// is read as the words the characters otherwise spell.
    #[test]
    fn a_name_the_list_does_not_hold_is_read_as_words() {
        let Ok(root) = std::env::var("KARYLL_LEXICON_DIR") else {
            return;
        };
        let path = Path::new(&root).join("usr/lib/mmseg/data_mmap");
        let dict = read(&path, Layout::Mmseg).expect("simplified list");

        // 严守一 is a person; without him the reading is 严守 · 一把.
        let chars: Vec<char> = "严守一把手机关了".chars().collect();
        let got: Vec<String> = karyll_core::segment::cuts(&chars, &dict)
            .windows(2)
            .map(|w| chars[w[0]..w[1]].iter().collect())
            .collect();
        assert_eq!(got.join(" "), "严守 一把 手机 关了");
        assert!(!dict.contains(&"严守一".chars().collect::<Vec<_>>()));
    }
}
