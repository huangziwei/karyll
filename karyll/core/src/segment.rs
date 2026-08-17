//! Where one word ends and the next begins inside a run of Han.
//!
//! Chinese sets no spaces, so a run of Han is a string of characters that could
//! be cut in many places and reads correctly in only one of them. 研究生命起源
//! is 研究生 · 命 · 起源 by longest match and 研究 · 生命 · 起源 to a reader.
//! Picking between those is what this does, by MMSEG: at each position, take
//! every way the next three words could be read, and score the readings against
//! each other rather than the words in isolation.
//!
//! Four measures decide it, each breaking the ties the one before it left:
//!
//! 1. the most characters covered by the three words,
//! 2. the longest words on average, which separates readings that ran out of
//!    text from readings that covered the same span in fewer words,
//! 3. the most even word lengths, which prefers 研究 · 生命 over 研究生 · 命,
//! 4. the commonest single characters, read from the frequency the dictionary
//!    records for one-character words alone.
//!
//! The first word of the winning reading is committed and the whole question is
//! asked again from the character after it.
//!
//! **The arithmetic is integer throughout.** An average is compared by
//! cross-multiplying, a variance by clearing its denominator, and the fourth
//! measure by multiplying frequencies where the published rule adds their
//! logarithms — the same ordering, since a logarithm is monotonic, without a
//! float on a device that has two of them.

use crate::dict::{Dict, MAX_WORD};

/// Every boundary in `chars`, starting at 0 and ending at its length.
///
/// A character the dictionary knows nothing about is a word on its own, so the
/// boundaries always tile the run and a run of unknown characters comes back
/// one character at a time.
pub fn cuts(chars: &[char], dict: &Dict) -> Vec<usize> {
    // Every position is asked about once, before any of it is decided. Each is
    // read by up to three of the readings below, and the dictionary answers the
    // same way every time.
    let spots: Vec<Spot> = (0..chars.len()).map(|i| Spot::at(chars, i, dict)).collect();

    let mut out = vec![0];
    let mut at = 0;
    while at < chars.len() {
        at += first_word(&spots, at);
        out.push(at);
    }
    out
}

/// How many characters the first word at `at` claims.
fn first_word(spots: &[Spot], at: usize) -> usize {
    let end = spots.len();
    let mut best: Option<Chunk> = None;
    let mut keep = |chunk: Chunk| match &best {
        Some(current) if !chunk.beats(current) => {}
        _ => best = Some(chunk),
    };

    // Longest first, so that a reading which ties on all four measures keeps
    // the longer opening word rather than the shorter one.
    for &a in spots[at].lens().iter().rev() {
        let after_a = at + a as usize;
        if after_a == end {
            keep(Chunk::of(&[a], spots, at));
            continue;
        }
        for &b in spots[after_a].lens().iter().rev() {
            let after_b = after_a + b as usize;
            if after_b == end {
                keep(Chunk::of(&[a, b], spots, at));
                continue;
            }
            for &c in spots[after_b].lens().iter().rev() {
                keep(Chunk::of(&[a, b, c], spots, at));
            }
        }
    }

    best.map(|c| c.lens[0] as usize).unwrap_or(1).max(1)
}

/// What the dictionary says about one position: the lengths of the words that
/// start there, and what the character on its own is worth.
///
/// Held without allocating — there can never be more candidate lengths than the
/// longest word the dictionary admits.
struct Spot {
    lens: [u8; MAX_WORD],
    n: u8,
    /// The frequency of the single character here, which is zero unless it is a
    /// word in its own right.
    freq: u16,
}

impl Spot {
    fn at(chars: &[char], at: usize, dict: &Dict) -> Self {
        let mut spot = Spot {
            lens: [0; MAX_WORD],
            n: 0,
            freq: 0,
        };
        let room = (chars.len() - at).min(dict.longest());
        for len in 1..=room {
            if let Some(freq) = dict.lookup(&chars[at..at + len]) {
                if len == 1 {
                    spot.freq = freq;
                }
                spot.lens[spot.n as usize] = len as u8;
                spot.n += 1;
            }
        }
        // An unknown character still has to be stepped over: a reading that
        // stopped dead at one would leave the rest of the run unsegmented.
        if spot.n == 0 {
            spot.lens[0] = 1;
            spot.n = 1;
        }
        spot
    }

    fn lens(&self) -> &[u8] {
        &self.lens[..self.n as usize]
    }
}

/// One reading of the next three words, and the four numbers that rank it.
#[derive(Clone, Copy)]
struct Chunk {
    lens: [u8; 3],
    /// How many words the reading actually has: fewer than three only when the
    /// run ends first.
    words: u64,
    /// Characters covered.
    total: u64,
    /// Their squares, which is all a variance needs beyond the total.
    squares: u64,
    /// The frequencies of the one-character words multiplied together, with an
    /// unrecorded frequency counting as 1 — a word the dictionary holds without
    /// one says nothing either way.
    freedom: u64,
}

impl Chunk {
    fn of(lens: &[u8], spots: &[Spot], at: usize) -> Self {
        let mut chunk = Chunk {
            lens: [0; 3],
            words: lens.len() as u64,
            total: 0,
            squares: 0,
            freedom: 1,
        };
        let mut pos = at;
        for (slot, &len) in lens.iter().enumerate() {
            chunk.lens[slot] = len;
            chunk.total += u64::from(len);
            chunk.squares += u64::from(len) * u64::from(len);
            if len == 1 {
                chunk.freedom *= u64::from(spots[pos].freq).max(1);
            }
            pos += len as usize;
        }
        chunk
    }

    /// Whether this reading is strictly better than `other`.
    fn beats(&self, other: &Chunk) -> bool {
        if self.total != other.total {
            return self.total > other.total;
        }
        // Averages, compared without dividing.
        let (mine, theirs) = (self.total * other.words, other.total * self.words);
        if mine != theirs {
            return mine > theirs;
        }
        // Variance is (words * squares - total²) / words², and the numerator
        // cannot go negative: no set of lengths has a spread below zero.
        let spread = |c: &Chunk| c.words * c.squares - c.total * c.total;
        let (mine, theirs) = (
            spread(self) * other.words * other.words,
            spread(other) * self.words * self.words,
        );
        if mine != theirs {
            return mine < theirs;
        }
        self.freedom > other.freedom
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dict::{Layout, fixture};

    fn dict(words: &[(&str, u16)]) -> Dict {
        Dict::parse(fixture::image(words, Layout::Mmseg), Layout::Mmseg).expect("parses")
    }

    fn split(text: &str, dict: &Dict) -> Vec<String> {
        let chars: Vec<char> = text.chars().collect();
        cuts(&chars, dict)
            .windows(2)
            .map(|w| chars[w[0]..w[1]].iter().collect())
            .collect()
    }

    /// The example MMSEG is named for: longest match reads 研究生 and a reader
    /// reads 研究 · 生命, and only the third measure tells them apart.
    #[test]
    fn the_ambiguity_that_longest_match_gets_wrong() {
        let d = dict(&[
            ("研究", 0),
            ("研究生", 0),
            ("生命", 0),
            ("起源", 0),
            ("命", 500),
        ]);
        assert_eq!(split("研究生命起源", &d), ["研究", "生命", "起源"]);
    }

    #[test]
    fn a_plain_sentence_comes_apart_at_its_words() {
        let d = dict(&[
            ("今天", 0),
            ("天气", 0),
            ("很", 900),
            ("好", 800),
            ("今", 100),
            ("天", 700),
            ("气", 200),
        ]);
        assert_eq!(split("今天天气很好", &d), ["今天", "天气", "很", "好"]);
    }

    #[test]
    fn characters_the_dictionary_does_not_know_are_words_of_one() {
        let d = dict(&[("今天", 0)]);
        assert_eq!(split("今天鑫鑫", &d), ["今天", "鑫", "鑫"]);
        assert_eq!(split("鑫", &d), ["鑫"]);
    }

    #[test]
    fn the_boundaries_always_tile_the_run() {
        let d = dict(&[("研究", 0), ("生命", 0), ("起源", 0), ("研究生", 0)]);
        for text in ["", "研", "研究生命起源", "起源研究生命", "鑫鑫鑫鑫鑫"] {
            let chars: Vec<char> = text.chars().collect();
            let cuts = cuts(&chars, &d);
            assert_eq!(cuts.first(), Some(&0), "{text}");
            assert_eq!(cuts.last(), Some(&chars.len()), "{text}");
            assert!(
                cuts.windows(2).all(|w| w[0] < w[1]),
                "{text} went backwards"
            );
        }
    }

    /// With everything else equal the longer opening word wins, so a run that
    /// the dictionary reads as one word is not cut into two.
    #[test]
    fn a_reading_that_ties_keeps_the_longer_first_word() {
        let d = dict(&[("中华", 0), ("中华人民", 0), ("人民", 0)]);
        assert_eq!(split("中华人民", &d), ["中华人民"]);
    }

    /// The fourth measure, on its own: two readings that cover the same span in
    /// the same shape differ only by which single character is the common one.
    #[test]
    fn the_commonest_single_character_breaks_a_remaining_tie() {
        let d = dict(&[("阿", 5), ("阿丙", 0), ("丙", 9000), ("丁", 3)]);
        // 阿丙丁 reads as 阿丙 · 丁 or 阿 · 丙 · 丁; the first covers the span in
        // two words and so wins on average length before frequency is reached.
        assert_eq!(split("阿丙丁", &d), ["阿丙", "丁"]);
    }

    #[test]
    fn an_empty_run_has_one_boundary_and_no_words() {
        let d = dict(&[("今天", 0)]);
        assert_eq!(cuts(&[], &d), vec![0]);
    }
}
