//! Where a word may be broken at the end of a line.
//!
//! Hyphenation is a pattern set in the Liang tradition: short letter sequences
//! carrying digits, the odd values marking a legal break, matched everywhere in
//! a word at once. The Kindle carries ten of them, at
//! `/usr/java/lib/dictionaries/hyphen/`, in the text form `libhyphen`
//! distributes — so karyll reads the firmware's data and bundles no patterns of
//! its own, the way [`crate::dict`] reads the firmware's word lists.
//!
//! A dictionary is levelled. The first level finds the breaks a word already
//! carries, where a hyphen or an apostrophe divides it, and each part it finds
//! is then run through the language's own patterns on its own.
//!
//! **The patterns are matched as bytes, against the word's UTF-8.** A file that
//! declares `ISO8859-1` — which the German, French, Italian, Dutch and
//! Portuguese ones do — is decoded on the way in, so a pattern spelling `ä`
//! carries the same two bytes the word does.
//!
//! **Nothing here opens a file.** The image arrives as bytes so that this
//! parses under `cargo test` on a machine that has none of them.

use std::collections::{BTreeSet, HashMap, VecDeque};

/// Break decisions taken by hand over the stock American English patterns.
/// See `dic/LICENSE` for where it came from and what it is for.
pub const EN_CURATION: &str = include_str!("dic/en.curation");

/// The character marking a permitted break inside a word.
pub const SOFT_HYPHEN: char = '\u{00ad}';

/// Why a dictionary is not usable.
#[derive(Debug)]
pub enum HyphenationError {
    /// The file declares a character set this reader cannot decode.
    UnsupportedCharset(String),
    /// A level declares no states, so it can match nothing.
    EmptyLevel,
    /// A pattern respells the word around the break, which this reader does not
    /// apply and must not silently drop.
    UnsupportedReplacement(String),
}

impl std::fmt::Display for HyphenationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            HyphenationError::UnsupportedCharset(c) => {
                write!(f, "hyphenation dictionary charset {c} is not one we decode")
            }
            HyphenationError::EmptyLevel => write!(f, "hyphenation dictionary level has no states"),
            HyphenationError::UnsupportedReplacement(p) => {
                write!(f, "hyphenation pattern {p} respells the word")
            }
        }
    }
}

impl std::error::Error for HyphenationError {}

// ---------------------------------------------------------------------------
// The matching automaton both halves of the reader build.
// ---------------------------------------------------------------------------

/// One state of a level's automaton.
#[derive(Debug, Clone, Copy)]
struct State {
    /// Byte offset of this state's digit string in the pool, or `None`.
    digits: Option<u32>,
    /// State to retry the current byte in, or `None` to restart from the root.
    fallback: Option<u32>,
    /// Where this state's transitions begin in the level's transition array.
    trans_start: u32,
    /// How many transitions this state has.
    trans_len: u32,
}

/// One level of a dictionary: a pattern set plus the limits it applies.
///
/// A level is a set of Liang patterns arranged as a trie of byte transitions
/// with fallback links, so one pass over a word finds every pattern that
/// matches anywhere in it. Each state carries the digit string of the pattern
/// that ends there, merged with the digit strings of every shorter pattern
/// ending at the same place, which is what lets a match be read off the state
/// alone rather than by walking the fallback chain at every byte.
#[derive(Debug, Clone)]
struct Level {
    left_min: usize,
    right_min: usize,
    compound_left_min: usize,
    compound_right_min: usize,
    states: Vec<State>,
    /// Destination state and matched byte, in state order.
    transitions: Vec<(u32, u8)>,
    /// NUL-terminated digit strings, indexed by a state's `digits` offset.
    /// Offset zero means "no digit string", so nothing usable is stored there.
    pool: Vec<u8>,
    /// Character sequences that suppress hyphenation next to them.
    no_hyphen: Vec<Vec<u8>>,
}

impl Level {
    /// The digit string for a state, as ASCII digits.
    fn digits(&self, state: u32) -> &[u8] {
        let Some(at) = self.states[state as usize].digits else {
            return &[];
        };
        let at = at as usize;
        let end = self.pool[at..]
            .iter()
            .position(|&b| b == 0)
            .map_or(self.pool.len(), |n| at + n);
        &self.pool[at..end]
    }

    /// Run the automaton over `word` and raise `values` wherever a pattern
    /// applies. `values[i]` governs a break before byte `i` of `word`.
    fn apply(&self, word: &[u8], values: &mut [u8]) {
        // Patterns are written against a word framed by `.` on both sides, so
        // that a pattern can anchor to the start or the end.
        let mut state: u32 = 0;
        let framed_len = word.len() + 2;
        for i in 0..=framed_len {
            let ch = match i {
                0 => b'.',
                _ if i <= word.len() => word[i - 1],
                _ if i == word.len() + 1 => b'.',
                // One step past the frame flushes any pattern that ends on it.
                _ => 0,
            };
            state = self.step(state, ch);
            let digits = self.digits(state);
            if digits.is_empty() {
                continue;
            }
            // The last digit lands on the byte just consumed, so the string
            // reaches back over the bytes that matched it.
            let Some(start) = (i + 1).checked_sub(digits.len()) else {
                continue;
            };
            for (k, &d) in digits.iter().enumerate() {
                let at = start + k;
                if at < values.len() && values[at] < d - b'0' {
                    values[at] = d - b'0';
                }
            }
        }
    }

    /// The state reached from `state` on `ch`, following fallbacks.
    fn step(&self, state: u32, ch: u8) -> u32 {
        let mut state = state;
        loop {
            let s = self.states[state as usize];
            let from = s.trans_start as usize;
            let found = self.transitions[from..from + s.trans_len as usize]
                .iter()
                .find(|(_, c)| *c == ch);
            if let Some(&(next, _)) = found {
                return next;
            }
            match s.fallback {
                Some(f) => state = f,
                // Nothing in the automaton continues this byte; restart.
                None => return 0,
            }
        }
    }
}

// ---------------------------------------------------------------------------
// The text dictionary form.
// ---------------------------------------------------------------------------

/// The level that finds the breaks a word already carries, generated for a
/// dictionary that declares patterns alone. Each mark a word contains is a
/// place it may break, and `NOHYPHEN` keeps a soft hyphen from being printed
/// against a mark that is already visible.
const COMPOUND_LEVEL: &str = "\
NOHYPHEN ',–,’,-
1-1
1'1
1–1
1’1
";

/// Characters a break must leave behind it where a file names no limit, which
/// is the convention a bare TeX pattern list is written to.
const DEFAULT_MIN: usize = 2;

/// Characters a break must leave against a boundary inside a compound where a
/// file names neither that limit nor the plain one.
const DEFAULT_COMPOUND_MIN: usize = 3;

/// Decode a dictionary image to text, by the character set it declares.
///
/// The first line names it. `ISO8859-1` maps byte for byte onto the first 256
/// code points, so decoding it is one cast per byte and no table.
fn decode(bytes: &[u8]) -> Result<String, HyphenationError> {
    let end = bytes
        .iter()
        .position(|&b| b == b'\n')
        .unwrap_or(bytes.len());
    let charset = String::from_utf8_lossy(&bytes[..end]).trim().to_string();
    if charset.eq_ignore_ascii_case("UTF-8") {
        return Ok(String::from_utf8_lossy(bytes).into_owned());
    }
    if charset.eq_ignore_ascii_case("ISO8859-1") || charset.eq_ignore_ascii_case("ISO-8859-1") {
        return Ok(bytes.iter().map(|&b| b as char).collect());
    }
    Err(HyphenationError::UnsupportedCharset(charset))
}

/// Read a text dictionary into its levels.
///
/// # Layout
///
/// The first line names the character set. Every line after it is one of:
///
/// | line | meaning |
/// |---|---|
/// | `LEFTHYPHENMIN n`, `RIGHTHYPHENMIN n` | characters a break must leave on each side of itself |
/// | `COMPOUNDLEFTHYPHENMIN n`, `COMPOUNDRIGHTHYPHENMIN n` | the same, for a part of a word that already breaks |
/// | `NOHYPHEN a,b,c` | sequences that forbid a break next to them |
/// | `NEXTLEVEL` | ends the compound level; the patterns of the language follow |
/// | `%…` | a comment |
/// | anything else | a pattern: letters carrying digits, `.` anchoring to a word edge |
///
/// A file that never says `NEXTLEVEL` is patterns alone, and the compound level
/// that finds the breaks a word already carries is generated for it.
///
/// # Building the automaton
///
/// Patterns go into a trie of byte transitions. A state's fallback is the
/// longest proper suffix of its own pattern that is also a state, so a walk
/// that runs out of transitions resumes at the longest still-matching suffix
/// rather than at the root. Because a match is read off the state alone, each
/// state's digit string absorbs the digit strings of its fallback chain — the
/// shorter patterns that end in the same place — merged digit by digit from the
/// right, keeping the larger of each pair.
fn parse_levels(text: &str) -> Result<Vec<Level>, HyphenationError> {
    let mut lines = text.lines();
    // The charset line is consumed by `decode`, which has already acted on it.
    lines.next();

    let mut sections = vec![Section::default()];
    for line in lines {
        let line = line.trim();
        if line.is_empty() || line.starts_with('%') {
            continue;
        }
        if line == "NEXTLEVEL" {
            sections.push(Section::default());
            continue;
        }
        sections.last_mut().unwrap().read(line)?;
    }

    // Patterns alone are the language's own level, under a generated one.
    if sections.len() == 1 {
        let mut compound = Section::default();
        for line in COMPOUND_LEVEL.lines() {
            compound.read(line)?;
        }
        let patterns = sections.pop().unwrap();
        compound.left_min = patterns.left_min;
        compound.right_min = patterns.right_min;
        compound.compound_left_min = patterns.compound_left_min;
        compound.compound_right_min = patterns.compound_right_min;
        sections = vec![compound, patterns];
    }

    let levels: Vec<Level> = sections.into_iter().map(Section::build).collect();
    if levels.iter().any(|l| l.states.is_empty()) {
        return Err(HyphenationError::EmptyLevel);
    }
    Ok(levels)
}

/// One level of a dictionary as the file states it.
#[derive(Default)]
struct Section {
    left_min: Option<usize>,
    right_min: Option<usize>,
    compound_left_min: Option<usize>,
    compound_right_min: Option<usize>,
    no_hyphen: Vec<Vec<u8>>,
    /// Each pattern's letters and its digit string, the digits already stripped
    /// of leading zeros.
    patterns: Vec<(Vec<u8>, Vec<u8>)>,
}

impl Section {
    /// Take one line of the file.
    fn read(&mut self, line: &str) -> Result<(), HyphenationError> {
        for (keyword, field) in [
            ("LEFTHYPHENMIN", 0),
            ("RIGHTHYPHENMIN", 1),
            ("COMPOUNDLEFTHYPHENMIN", 2),
            ("COMPOUNDRIGHTHYPHENMIN", 3),
        ] {
            if let Some(value) = line.strip_prefix(keyword) {
                let n = value.trim().parse().ok();
                match field {
                    0 => self.left_min = n,
                    1 => self.right_min = n,
                    2 => self.compound_left_min = n,
                    _ => self.compound_right_min = n,
                }
                return Ok(());
            }
        }
        if let Some(value) = line.strip_prefix("NOHYPHEN") {
            self.no_hyphen = value
                .trim()
                .split(',')
                .filter(|s| !s.is_empty())
                .map(|s| s.as_bytes().to_vec())
                .collect();
            return Ok(());
        }
        // A pattern that spells a word differently on either side of the break
        // is beyond what this reader applies, and dropping it would silently
        // hyphenate the word wrongly.
        if line.contains('/') {
            return Err(HyphenationError::UnsupportedReplacement(line.to_string()));
        }
        let mut letters: Vec<u8> = Vec::new();
        let mut digits: Vec<u8> = vec![b'0'];
        for &b in line.as_bytes() {
            if b.is_ascii_digit() {
                *digits.last_mut().unwrap() = b;
            } else {
                letters.push(b);
                digits.push(b'0');
            }
        }
        let start = digits.iter().take_while(|&&d| d == b'0').count();
        if start < digits.len() {
            self.patterns.push((letters, digits[start..].to_vec()));
        }
        Ok(())
    }

    /// Compile the section's patterns into a level.
    fn build(self) -> Level {
        let mut trie = Trie::default();
        for (letters, digits) in &self.patterns {
            trie.insert(letters, digits);
        }
        trie.link_and_merge();

        let left_min = self.left_min.unwrap_or(DEFAULT_MIN);
        let right_min = self.right_min.unwrap_or(DEFAULT_MIN);
        let mut level = Level {
            left_min,
            right_min,
            compound_left_min: self
                .compound_left_min
                .or(self.left_min)
                .unwrap_or(DEFAULT_COMPOUND_MIN),
            compound_right_min: self
                .compound_right_min
                .or(self.right_min)
                .unwrap_or(DEFAULT_COMPOUND_MIN),
            states: Vec::with_capacity(trie.nodes.len()),
            transitions: Vec::new(),
            pool: vec![0],
            no_hyphen: self.no_hyphen,
        };
        for node in &trie.nodes {
            let digits = (!node.digits.is_empty()).then(|| {
                let at = level.pool.len() as u32;
                level.pool.extend_from_slice(&node.digits);
                level.pool.push(0);
                at
            });
            let trans_start = level.transitions.len() as u32;
            level
                .transitions
                .extend(node.transitions.iter().map(|&(byte, to)| (to, byte)));
            level.states.push(State {
                digits,
                fallback: node.fallback,
                trans_start,
                trans_len: node.transitions.len() as u32,
            });
        }
        level
    }
}

/// A trie of pattern letters under construction.
#[derive(Default)]
struct Trie {
    nodes: Vec<Node>,
}

#[derive(Default)]
struct Node {
    /// Matched byte and the state it leads to.
    transitions: Vec<(u8, u32)>,
    /// ASCII digits, leading zeros already stripped.
    digits: Vec<u8>,
    fallback: Option<u32>,
}

impl Trie {
    /// Place one pattern, replacing any digits already held for its letters.
    fn insert(&mut self, letters: &[u8], digits: &[u8]) {
        if self.nodes.is_empty() {
            self.nodes.push(Node::default());
        }
        let mut at = 0u32;
        for &byte in letters {
            at = match self.nodes[at as usize]
                .transitions
                .iter()
                .find(|(b, _)| *b == byte)
            {
                Some(&(_, next)) => next,
                None => {
                    let next = self.nodes.len() as u32;
                    self.nodes.push(Node::default());
                    self.nodes[at as usize].transitions.push((byte, next));
                    next
                }
            };
        }
        self.nodes[at as usize].digits = digits.to_vec();
    }

    /// Point each state at its longest proper suffix and absorb that suffix's
    /// digits, so a state states every pattern that ends where it does.
    fn link_and_merge(&mut self) {
        if self.nodes.is_empty() {
            self.nodes.push(Node::default());
            return;
        }
        for node in &mut self.nodes {
            node.transitions.sort_unstable();
        }
        let mut queue: VecDeque<u32> = VecDeque::new();
        for i in 0..self.nodes[0].transitions.len() {
            let (_, child) = self.nodes[0].transitions[i];
            self.nodes[child as usize].fallback = Some(0);
            queue.push_back(child);
        }
        while let Some(at) = queue.pop_front() {
            let fallback = self.nodes[at as usize].fallback.unwrap_or(0);
            let digits = merge(
                &self.nodes[at as usize].digits,
                &self.nodes[fallback as usize].digits,
            );
            self.nodes[at as usize].digits = digits;
            for i in 0..self.nodes[at as usize].transitions.len() {
                let (byte, child) = self.nodes[at as usize].transitions[i];
                self.nodes[child as usize].fallback = Some(self.step(fallback, byte));
                queue.push_back(child);
            }
        }
    }

    /// The state reached from `at` on `byte`, following fallbacks. Every
    /// fallback it consults belongs to a shorter pattern, so it is already set.
    fn step(&self, at: u32, byte: u8) -> u32 {
        let mut at = at;
        loop {
            if let Some(&(_, next)) = self.nodes[at as usize]
                .transitions
                .iter()
                .find(|(b, _)| *b == byte)
            {
                return next;
            }
            match self.nodes[at as usize].fallback {
                Some(f) => at = f,
                None => return 0,
            }
        }
    }
}

/// Two digit strings that end in the same place, merged digit by digit from the
/// right, keeping the larger of each pair.
fn merge(a: &[u8], b: &[u8]) -> Vec<u8> {
    if a.is_empty() {
        return b.to_vec();
    }
    if b.is_empty() {
        return a.to_vec();
    }
    let len = a.len().max(b.len());
    let mut out = Vec::with_capacity(len);
    for i in 0..len {
        let from = |s: &[u8]| match (i + s.len()).checked_sub(len) {
            Some(k) => s[k],
            None => b'0',
        };
        out.push(from(a).max(from(b)));
    }
    out
}

// ---------------------------------------------------------------------------
// Break decisions taken by hand.
// ---------------------------------------------------------------------------

/// The words a dictionary breaks by hand rather than by pattern.
///
/// A pattern set is generated from a word list by machine and is judged on
/// aggregate: it may break a rare word in an odd place and still be a good set.
/// A writer meets the commonest words on every line, though, so a poor break in
/// one of those is seen constantly, and no adjustment of the pattern machinery
/// reaches it — the decision is per word.
///
/// # Layout
///
/// One entry per line, plus one keyword:
///
/// | line | meaning |
/// |---|---|
/// | `MINWORDLENGTH n` | words shorter than this are never broken |
/// | `word` | never broken |
/// | `wo-rd` | broken only where the marks are |
/// | `%…` | a comment |
///
/// An entry replaces the patterns for the word it names, and stands whatever
/// the word's length, being itself the exception to the line above. The
/// dictionary's own limits on how near an edge a break may fall still apply, as
/// they do to every break. Entries are matched without regard to case, and a
/// word that already carries a mark cannot be named by one, the marks being the
/// breaks; a word inside such a compound can.
#[derive(Debug, Clone, Default)]
struct Curation {
    /// Words shorter than this are not broken at all.
    min_word_length: usize,
    /// Each word, lowercased, against the byte offsets it breaks at.
    words: HashMap<String, Vec<usize>>,
}

impl Curation {
    /// Read a curation file.
    fn parse(text: &str) -> Curation {
        let mut curation = Curation::default();
        for line in text.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('%') {
                continue;
            }
            if let Some(value) = line.strip_prefix("MINWORDLENGTH") {
                curation.min_word_length = value.trim().parse().unwrap_or_default();
                continue;
            }
            let mut word = String::with_capacity(line.len());
            let mut breaks = Vec::new();
            for part in line.split('-') {
                if !word.is_empty() {
                    breaks.push(word.len());
                }
                word.push_str(part);
            }
            curation.words.insert(word.to_lowercase(), breaks);
        }
        curation
    }

    /// The breaks stated for a word, if it is one of them.
    fn breaks(&self, word: &str) -> Option<&[usize]> {
        if self.words.is_empty() {
            return None;
        }
        self.words
            .get(word)
            .or_else(|| self.words.get(&word.to_lowercase()))
            .map(Vec::as_slice)
    }
}

// ---------------------------------------------------------------------------
// The dictionary itself.
// ---------------------------------------------------------------------------

/// A hyphenation dictionary for one language.
#[derive(Debug, Clone)]
pub struct Hyphenator {
    levels: Vec<Level>,
    curation: Curation,
}

impl Hyphenator {
    /// Read a dictionary image, decoding it by the character set it declares.
    pub fn parse(bytes: &[u8]) -> Result<Self, HyphenationError> {
        Self::from_patterns(&decode(bytes)?)
    }

    /// Read a pattern set already decoded, charset line and all.
    pub fn from_patterns(text: &str) -> Result<Self, HyphenationError> {
        Ok(Hyphenator {
            levels: parse_levels(text)?,
            curation: Curation::default(),
        })
    }

    /// The same dictionary with per-word decisions over it.
    pub fn curated(mut self, text: &str) -> Self {
        self.curation = Curation::parse(text);
        self
    }

    /// How many states the language's own level holds, for a log line that says
    /// a dictionary arrived rather than only that a file was read.
    pub fn states(&self) -> usize {
        self.levels.last().map_or(0, |l| l.states.len())
    }

    /// Least number of characters that must precede a break.
    pub fn left_min(&self) -> usize {
        self.levels[0].left_min
    }

    /// Least number of characters that must follow a break.
    pub fn right_min(&self) -> usize {
        self.levels[0].right_min
    }

    /// Shortest word the dictionary breaks at all, zero where it sets no such
    /// limit.
    pub fn min_word_length(&self) -> usize {
        self.curation.min_word_length
    }

    /// Set the shortest word to break, overriding what the dictionary carries.
    pub fn set_min_word_length(&mut self, characters: usize) {
        self.curation.min_word_length = characters;
    }

    /// Character offsets within `word` at which it may be broken, ascending.
    ///
    /// The unit the wrapper works in — [`crate::wrap`] indexes characters, and
    /// a byte offset would have to be converted at every call site.
    pub fn breaks_in(&self, word: &[char]) -> Vec<usize> {
        let text: String = word.iter().collect();
        let mut at = self.hyphenate(&text).into_iter().peekable();
        let mut out = Vec::new();
        let mut byte = 0usize;
        for (index, c) in text.chars().enumerate() {
            while at.peek().is_some_and(|&b| b < byte) {
                at.next();
            }
            if at.peek() == Some(&byte) {
                out.push(index);
                at.next();
            }
            byte += c.len_utf8();
        }
        out
    }

    /// Byte offsets in `word` at which it may be broken, ascending. Each is the
    /// index of the first byte of the part that would move to the next line.
    pub fn hyphenate(&self, word: &str) -> Vec<usize> {
        if word.is_empty() {
            return Vec::new();
        }
        // Patterns are written in lower case, so a capital matches nothing and
        // would leave the word to be broken by whatever its tail matches.
        if !word.chars().any(char::is_uppercase) {
            return self.matched_breaks(word);
        }
        let (lowered, origin) = lowercase_with_origins(word);
        self.matched_breaks(&lowered)
            .into_iter()
            .filter_map(|at| origin[at])
            .collect()
    }

    /// Byte offsets this dictionary permits a break at, for a word already in
    /// the case the patterns are written in.
    fn matched_breaks(&self, word: &str) -> Vec<usize> {
        let bytes = word.as_bytes();
        let top = &self.levels[0];
        // `values[i]` governs a break before byte `i`; an odd value permits one.
        let mut values = vec![0u8; bytes.len() + 1];
        top.apply(bytes, &mut values);

        let mut breaks: BTreeSet<usize> = BTreeSet::new();
        // Compound boundaries the first level found, and the segments between
        // them, which the deeper levels each run over on their own.
        let mut bounds: Vec<usize> = (1..bytes.len())
            .filter(|&i| values[i] % 2 == 1 && word.is_char_boundary(i))
            .collect();
        for &b in &bounds {
            breaks.insert(b);
        }
        bounds.insert(0, 0);
        bounds.push(bytes.len());

        for pair in bounds.windows(2) {
            let (from, to) = (pair[0], pair[1]);
            let segment = &bytes[from..to];
            if segment.is_empty() {
                continue;
            }
            // A word decided by hand is not put to the patterns at all, and
            // stands whatever its length.
            if let Some(stated) = self.curation.breaks(&word[from..to]) {
                breaks.extend(stated.iter().map(|&at| from + at));
                continue;
            }
            if word[from..to].chars().count() < self.curation.min_word_length {
                continue;
            }
            for level in &self.levels[1..] {
                let mut inner = vec![0u8; segment.len() + 1];
                level.apply(segment, &mut inner);
                // A part that starts or ends inside the word keeps its distance
                // from the boundary by the compound limits rather than the
                // plain ones.
                let head = if from == 0 {
                    top.left_min
                } else {
                    top.compound_left_min
                };
                let tail = if to == bytes.len() {
                    top.right_min
                } else {
                    top.compound_right_min
                };
                for (i, &v) in inner.iter().enumerate() {
                    if v % 2 == 0 || i == 0 || i == segment.len() {
                        continue;
                    }
                    let at = from + i;
                    if !word.is_char_boundary(at) {
                        continue;
                    }
                    if chars_between(word, from, at) < head || chars_between(word, at, to) < tail {
                        continue;
                    }
                    breaks.insert(at);
                }
            }
        }

        breaks
            .into_iter()
            .filter(|&at| {
                chars_between(word, 0, at) >= top.left_min
                    && chars_between(word, at, bytes.len()) >= top.right_min
                    && !self.suppressed(word, at)
            })
            .collect()
    }

    /// `word` with a soft hyphen at each permitted break.
    pub fn with_soft_hyphens(&self, word: &str) -> String {
        let breaks = self.hyphenate(word);
        if breaks.is_empty() {
            return word.to_string();
        }
        let mut out = String::with_capacity(word.len() + breaks.len() * 2);
        let mut last = 0;
        for at in breaks {
            out.push_str(&word[last..at]);
            out.push(SOFT_HYPHEN);
            last = at;
        }
        out.push_str(&word[last..]);
        out
    }

    /// Whether a no-hyphen sequence sits against a break, which forbids it.
    fn suppressed(&self, word: &str, at: usize) -> bool {
        self.levels.iter().any(|level| {
            level.no_hyphen.iter().any(|seq| {
                word.as_bytes()[at..].starts_with(seq)
                    || word.as_bytes()[..at].ends_with(seq.as_slice())
            })
        })
    }
}

/// Characters between two byte offsets of `s`.
fn chars_between(s: &str, from: usize, to: usize) -> usize {
    s[from..to].chars().count()
}

/// `word` in lower case, and where each byte offset of it sits in the original.
/// A character whose lower case spells out as several characters has no offset
/// of its own past its first byte, so a break inside one is not a break in the
/// word.
fn lowercase_with_origins(word: &str) -> (String, Vec<Option<usize>>) {
    let mut lowered = String::with_capacity(word.len());
    let mut origin = Vec::with_capacity(word.len() + 1);
    for (at, ch) in word.char_indices() {
        lowered.extend(ch.to_lowercase());
        origin.push(Some(at));
        origin.resize(lowered.len(), None);
    }
    origin.push(Some(word.len()));
    (lowered, origin)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A dictionary of the given lines, with the charset line already on it.
    fn dictionary(body: &str) -> Hyphenator {
        Hyphenator::from_patterns(&format!("UTF-8\n{body}")).unwrap()
    }

    #[test]
    fn breaks_where_a_pattern_says() {
        let h = dictionary("LEFTHYPHENMIN 1\nRIGHTHYPHENMIN 1\na1b\n");
        assert_eq!(h.hyphenate("xaby"), vec![2]);
        assert_eq!(h.with_soft_hyphens("xaby"), "xa\u{ad}by");
        assert_eq!(h.hyphenate("xyz"), Vec::<usize>::new());
    }

    #[test]
    fn an_even_digit_outranks_an_odd_one() {
        // Both patterns land on the same place; the higher value decides, and
        // an even value is a refusal.
        let h = dictionary("LEFTHYPHENMIN 1\nRIGHTHYPHENMIN 1\na1b\nxa2by\n");
        assert_eq!(h.hyphenate("xaby"), Vec::<usize>::new());
    }

    #[test]
    fn a_shorter_pattern_still_matches_under_a_longer_one() {
        // Walking "abc" ends in a state built for `a2bc`; the break comes from
        // `b1c`, which ends in the same place and is only reachable through the
        // fallback chain.
        let h = dictionary("LEFTHYPHENMIN 1\nRIGHTHYPHENMIN 1\na2bc\nb1c\n");
        assert_eq!(h.hyphenate("abcd"), vec![2]);
    }

    #[test]
    fn limits_keep_breaks_away_from_the_edges() {
        let h = dictionary("LEFTHYPHENMIN 2\nRIGHTHYPHENMIN 3\na1b\n");
        assert_eq!(h.hyphenate("xab"), Vec::<usize>::new());
        assert_eq!(h.hyphenate("xabyz"), vec![2]);
    }

    #[test]
    fn a_generated_compound_level_breaks_a_marked_word_into_parts() {
        let h = dictionary("LEFTHYPHENMIN 1\nRIGHTHYPHENMIN 1\na1b\n");
        assert_eq!(h.hyphenate("ab-ab"), vec![1, 4]);
        // The mark is a break already; no soft hyphen is offered against it.
        assert_eq!(h.with_soft_hyphens("ab-ab"), "a\u{ad}b-a\u{ad}b");
        assert_eq!(h.with_soft_hyphens("ab’ab"), "a\u{ad}b’a\u{ad}b");
    }

    #[test]
    fn a_declared_compound_level_is_taken_as_written() {
        let h = dictionary("LEFTHYPHENMIN 1\nRIGHTHYPHENMIN 1\n1=1\nNEXTLEVEL\na1b\n");
        // `=` is the only mark this file calls a compound boundary, and it
        // declares no `NOHYPHEN`, so a break is offered on both sides of it.
        assert_eq!(h.hyphenate("ab=ab"), vec![1, 2, 3, 4]);
        // A hyphen divides nothing here, so no break is offered against it and
        // the word is matched whole.
        assert_eq!(h.hyphenate("ab-ab"), vec![1, 4]);
    }

    #[test]
    fn multibyte_letters_break_only_on_character_boundaries() {
        let h = dictionary("LEFTHYPHENMIN 1\nRIGHTHYPHENMIN 1\né1è\n");
        let word = "xéèy";
        assert_eq!(h.hyphenate(word), vec![3]);
        assert_eq!(h.with_soft_hyphens(word), "xé\u{ad}èy");
    }

    /// The German, French, Italian, Dutch and Portuguese files the Kindle
    /// carries all declare this, and a pattern spelling an umlaut has to end up
    /// with the same bytes the word does.
    fn latin1(body: &str) -> Vec<u8> {
        let mut bytes = b"ISO8859-1\n".to_vec();
        bytes.extend(body.chars().map(|c| c as u8));
        bytes
    }

    #[test]
    fn an_iso8859_1_dictionary_is_decoded_on_the_way_in() {
        let h = Hyphenator::parse(&latin1("LEFTHYPHENMIN 1\nRIGHTHYPHENMIN 1\nä1ö\n")).unwrap();
        // Two bytes each in UTF-8, so a byte offset is not a character offset.
        assert_eq!(h.hyphenate("xäöy"), vec![3]);
        assert_eq!(h.breaks_in(&"xäöy".chars().collect::<Vec<_>>()), vec![2]);
    }

    #[test]
    fn a_charset_we_cannot_decode_is_refused() {
        assert!(matches!(
            Hyphenator::parse(b"KOI8-R\na1b\n"),
            Err(HyphenationError::UnsupportedCharset(c)) if c == "KOI8-R"
        ));
    }

    #[test]
    fn a_pattern_that_respells_the_word_is_refused_rather_than_dropped() {
        assert!(matches!(
            Hyphenator::from_patterns("UTF-8\nLEFTHYPHENMIN 1\nc1k/k=k\n"),
            Err(HyphenationError::UnsupportedReplacement(_))
        ));
    }

    #[test]
    fn breaks_are_reported_in_characters_for_the_wrapper() {
        let h = dictionary("LEFTHYPHENMIN 1\nRIGHTHYPHENMIN 1\na1b\n");
        assert_eq!(h.breaks_in(&"xaby".chars().collect::<Vec<_>>()), vec![2]);
        assert_eq!(h.breaks_in(&[]), Vec::<usize>::new());
    }

    #[test]
    fn a_word_decided_by_hand_divides_as_decided() {
        let h = dictionary("LEFTHYPHENMIN 1\nRIGHTHYPHENMIN 1\na1b\n").curated("aab\nab-ab\n");
        // Named and unmarked: never divided, whatever the patterns say.
        assert_eq!(h.hyphenate("aab"), Vec::<usize>::new());
        // Named and marked: divided where the marks are and nowhere else.
        assert_eq!(h.hyphenate("abab"), vec![2]);
    }

    #[test]
    fn a_short_word_is_left_whole() {
        let h = dictionary("LEFTHYPHENMIN 1\nRIGHTHYPHENMIN 1\na1b\n").curated("MINWORDLENGTH 6\n");
        assert_eq!(h.min_word_length(), 6);
        assert_eq!(h.hyphenate("xaby"), Vec::<usize>::new());
        assert_eq!(h.hyphenate("xxabyy"), vec![3]);
    }

    #[test]
    fn the_bundled_curation_parses_and_states_a_minimum() {
        let c = Curation::parse(EN_CURATION);
        assert_eq!(c.min_word_length, 6);
        assert_eq!(c.breaks("understanding"), Some(&[5, 10][..]));
        assert_eq!(c.breaks("father"), Some(&[][..]));
        // Case is not part of the match, and a word not named is not claimed.
        assert_eq!(c.breaks("October"), Some(&[4][..]));
        assert_eq!(c.breaks("zzzz"), None);
    }
}
