//! The word lists at `/usr/lib/mmseg/` and `/usr/lib/resegmenter/`, read as
//! data. **The file's hash table is never read**: an entry is a link to the
//! next, a byte count and UTF-8, so they walk as a sequence. Opens no file.

/// The longest run of characters that may be claimed as one word. The entries
/// past this length are proverbs and institution names, and a double-tap that
/// selected twenty characters answers a question nobody asked.
pub const MAX_WORD: usize = 8;

/// Which of the two entry layouts a file uses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Layout {
    /// `data_mmap`: link, byte count, character count, frequency, text, NUL.
    Mmseg,
    /// `words_list.mem`: link, byte count, text.
    Words,
}

/// One word's place in the file image. `at` is the offset of its text, and zero
/// marks an empty slot — the first eight bytes are the header.
#[derive(Debug, Clone, Copy, Default)]
struct Entry {
    at: u32,
    nbytes: u8,
    freq: u16,
}

/// A dictionary: the file's own bytes plus a table into them. **The table is
/// built here**, since the file's own hash is written down nowhere. Nothing is
/// copied out of the image, so a lookup neither decodes nor allocates.
pub struct Dict {
    bytes: Vec<u8>,
    slots: Vec<Entry>,
    mask: usize,
    len: usize,
    longest: usize,
}

impl Dict {
    /// Read a dictionary image. `None` where the entries do not tile the space
    /// the header claims, which is the one check that identifies the layout.
    pub fn parse(bytes: Vec<u8>, layout: Layout) -> Option<Self> {
        let table_at = u32::from_le_bytes(bytes.get(4..8)?.try_into().ok()?) as usize;
        if table_at > bytes.len() || table_at < 8 {
            return None;
        }

        let mut index = Vec::new();
        let mut longest = 0;
        let mut at = 8;
        while at < table_at {
            // The link is not followed; entries run in file order.
            let nbytes = *bytes.get(at + 4)? as usize;
            let (text_at, next) = match layout {
                // The character count sits beside the byte count and the
                // frequency after it; the text is terminated as well as counted.
                Layout::Mmseg => (at + 8, at + 8 + nbytes + 1),
                Layout::Words => (at + 5, at + 5 + nbytes),
            };
            if next > table_at || nbytes == 0 {
                return None;
            }
            let freq = match layout {
                Layout::Mmseg => u16::from_le_bytes(bytes.get(at + 6..at + 8)?.try_into().ok()?),
                Layout::Words => 0,
            };
            let text = bytes.get(text_at..text_at + nbytes)?;
            longest = longest.max(count_chars(text));
            index.push(Entry {
                at: text_at as u32,
                nbytes: nbytes as u8,
                freq,
            });
            at = next;
        }
        if at != table_at || index.is_empty() {
            return None;
        }

        // Half full at most, so a lookup that misses stops within a slot or two
        // of where it started.
        let mut capacity = 1usize;
        while capacity < index.len() * 2 {
            capacity <<= 1;
        }
        let mask = capacity - 1;
        let mut slots = vec![Entry::default(); capacity];
        for entry in &index {
            let mut slot = hash(word_of(&bytes, *entry)) as usize & mask;
            while slots[slot].at != 0 {
                slot = (slot + 1) & mask;
            }
            slots[slot] = *entry;
        }

        Some(Self {
            bytes,
            slots,
            mask,
            len: index.len(),
            longest: longest.min(MAX_WORD),
        })
    }

    /// How many words the file holds.
    pub fn len(&self) -> usize {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// The longest word worth looking for, in characters.
    ///
    /// Bounded by [`MAX_WORD`], so a caller may probe every length up to it.
    pub fn longest(&self) -> usize {
        self.longest
    }

    /// The frequency recorded for `word`, or `None` if there is no such word. A
    /// word held without one answers `Some(0)` — every word past one character.
    pub fn lookup(&self, word: &[char]) -> Option<u16> {
        if word.is_empty() {
            return None;
        }
        let mut slot = hash_chars(word) as usize & self.mask;
        loop {
            let entry = self.slots[slot];
            if entry.at == 0 {
                return None;
            }
            if same_word(word_of(&self.bytes, entry), word) {
                return Some(entry.freq);
            }
            slot = (slot + 1) & self.mask;
        }
    }

    pub fn contains(&self, word: &[char]) -> bool {
        self.lookup(word).is_some()
    }
}

fn word_of(bytes: &[u8], e: Entry) -> &[u8] {
    let at = e.at as usize;
    &bytes[at..at + e.nbytes as usize]
}

/// FNV-1a over the bytes of a word. Written out because the standard hasher
/// would need the word gathered into something that implements `Hash`, which is
/// the allocation this arrangement exists to avoid.
fn hash(bytes: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for &b in bytes {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x100_0000_01b3);
    }
    h
}

/// The same hash, over a word that has not been encoded yet.
fn hash_chars(word: &[char]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    let mut buf = [0u8; 4];
    for &c in word {
        for &b in c.encode_utf8(&mut buf).as_bytes() {
            h ^= u64::from(b);
            h = h.wrapping_mul(0x100_0000_01b3);
        }
    }
    h
}

/// Whether a stored word and a candidate are the same, without decoding the
/// one or encoding the other into anything lasting.
fn same_word(stored: &[u8], cand: &[char]) -> bool {
    let mut rest = stored;
    let mut buf = [0u8; 4];
    for &c in cand {
        let want = c.encode_utf8(&mut buf).as_bytes();
        if !rest.starts_with(want) {
            return false;
        }
        rest = &rest[want.len()..];
    }
    rest.is_empty()
}

/// Characters in a UTF-8 string, counted from its lead bytes. Invalid bytes
/// cannot make this panic, only wrong, and a wrong word never matches.
fn count_chars(bytes: &[u8]) -> usize {
    bytes.iter().filter(|b| (*b & 0xC0) != 0x80).count()
}

#[cfg(test)]
pub(crate) mod fixture {
    use super::Layout;

    /// Build a dictionary image the way the device's own writer does, so the
    /// parser is exercised against the layout rather than against itself.
    pub fn image(words: &[(&str, u16)], layout: Layout) -> Vec<u8> {
        let mut entries = Vec::new();
        for (word, freq) in words {
            let at = 8 + entries.len();
            let body = match layout {
                Layout::Mmseg => 8 + word.len() + 1,
                Layout::Words => 5 + word.len(),
            };
            entries.extend_from_slice(&((at + body) as u32).to_le_bytes());
            entries.push(word.len() as u8);
            if layout == Layout::Mmseg {
                entries.push(word.chars().count() as u8);
                entries.extend_from_slice(&freq.to_le_bytes());
            }
            entries.extend_from_slice(word.as_bytes());
            if layout == Layout::Mmseg {
                entries.push(0);
            }
        }

        let buckets: u32 = 4;
        let mut out = Vec::new();
        out.extend_from_slice(&buckets.to_le_bytes());
        out.extend_from_slice(&((8 + entries.len()) as u32).to_le_bytes());
        out.extend_from_slice(&entries);
        out.extend(std::iter::repeat_n(0u8, buckets as usize * 4));
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chars(s: &str) -> Vec<char> {
        s.chars().collect()
    }

    fn zh(words: &[(&str, u16)]) -> Dict {
        Dict::parse(fixture::image(words, Layout::Mmseg), Layout::Mmseg).expect("parses")
    }

    #[test]
    fn reads_back_every_word_it_was_given() {
        let d = zh(&[("今天", 0), ("天气", 0), ("的", 65535), ("研究生", 0)]);
        assert_eq!(d.len(), 4);
        for w in ["今天", "天气", "的", "研究生"] {
            assert!(d.contains(&chars(w)), "{w} missing");
        }
        assert!(!d.contains(&chars("命起")));
        assert!(
            !d.contains(&chars("今")),
            "a prefix of a word is not a word"
        );
    }

    #[test]
    fn the_frequency_column_comes_back_for_single_characters() {
        let d = zh(&[("的", 65535), ("是", 32081), ("今天", 0)]);
        assert_eq!(d.lookup(&chars("的")), Some(65535));
        assert_eq!(d.lookup(&chars("是")), Some(32081));
        assert_eq!(d.lookup(&chars("今天")), Some(0), "held, with no frequency");
        assert_eq!(d.lookup(&chars("无")), None);
    }

    /// The Japanese file records no frequencies and terminates nothing, so the
    /// same reader has to walk a shorter entry.
    #[test]
    fn the_other_layout_parses_the_same_way() {
        let img = fixture::image(&[("東京", 0), ("勉強", 0), ("連文節", 0)], Layout::Words);
        let d = Dict::parse(img, Layout::Words).expect("parses");
        assert_eq!(d.len(), 3);
        assert!(d.contains(&chars("東京")));
        assert!(d.contains(&chars("連文節")));
        assert_eq!(d.lookup(&chars("勉強")), Some(0));
    }

    #[test]
    fn a_file_of_the_wrong_layout_is_refused_rather_than_misread() {
        let img = fixture::image(&[("今天", 0), ("天气", 0)], Layout::Mmseg);
        assert!(Dict::parse(img.clone(), Layout::Words).is_none());
        assert!(Dict::parse(img, Layout::Mmseg).is_some());
    }

    #[test]
    fn rubbish_is_refused_rather_than_indexed() {
        assert!(Dict::parse(vec![], Layout::Mmseg).is_none());
        assert!(
            Dict::parse(vec![0; 8], Layout::Mmseg).is_none(),
            "no entries"
        );
        // A header pointing past the end of the file.
        let mut img = fixture::image(&[("今天", 0)], Layout::Mmseg);
        img[4..8].copy_from_slice(&u32::MAX.to_le_bytes());
        assert!(Dict::parse(img, Layout::Mmseg).is_none());
    }

    #[test]
    fn the_longest_word_is_reported_and_bounded() {
        let d = zh(&[("一", 0), ("今天天气", 0)]);
        assert_eq!(d.longest(), 4);
        // Longer than a double-tap should ever claim, so it is clamped.
        let long = "匹夫无罪怀璧其罪的说法";
        let d = zh(&[(long, 0)]);
        assert_eq!(d.longest(), MAX_WORD);
    }

    /// A word is confirmed byte by byte after the hash points at a slot, so
    /// this checks a set where a comparison that stopped mid-character, or a
    /// probe that gave up at the first occupied slot, would answer wrongly.
    #[test]
    fn lookup_finds_every_word_however_the_slots_collide() {
        let words = [
            "一", "丁", "中国", "中华", "书", "书法", "龙", "龙虾", "a", "ab", "あ", "東京",
        ];
        let pairs: Vec<(&str, u16)> = words.iter().map(|w| (*w, 0u16)).collect();
        let d = zh(&pairs);
        for w in words {
            assert!(d.contains(&chars(w)), "{w} missing");
        }
        for w in ["中", "书d", "龙b", "b", "東", "阿"] {
            assert!(!d.contains(&chars(w)), "{w} should be absent");
        }
    }

    #[test]
    fn an_empty_candidate_matches_nothing() {
        let d = zh(&[("今天", 0)]);
        assert!(!d.contains(&[]));
    }
}
