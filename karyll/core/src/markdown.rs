//! Markdown markup analysis.
//!
//! karyll shows Markdown source, styled — it never hides the syntax and never
//! renders a preview. A heading stays `## Heading`; the `##` is just drawn
//! quieter than the words. So the parser's job is not to strip markers but to
//! *label* every character, including the markers, and hand the whole labelled
//! run to the renderer.
//!
//! That means spans tile their line exactly: concatenating them reproduces the
//! source. Nothing is dropped and nothing is synthesised.
//!
//! The dialect is deliberately small — the subset that appears in prose.
//! Indented code blocks are not recognised, because a four-space indent is more
//! likely to be a writer's indentation than code.

use std::ops::Range;

/// What kind of line this is. Drives size, weight and indent when drawing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Block {
    Paragraph,
    Blank,
    /// `#` through `######`.
    Heading(u8),
    Quote,
    ListItem {
        ordered: bool,
    },
    /// A bullet with a box after it: `- [ ] ` or `- [x] `.
    ///
    /// Three things differ from a plain [`Block::ListItem`]: Enter continues it
    /// as an *unticked* item, a key ticks it, and a done one is drawn struck
    /// through.
    Task {
        done: bool,
    },
    /// The ``` or ~~~ line itself.
    Fence,
    /// A line inside a fenced block.
    Code,
    /// A horizontal rule: `---`, `***` or `___`.
    Rule,
}

/// What a run of characters is, semantically. The renderer decides how each
/// one looks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Style {
    /// Prose.
    Text,
    /// Markup characters: `##`, `**`, `` ` ``, `](`. Drawn quieter than prose.
    Syntax,
    Emphasis,
    Strong,
    /// `~~cut this~~`, and the prose of a task that is done.
    ///
    /// **Not a face.** No family carries a struck-through cut; the renderer
    /// draws the body face and rules a line through it. A rule is
    /// orthogonal to the face under it and one `style` per span cannot say
    /// both, so this does not combine the way [`Style::StrongEmphasis`] does.
    Strikethrough,
    /// Both at once: `**strong with *emphasis* inside it**`. A style of its own
    /// rather than a pair of flags, because that is the smallest change that
    /// lets one span say it — and the face it asks for, `Role::BodyBoldItalic`,
    /// already existed for emphasis inside a heading.
    StrongEmphasis,
    Code,
    /// The visible text of a link.
    Link,
    /// The target of a link. Quiet, like syntax.
    Url,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Span {
    pub range: Range<usize>,
    pub style: Style,
}

/// One logical line, labelled.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LineMarkup {
    /// The line's characters, not including the newline that ended it.
    pub range: Range<usize>,
    pub block: Block,
    /// Tiles `range` exactly, in order.
    pub spans: Vec<Span>,
    /// What `==…==` covers, **markers included**, in order and never
    /// overlapping.
    ///
    /// A highlight is a field behind the text rather than a face for it, and a
    /// run can be bold *and* highlighted — which one [`Style`] per span cannot
    /// say. So the field is carried here as a range and the body keeps its own
    /// styles: `==a **b** c==` is bold in the middle of a highlight. The `==`
    /// stay [`Style::Syntax`] and are drawn quiet on top of the field.
    pub highlights: Vec<Range<usize>>,
}

impl LineMarkup {
    /// How many characters at the head of the line are its block marker —
    /// `## `, `> `, `- [x] ` — including the space that ends it and any indent
    /// before it. Zero for a line that has none.
    ///
    /// Taken from the spans rather than parsed again, so it cannot disagree
    /// with what [`analyze`] decided; the block is what says whether the
    /// leading syntax span is a marker at all, since a fence and a rule are
    /// syntax end to end and neither has one.
    pub fn marker(&self) -> usize {
        if !matches!(
            self.block,
            Block::Heading(_) | Block::Quote | Block::ListItem { .. } | Block::Task { .. }
        ) {
            return 0;
        }
        match self.spans.first() {
            Some(span) if span.style == Style::Syntax => span.range.len(),
            _ => 0,
        }
    }
}

/// Label every line of `chars`.
pub fn analyze(chars: &[char]) -> Vec<LineMarkup> {
    let mut out = Vec::new();
    let mut in_fence = false;
    let mut start = 0usize;

    loop {
        let end = (start..chars.len())
            .find(|&i| chars[i] == '\n')
            .unwrap_or(chars.len());
        out.push(analyze_line(chars, start..end, &mut in_fence));
        if end >= chars.len() {
            break;
        }
        start = end + 1;
    }
    out
}

fn analyze_line(chars: &[char], range: Range<usize>, in_fence: &mut bool) -> LineMarkup {
    let line = &chars[range.clone()];
    let base = range.start;

    // Inside a fence nothing is markup until the closing fence.
    if *in_fence {
        return if is_fence(line) {
            *in_fence = false;
            all_syntax(range, Block::Fence)
        } else {
            let block = Block::Code;
            let spans = one_span(range.clone(), Style::Code);
            LineMarkup {
                range,
                block,
                spans,
                highlights: Vec::new(),
            }
        };
    }
    if is_fence(line) {
        *in_fence = true;
        return all_syntax(range, Block::Fence);
    }

    if line.iter().all(|c| c.is_whitespace()) {
        return LineMarkup {
            range: range.clone(),
            block: Block::Blank,
            spans: one_span(range, Style::Text),
            highlights: Vec::new(),
        };
    }
    if is_rule(line) {
        return all_syntax(range, Block::Rule);
    }

    // Leading marker, if any. `marker` is how many characters of the line it
    // spans, including the space that terminates it.
    let indent = line.iter().take_while(|c| **c == ' ').count().min(3);
    let rest = &line[indent..];
    let (block, marker) = match leading_marker(rest) {
        Some((block, n)) => (block, indent + n),
        None => (Block::Paragraph, 0),
    };

    let mut spans = Vec::new();
    if marker > 0 {
        spans.push(Span {
            range: base..base + marker,
            style: Style::Syntax,
        });
    }
    let mut highlights = Vec::new();
    spans.extend(inline(chars, base + marker..range.end, &mut highlights));
    // **A ticked task reads as struck-out prose.** `[x]` against `[ ]` is one
    // glyph of difference on a one-bit panel, which is not enough to pick a
    // done item out of a list at a glance.
    if block == (Block::Task { done: true }) {
        for span in &mut spans {
            if span.style == Style::Text {
                span.style = Style::Strikethrough;
            }
        }
    }
    LineMarkup {
        range,
        block,
        spans,
        highlights,
    }
}

/// A line with its markup taken out: `## The **plan**` reads `The plan`.
///
/// For the places that list a line rather than draw it — the outline is the one
/// that wanted it — where `#` and `**` are noise: a reader running an eye down a
/// column of section names is not reading Markdown, they are reading names.
///
/// **Built from the spans rather than by stripping characters**, so it cannot
/// disagree with what the renderer draws. Anything [`analyze`] calls syntax is
/// dropped and everything else kept, so a heading with a link or emphasis in it
/// keeps the words and loses only the punctuation that made them emphatic.
pub fn plain(chars: &[char], line: &LineMarkup) -> String {
    let text: String = line
        .spans
        .iter()
        .filter(|span| !matches!(span.style, Style::Syntax | Style::Url))
        .flat_map(|span| chars[span.range.clone()].iter())
        .collect();
    text.trim().to_string()
}

/// What pressing Enter at the end of `line` should do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Continue {
    /// An ordinary line break.
    Break,
    /// A line break followed by this, carrying the list or quote on. Includes
    /// the original indentation, so a nested item stays nested.
    Marker(String),
    /// The line is a marker with nothing written after it, so the writer is
    /// finishing the list rather than adding to it. Delete this many characters
    /// back — the whole empty item — and break.
    End(usize),
}

/// Whether Enter continues the block `line` is in, and with what.
///
/// **The single biggest flow win available in a Markdown editor**: without it
/// every bullet after the first is typed by hand, which is enough friction to
/// stop people using lists at all.
///
/// Three rules, and the third is the one people forget:
///
/// - A list item or quote with something in it continues, marker and all.
/// - An **ordered** list counts on: `3.` is followed by `4.`, because a writer
///   renumbering by hand is exactly what the marker exists to avoid.
/// - A marker with **nothing after it** ends the block instead. Pressing Enter
///   twice is how every editor gets out of a list, and without this the writer
///   would be trapped adding empty bullets.
///
/// Headings deliberately do not continue: `# Title` is followed by prose, never
/// by another heading. Fences do not either, since the closing ``` is a
/// different thing from a new line inside the block.
pub fn continues(line: &[char]) -> Continue {
    let indent = line.iter().take_while(|c| **c == ' ').count().min(3);
    let rest = &line[indent..];
    let Some((block, marker)) = leading_marker(rest) else {
        return Continue::Break;
    };
    if !matches!(
        block,
        Block::ListItem { .. } | Block::Quote | Block::Task { .. }
    ) {
        return Continue::Break;
    }
    // Nothing but the marker and blank space after it.
    if rest[marker..].iter().all(|c| c.is_whitespace()) {
        return Continue::End(line.len());
    }

    let mut out: String = std::iter::repeat_n(' ', indent).collect();
    match block {
        Block::ListItem { ordered: true } => {
            let digits: String = rest.iter().take_while(|c| c.is_ascii_digit()).collect();
            let next = digits.parse::<u32>().unwrap_or(0).saturating_add(1);
            // The delimiter the writer chose — `.` or `)` — is kept.
            let delimiter = rest[digits.len()];
            out.push_str(&format!("{next}{delimiter} "));
        }
        // **Always unticked**, whatever this one is: nobody writes down a task
        // they have already finished.
        Block::Task { .. } => {
            out.extend(&rest[..2]);
            out.push_str("[ ] ");
        }
        _ => out.extend(&rest[..marker]),
    }
    Continue::Marker(out)
}

/// Put `marker` around `span`, or take it off if it is already there.
///
/// Returns the range to replace and what to put in it, so the caller makes one
/// edit and one undo step out of it.
///
/// **A toggle rather than an insert**, because the key that adds emphasis is
/// the key a writer reaches for to remove it, and `****bold****` is what
/// happens otherwise.
pub fn toggle_emphasis(chars: &[char], span: Range<usize>, marker: &str) -> (Range<usize>, String) {
    let width = marker.chars().count();
    let inner: String = chars[span.clone()].iter().collect();

    // Already wrapped, with the markers just outside the span — which is what
    // wrapping leaves behind, so pressing the key twice undoes it.
    let before = span.start.checked_sub(width).map(|s| s..span.start);
    let after = span.end..(span.end + width).min(chars.len());
    if let Some(before) = before
        && chars[before.clone()].iter().collect::<String>() == marker
        && chars[after.clone()].iter().collect::<String>() == marker
    {
        return (before.start..after.end, inner);
    }

    // Or with the markers inside it, which is what selecting a whole bold word
    // by double-tapping gives.
    if inner.len() >= width * 2 && inner.starts_with(marker) && inner.ends_with(marker) {
        let kept: String = inner[width..inner.len() - width].to_string();
        return (span, kept);
    }

    (span, format!("{marker}{inner}{marker}"))
}

/// Set `line` to heading `level`, or back to a paragraph if it is already at
/// that level.
///
/// Any heading already there is replaced rather than added to, so `##` at level
/// one is `#` and not `###`. Indentation is dropped: a heading is not an
/// indented thing, and leaving spaces in front of the hashes would stop it
/// being a heading at all.
pub fn toggle_heading(line: &[char], level: u8) -> String {
    let indent = line.iter().take_while(|c| **c == ' ').count().min(3);
    let rest = &line[indent..];
    let hashes = rest.iter().take_while(|c| **c == '#').count();
    let had = (1..=6).contains(&hashes) && rest.get(hashes) == Some(&' ');
    let body: String = if had {
        rest[hashes + 1..].iter().collect()
    } else {
        rest.iter().collect()
    };

    if had && hashes as u8 == level {
        return body;
    }
    let level = level.clamp(1, 6) as usize;
    format!("{} {body}", "#".repeat(level))
}

#[cfg(test)]
mod authoring {
    use super::*;

    fn chars(s: &str) -> Vec<char> {
        s.chars().collect()
    }

    #[test]
    fn emphasis_wraps_what_is_selected() {
        let c = chars("make this bold");
        let (range, text) = toggle_emphasis(&c, 5..9, "**");
        assert_eq!(range, 5..9);
        assert_eq!(text, "**this**");
    }

    #[test]
    fn the_same_key_takes_it_off_again() {
        // The markers sit just outside the span, which is where wrapping left
        // them — so pressing the key twice is a round trip.
        let c = chars("make **this** bold");
        let (range, text) = toggle_emphasis(&c, 7..11, "**");
        assert_eq!(range, 5..13);
        assert_eq!(text, "this");
    }

    #[test]
    fn a_selection_that_swallowed_the_markers_unwraps_too() {
        // Double-tapping a bold word selects the whole run, markers included.
        let c = chars("make **this** bold");
        let (range, text) = toggle_emphasis(&c, 5..13, "**");
        assert_eq!(range, 5..13);
        assert_eq!(text, "this");
    }

    #[test]
    fn italic_and_bold_are_the_same_rule_with_different_markers() {
        let c = chars("word");
        assert_eq!(toggle_emphasis(&c, 0..4, "*").1, "*word*");
        assert_eq!(toggle_emphasis(&chars("*word*"), 1..5, "*").1, "word");
    }

    #[test]
    fn emphasis_works_on_cjk_which_has_no_word_spaces() {
        let c = chars("用中文写作");
        assert_eq!(toggle_emphasis(&c, 1..3, "**").1, "**中文**");
    }

    #[test]
    fn a_heading_replaces_whatever_level_was_there() {
        assert_eq!(toggle_heading(&chars("Title"), 1), "# Title");
        assert_eq!(toggle_heading(&chars("## Title"), 1), "# Title");
        assert_eq!(toggle_heading(&chars("# Title"), 3), "### Title");
    }

    #[test]
    fn the_same_level_twice_makes_it_prose_again() {
        assert_eq!(toggle_heading(&chars("## Title"), 2), "Title");
    }

    #[test]
    fn a_heading_is_not_indented() {
        // Spaces in front of the hashes stop it being a heading at all.
        assert_eq!(toggle_heading(&chars("  Title"), 2), "## Title");
    }

    #[test]
    fn seven_hashes_were_never_a_heading_so_the_line_keeps_them() {
        assert_eq!(toggle_heading(&chars("####### x"), 1), "# ####### x");
    }
}

#[cfg(test)]
mod continuation {
    use super::*;

    fn at(line: &str) -> Continue {
        continues(&line.chars().collect::<Vec<_>>())
    }

    fn marker(line: &str) -> String {
        match at(line) {
            Continue::Marker(m) => m,
            other => panic!("{line:?} gave {other:?}"),
        }
    }

    #[test]
    fn a_bullet_carries_on() {
        assert_eq!(marker("- first"), "- ");
        assert_eq!(marker("* first"), "* ");
        assert_eq!(marker("+ first"), "+ ");
    }

    #[test]
    fn an_ordered_list_counts_on_rather_than_repeating() {
        // Renumbering by hand is exactly what the marker exists to avoid.
        assert_eq!(marker("1. first"), "2. ");
        assert_eq!(marker("9. ninth"), "10. ");
        // And it keeps the delimiter the writer chose.
        assert_eq!(marker("3) third"), "4) ");
    }

    #[test]
    fn a_nested_item_stays_nested() {
        assert_eq!(marker("  - nested"), "  - ");
        assert_eq!(marker("  2. nested"), "  3. ");
    }

    #[test]
    fn a_quote_carries_on_too() {
        assert_eq!(marker("> quoted"), "> ");
    }

    #[test]
    fn an_empty_item_ends_the_list_instead_of_adding_another() {
        // Enter twice is how every editor leaves a list. Without this the
        // writer is trapped adding empty bullets.
        assert_eq!(at("- "), Continue::End(2));
        assert_eq!(at("  1. "), Continue::End(5));
        assert_eq!(at("> "), Continue::End(2));
    }

    #[test]
    fn prose_and_headings_just_break() {
        assert_eq!(at("ordinary prose"), Continue::Break);
        assert_eq!(at(""), Continue::Break);
        // A heading is followed by prose, never by another heading.
        assert_eq!(at("# Title"), Continue::Break);
        assert_eq!(at("### Section"), Continue::Break);
    }

    #[test]
    fn a_hyphen_that_is_not_a_bullet_is_not_continued() {
        // The space is what makes it a marker — `-well-known` is a word.
        assert_eq!(at("-not a bullet"), Continue::Break);
        assert_eq!(at("*emphasis* here"), Continue::Break);
    }

    #[test]
    fn a_cjk_item_continues_like_any_other() {
        assert_eq!(marker("- 中文条目"), "- ");
        assert_eq!(marker("1. 日本語"), "2. ");
    }
}

#[cfg(test)]
mod marker_tests {
    use super::*;

    fn marker_of(text: &str) -> usize {
        let chars: Vec<char> = text.chars().collect();
        analyze(&chars)[0].marker()
    }

    #[test]
    fn a_marker_is_its_punctuation_its_indent_and_the_space_after_it() {
        assert_eq!(marker_of("# Title"), 2);
        assert_eq!(marker_of("###### Deep"), 7);
        assert_eq!(marker_of("> Quoted"), 2);
        assert_eq!(marker_of("- Bullet"), 2);
        assert_eq!(marker_of("1. First"), 3);
        assert_eq!(marker_of("- [x] Done"), 6);
        assert_eq!(marker_of("  - Nested"), 4);
    }

    /// **A block that is syntax end to end has no marker.** A fence and a rule
    /// are one leading syntax span covering the whole line, which is the shape
    /// a marker has and not the thing a marker is.
    #[test]
    fn a_block_without_one_answers_nothing() {
        assert_eq!(marker_of("ordinary prose"), 0);
        assert_eq!(marker_of("```"), 0);
        assert_eq!(marker_of("---"), 0);
        assert_eq!(marker_of(""), 0);
        assert_eq!(marker_of("#no space"), 0);
    }
}

/// Recognise a block marker at the very start of a line, returning how many
/// characters it occupies.
fn leading_marker(rest: &[char]) -> Option<(Block, usize)> {
    // Headings: one to six hashes, then a space.
    let hashes = rest.iter().take_while(|c| **c == '#').count();
    if (1..=6).contains(&hashes) && rest.get(hashes) == Some(&' ') {
        return Some((Block::Heading(hashes as u8), hashes + 1));
    }

    // Block quote. The space after `>` is optional.
    if rest.first() == Some(&'>') {
        let n = if rest.get(1) == Some(&' ') { 2 } else { 1 };
        return Some((Block::Quote, n));
    }

    // Bullet list. The space is required, so `*emphasis*` is not a bullet.
    if matches!(rest.first(), Some('-' | '*' | '+')) && rest.get(1) == Some(&' ') {
        // A box after the bullet makes it a task, and the box is part of the
        // marker: `[x]` is punctuation the writer typed, not prose.
        if let Some(done) = box_at(&rest[2..]) {
            return Some((Block::Task { done }, 2 + BOX));
        }
        return Some((Block::ListItem { ordered: false }, 2));
    }

    // Ordered list: digits, then `.` or `)`, then a space.
    let digits = rest.iter().take_while(|c| c.is_ascii_digit()).count();
    if digits > 0
        && matches!(rest.get(digits), Some('.' | ')'))
        && rest.get(digits + 1) == Some(&' ')
    {
        return Some((Block::ListItem { ordered: true }, digits + 2));
    }

    None
}

/// How many characters `[ ] ` takes, box and trailing space.
const BOX: usize = 4;

/// A task box at the head of `rest`, and whether it is ticked.
///
/// `x` or `X`; anything else between the brackets is not a box, so `[1]` after
/// a bullet stays prose.
fn box_at(rest: &[char]) -> Option<bool> {
    if rest.first() != Some(&'[') || rest.get(2) != Some(&']') || rest.get(3) != Some(&' ') {
        return None;
    }
    match rest.get(1) {
        Some(' ') => Some(false),
        Some('x' | 'X') => Some(true),
        _ => None,
    }
}

/// Where a line's task box is, as an offset into it, and whether it is ticked.
///
/// The offset is of the character *between* the brackets, which is the one a
/// tick replaces. `None` for any line that is not a task.
pub fn task_box(line: &[char]) -> Option<(usize, bool)> {
    let indent = line.iter().take_while(|c| **c == ' ').count().min(3);
    let rest = &line[indent..];
    if !matches!(rest.first(), Some('-' | '*' | '+')) || rest.get(1) != Some(&' ') {
        return None;
    }
    box_at(&rest[2..]).map(|done| (indent + 3, done))
}

fn is_fence(line: &[char]) -> bool {
    let t: Vec<char> = line.iter().copied().skip_while(|c| *c == ' ').collect();
    (t.starts_with(&['`', '`', '`']) && !t[3..].contains(&'`')) || t.starts_with(&['~', '~', '~'])
}

fn is_rule(line: &[char]) -> bool {
    let t: Vec<char> = line.iter().copied().filter(|c| *c != ' ').collect();
    t.len() >= 3
        && (t.iter().all(|c| *c == '-')
            || t.iter().all(|c| *c == '*')
            || t.iter().all(|c| *c == '_'))
}

fn one_span(range: Range<usize>, style: Style) -> Vec<Span> {
    if range.is_empty() {
        Vec::new()
    } else {
        vec![Span { range, style }]
    }
}

fn all_syntax(range: Range<usize>, block: Block) -> LineMarkup {
    let spans = one_span(range.clone(), Style::Syntax);
    LineMarkup {
        range,
        block,
        spans,
        highlights: Vec::new(),
    }
}

/// Label the inline markup inside `range`, which must lie on one line.
///
/// `highlights` collects the `==…==` fields found anywhere inside `range`,
/// including inside emphasis, which is why it is threaded through the recursion
/// rather than returned: the ranges have to reach the line whole, and a field
/// found inside `**…**` belongs to the same line as one found beside it.
fn inline(chars: &[char], range: Range<usize>, highlights: &mut Vec<Range<usize>>) -> Vec<Span> {
    let mut spans: Vec<Span> = Vec::new();
    let mut text_from = range.start;
    let mut i = range.start;

    // Close off the prose run that ended just before `at`.
    fn flush(spans: &mut Vec<Span>, from: usize, at: usize) {
        if at > from {
            spans.push(Span {
                range: from..at,
                style: Style::Text,
            });
        }
    }

    while i < range.end {
        let c = chars[i];

        // Code is literal: nothing inside it is markup, so it is checked first.
        if c == '`'
            && let Some(close) = find(chars, i + 1, range.end, '`')
        {
            flush(&mut spans, text_from, i);
            spans.push(Span {
                range: i..i + 1,
                style: Style::Syntax,
            });
            if close > i + 1 {
                spans.push(Span {
                    range: i + 1..close,
                    style: Style::Code,
                });
            }
            spans.push(Span {
                range: close..close + 1,
                style: Style::Syntax,
            });
            i = close + 1;
            text_from = i;
            continue;
        }

        // Struck-out text, and **flat inside, like code**: the rule is drawn
        // over whatever face the characters are set in, so a bold word inside
        // it would have nowhere to say it was both.
        if c == '~'
            && chars.get(i + 1) == Some(&'~')
            && let Some(close) = find_run(chars, i + 2, range.end, '~', 2)
        {
            flush(&mut spans, text_from, i);
            spans.push(Span {
                range: i..i + 2,
                style: Style::Syntax,
            });
            if close > i + 2 {
                spans.push(Span {
                    range: i + 2..close,
                    style: Style::Strikethrough,
                });
            }
            spans.push(Span {
                range: close..close + 2,
                style: Style::Syntax,
            });
            i = close + 2;
            text_from = i;
            continue;
        }

        // Highlighted text. The markers stay syntax and the body is parsed like
        // any other, because the field this records is drawn *behind* the run
        // and leaves the faces alone — so unlike `~~`, emphasis inside it still
        // works.
        if c == '='
            && chars.get(i + 1) == Some(&'=')
            && let Some(close) = find_run(chars, i + 2, range.end, '=', 2)
        {
            flush(&mut spans, text_from, i);
            spans.push(Span {
                range: i..i + 2,
                style: Style::Syntax,
            });
            spans.extend(inline(chars, i + 2..close, highlights));
            spans.push(Span {
                range: close..close + 2,
                style: Style::Syntax,
            });
            // Pushed after the recursion so that a field nested inside this one
            // — `==a ==b== c==` closes at the first `==`, so this cannot
            // actually nest, but the body may still contain one after an
            // emphasis run — stays in document order.
            highlights.push(i..close + 2);
            highlights.sort_by_key(|h| h.start);
            i = close + 2;
            text_from = i;
            continue;
        }

        // Strong before emphasis: `**` must not be read as two `*`.
        if let Some((marker, style)) = emphasis_marker(chars, i, range.end)
            && let Some(close) = find_run(chars, i + marker, range.end, c, marker)
        {
            flush(&mut spans, text_from, i);
            spans.push(Span {
                range: i..i + marker,
                style: Style::Syntax,
            });
            // **The body is parsed, not taken flat.** Pushed as one span of the
            // outer style, the `*` inside `**a *b* c**` are bold text rather
            // than markers and nothing between them is italic. The recursion is
            // bounded: the inner range is strictly smaller at both ends.
            for span in inline(chars, i + marker..close, highlights) {
                spans.push(Span {
                    style: nested(style, span.style),
                    ..span
                });
            }
            spans.push(Span {
                range: close..close + marker,
                style: Style::Syntax,
            });
            i = close + marker;
            text_from = i;
            continue;
        }

        if c == '['
            && let Some(link) = link_at(chars, i, range.end)
        {
            flush(&mut spans, text_from, i);
            spans.extend(link.spans);
            i = link.end;
            text_from = i;
            continue;
        }

        i += 1;
    }

    flush(&mut spans, text_from, range.end);
    spans
}

/// What a run inside emphasis ends up as, given the emphasis around it.
///
/// `outer` is only ever `Emphasis` or `Strong` — the two things that can be
/// nested into — so anything emphatic found inside is the other one, and the
/// pair is both.
///
/// **Only prose and emphasis combine.** A marker inside bold is still a marker
/// and still drawn quiet; code inside bold is still code, which on this device
/// means the body face, because there is no monospace to embolden.
fn nested(outer: Style, inner: Style) -> Style {
    match inner {
        Style::Text => outer,
        Style::Emphasis | Style::Strong | Style::StrongEmphasis => Style::StrongEmphasis,
        other => other,
    }
}

/// An emphasis opener at `i`: how many marker characters, and what it means.
///
/// An opener must be followed by something other than a space, so that a lone
/// `*` in prose or a `-` used as a dash does not open emphasis that never
/// closes.
fn emphasis_marker(chars: &[char], i: usize, end: usize) -> Option<(usize, Style)> {
    let c = chars[i];
    if c != '*' && c != '_' {
        return None;
    }
    let run = (i..end).take_while(|&j| chars[j] == c).count();
    let marker = if run >= 2 { 2 } else { 1 };
    let style = if marker == 2 {
        Style::Strong
    } else {
        Style::Emphasis
    };
    // Bounded by `end`, not just by the buffer. Nested calls pass a range that
    // stops before the outer closing run, and reading past it would let the
    // outer `**` of `**a***` look like the start of something.
    match chars.get(i + marker).filter(|_| i + marker < end) {
        Some(next) if !next.is_whitespace() => Some((marker, style)),
        _ => None,
    }
}

fn find(chars: &[char], from: usize, end: usize, target: char) -> Option<usize> {
    (from..end).find(|&i| chars[i] == target)
}

/// Find the closing run for an opener of `len` copies of `target`.
///
/// **A run of exactly `len` wins, wherever it is.** A longer one closes only if
/// there is no exact match at all, and that ordering is what makes nesting
/// work: in `*a **b** c*` the `**` in the middle is the inner pair's, and taking
/// the first run that was merely long enough closed the outer emphasis on it —
/// giving `a **b` in italics and the rest as prose. The looser rule is kept as
/// the fallback because `**bold***` should still be bold followed by a stray
/// asterisk, which is what every other renderer does with it.
fn find_run(chars: &[char], from: usize, end: usize, target: char, len: usize) -> Option<usize> {
    let mut longer = None;
    let mut i = from;
    while i < end {
        if chars[i] != target {
            i += 1;
            continue;
        }
        let run = (i..end).take_while(|&j| chars[j] == target).count();
        // A closer must be at least as long as the opener, and must not be
        // preceded by a space — `a * b * c` is not emphasis.
        let closes = run >= len
            && chars
                .get(i.wrapping_sub(1))
                .is_some_and(|p| !p.is_whitespace());
        if closes {
            if run == len {
                return Some(i);
            }
            longer.get_or_insert(i);
        }
        i += run;
    }
    longer
}

struct Link {
    spans: Vec<Span>,
    end: usize,
}

/// Match `[text](url)` starting at `i`.
fn link_at(chars: &[char], i: usize, end: usize) -> Option<Link> {
    let close = find(chars, i + 1, end, ']')?;
    if chars.get(close + 1) != Some(&'(') {
        return None;
    }
    let paren = find(chars, close + 2, end, ')')?;

    let mut spans = vec![Span {
        range: i..i + 1,
        style: Style::Syntax,
    }];
    if close > i + 1 {
        spans.push(Span {
            range: i + 1..close,
            style: Style::Link,
        });
    }
    spans.push(Span {
        range: close..close + 2,
        style: Style::Syntax,
    });
    if paren > close + 2 {
        spans.push(Span {
            range: close + 2..paren,
            style: Style::Url,
        });
    }
    spans.push(Span {
        range: paren..paren + 1,
        style: Style::Syntax,
    });
    Some(Link {
        spans,
        end: paren + 1,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chars(s: &str) -> Vec<char> {
        s.chars().collect()
    }

    /// Render as `text/STYLE` pairs, so a test reads like what it checks.
    fn labelled(s: &str) -> Vec<(String, Style)> {
        let cs = chars(s);
        analyze(&cs)
            .into_iter()
            .flat_map(|l| l.spans)
            .map(|sp| (cs[sp.range].iter().collect::<String>(), sp.style))
            .collect()
    }

    fn blocks(s: &str) -> Vec<Block> {
        analyze(&chars(s)).into_iter().map(|l| l.block).collect()
    }

    /// The highlighted runs of a one-line document, marker to marker.
    fn highlighted(s: &str) -> Vec<String> {
        let cs = chars(s);
        analyze(&cs)
            .into_iter()
            .flat_map(|l| l.highlights)
            .map(|h| cs[h].iter().collect::<String>())
            .collect()
    }

    /// The invariant the renderer depends on: spans tile their line exactly.
    fn assert_tiles(s: &str) {
        let cs = chars(s);
        for line in analyze(&cs) {
            let mut at = line.range.start;
            for sp in &line.spans {
                assert_eq!(sp.range.start, at, "gap or overlap in {line:?}");
                at = sp.range.end;
            }
            assert_eq!(at, line.range.end, "spans do not reach the end of {line:?}");
        }
    }

    #[test]
    fn plain_prose_is_one_text_span() {
        assert_eq!(labelled("just words"), [("just words".into(), Style::Text)]);
    }

    #[test]
    fn heading_keeps_its_hashes_as_syntax() {
        assert_eq!(
            labelled("## Chapter one"),
            [
                ("## ".into(), Style::Syntax),
                ("Chapter one".into(), Style::Text)
            ]
        );
        assert_eq!(blocks("## Chapter one"), [Block::Heading(2)]);
    }

    #[test]
    fn heading_levels_and_non_headings() {
        assert_eq!(blocks("# a"), [Block::Heading(1)]);
        assert_eq!(blocks("###### f"), [Block::Heading(6)]);
        // Seven hashes is not a heading, and a hash with no space is not either.
        assert_eq!(blocks("####### g"), [Block::Paragraph]);
        assert_eq!(blocks("#nohash"), [Block::Paragraph]);
    }

    #[test]
    fn a_highlight_keeps_its_markers_as_syntax_and_its_body_as_prose() {
        assert_eq!(
            labelled("the ==highlighted== word"),
            [
                ("the ".into(), Style::Text),
                ("==".into(), Style::Syntax),
                ("highlighted".into(), Style::Text),
                ("==".into(), Style::Syntax),
                (" word".into(), Style::Text),
            ]
        );
    }

    #[test]
    fn the_field_covers_the_markers_too() {
        // What the renderer fills: the markers sit inside the field, not
        // beside it.
        assert_eq!(highlighted("the ==highlighted== word"), ["==highlighted=="]);
    }

    #[test]
    fn a_line_can_carry_more_than_one_field_and_they_come_in_order() {
        assert_eq!(highlighted("==one== and ==two=="), ["==one==", "==two=="]);
    }

    #[test]
    fn emphasis_inside_a_highlight_survives_it() {
        // The whole reason the field is carried beside the spans rather than as
        // one of them: `~~` cannot do this and does not claim to.
        assert_eq!(
            labelled("==a **b** c=="),
            [
                ("==".into(), Style::Syntax),
                ("a ".into(), Style::Text),
                ("**".into(), Style::Syntax),
                ("b".into(), Style::Strong),
                ("**".into(), Style::Syntax),
                (" c".into(), Style::Text),
                ("==".into(), Style::Syntax),
            ]
        );
        assert_eq!(highlighted("==a **b** c=="), ["==a **b** c=="]);
    }

    #[test]
    fn a_highlight_inside_emphasis_reaches_the_line() {
        // Threaded through the recursion, which is what the out-parameter buys.
        assert_eq!(
            highlighted("**bold ==and marked== here**"),
            ["==and marked=="]
        );
    }

    #[test]
    fn a_closing_marker_has_to_follow_a_word() {
        // The same rule emphasis has, and it matters more here: `== a == b ==`
        // in prose about arithmetic should not paint half the line yellow.
        assert!(highlighted("==open ==").is_empty());
        assert_eq!(highlighted("==closed=="), ["==closed=="]);
    }

    #[test]
    fn a_lone_double_equals_is_just_characters() {
        assert_eq!(labelled("a == b"), [("a == b".into(), Style::Text)]);
        assert!(highlighted("a == b").is_empty());
    }

    #[test]
    fn a_bare_run_of_equals_closes_on_itself_and_marks_nothing() {
        // `is_rule` knows -, * and _ but not =, so a line of them reaches the
        // inline parser and `====` is an opener that finds its closer with no
        // body between. An empty field draws as its own two markers, which is
        // what the characters are. Recorded because setext underlining looks
        // like this and karyll does not read setext.
        assert_eq!(highlighted("===="), ["===="]);
        assert_eq!(
            labelled("===="),
            [("==".into(), Style::Syntax), ("==".into(), Style::Syntax)]
        );
        assert_tiles("====");
        // Three is not two closers, so nothing opens and it stays prose.
        assert!(highlighted("===").is_empty());
    }

    #[test]
    fn highlights_tile_and_nest_without_breaking_the_span_invariant() {
        assert_tiles("the ==highlighted== word");
        assert_tiles("==a **b** c==");
        assert_tiles("**bold ==and marked== here**");
        assert_tiles("# a ==heading== too");
        assert_tiles("- [ ] a ==task== item");
    }

    #[test]
    fn emphasis_and_strong() {
        assert_eq!(
            labelled("a *b* c"),
            [
                ("a ".into(), Style::Text),
                ("*".into(), Style::Syntax),
                ("b".into(), Style::Emphasis),
                ("*".into(), Style::Syntax),
                (" c".into(), Style::Text),
            ]
        );
        assert_eq!(
            labelled("**bold**"),
            [
                ("**".into(), Style::Syntax),
                ("bold".into(), Style::Strong),
                ("**".into(), Style::Syntax),
            ]
        );
    }

    /// Taken flat, the body of an emphasis run leaves the inner `*` as bold
    /// *text* rather than markers, with nothing between them italic.
    #[test]
    fn emphasis_nests_inside_strong() {
        assert_eq!(
            labelled("**a *b* c**"),
            [
                ("**".into(), Style::Syntax),
                ("a ".into(), Style::Strong),
                ("*".into(), Style::Syntax),
                ("b".into(), Style::StrongEmphasis),
                ("*".into(), Style::Syntax),
                (" c".into(), Style::Strong),
                ("**".into(), Style::Syntax),
            ]
        );
    }

    #[test]
    fn strong_nests_inside_emphasis_the_same_way() {
        assert_eq!(
            labelled("*a **b** c*"),
            [
                ("*".into(), Style::Syntax),
                ("a ".into(), Style::Emphasis),
                ("**".into(), Style::Syntax),
                ("b".into(), Style::StrongEmphasis),
                ("**".into(), Style::Syntax),
                (" c".into(), Style::Emphasis),
                ("*".into(), Style::Syntax),
            ]
        );
    }

    /// Only prose and emphasis combine. Everything else inside keeps its own
    /// reading, because there is no bold monospace on this device and a link
    /// target is quiet wherever it appears.
    #[test]
    fn code_and_links_inside_strong_stay_themselves() {
        assert_eq!(
            labelled("**a `b` c**"),
            [
                ("**".into(), Style::Syntax),
                ("a ".into(), Style::Strong),
                ("`".into(), Style::Syntax),
                ("b".into(), Style::Code),
                ("`".into(), Style::Syntax),
                (" c".into(), Style::Strong),
                ("**".into(), Style::Syntax),
            ]
        );
        let styles: Vec<Style> = labelled("**see [it](u)**")
            .into_iter()
            .map(|(_, style)| style)
            .collect();
        assert!(styles.contains(&Style::Link), "the text is still a link");
        assert!(styles.contains(&Style::Url), "and the target still quiet");
    }

    /// The reason a longer closing run is still accepted when there is no exact
    /// one. Every other renderer reads this as bold followed by a stray
    /// asterisk, and requiring an exact match would have made the whole thing
    /// prose.
    #[test]
    fn a_trailing_extra_marker_still_closes() {
        assert_eq!(
            labelled("**bold***"),
            [
                ("**".into(), Style::Syntax),
                ("bold".into(), Style::Strong),
                ("**".into(), Style::Syntax),
                ("*".into(), Style::Text),
            ]
        );
    }

    /// The nesting recurses, so a run that opens and never closes must not be
    /// able to walk off its own range.
    #[test]
    fn an_unclosed_marker_inside_strong_is_just_text() {
        assert_eq!(
            labelled("**a *b**"),
            [
                ("**".into(), Style::Syntax),
                ("a *b".into(), Style::Strong),
                ("**".into(), Style::Syntax),
            ]
        );
        // And the degenerate ones terminate at all, which is the property the
        // recursion has to have.
        for source in ["**", "****", "***", "*****", "**a***", "*_*_*"] {
            let chars: Vec<char> = source.chars().collect();
            let spans = analyze(&chars)[0].spans.clone();
            let rebuilt: String = spans
                .iter()
                .flat_map(|s| chars[s.range.clone()].iter())
                .collect();
            assert_eq!(rebuilt, source, "spans must still tile {source:?}");
        }
    }

    #[test]
    fn underscores_emphasise_too() {
        assert_eq!(
            labelled("_it_"),
            [
                ("_".into(), Style::Syntax),
                ("it".into(), Style::Emphasis),
                ("_".into(), Style::Syntax),
            ]
        );
    }

    #[test]
    fn an_unclosed_marker_stays_prose() {
        assert_eq!(labelled("2 * 3 = 6"), [("2 * 3 = 6".into(), Style::Text)]);
        assert_eq!(
            labelled("a *dangling"),
            [("a *dangling".into(), Style::Text)]
        );
    }

    #[test]
    fn code_spans_are_literal_inside() {
        assert_eq!(
            labelled("use `a *b* c` here"),
            [
                ("use ".into(), Style::Text),
                ("`".into(), Style::Syntax),
                ("a *b* c".into(), Style::Code),
                ("`".into(), Style::Syntax),
                (" here".into(), Style::Text),
            ]
        );
    }

    #[test]
    fn links_split_into_text_and_url() {
        assert_eq!(
            labelled("see [docs](http://x.dev) now"),
            [
                ("see ".into(), Style::Text),
                ("[".into(), Style::Syntax),
                ("docs".into(), Style::Link),
                ("](".into(), Style::Syntax),
                ("http://x.dev".into(), Style::Url),
                (")".into(), Style::Syntax),
                (" now".into(), Style::Text),
            ]
        );
    }

    #[test]
    fn brackets_that_are_not_links_stay_prose() {
        assert_eq!(
            labelled("[just brackets]"),
            [("[just brackets]".into(), Style::Text)]
        );
    }

    #[test]
    fn lists_and_quotes() {
        assert_eq!(blocks("- one"), [Block::ListItem { ordered: false }]);
        assert_eq!(blocks("3. three"), [Block::ListItem { ordered: true }]);
        assert_eq!(blocks("1) one"), [Block::ListItem { ordered: true }]);
        assert_eq!(blocks("> quoted"), [Block::Quote]);
        // A bullet needs its space, so emphasis at line start is not a bullet.
        assert_eq!(blocks("*emphasis* here"), [Block::Paragraph]);
    }

    #[test]
    fn fenced_code_suspends_markup_until_it_closes() {
        let src = "before\n```\n# not a heading\n```\nafter";
        assert_eq!(
            blocks(src),
            [
                Block::Paragraph,
                Block::Fence,
                Block::Code,
                Block::Fence,
                Block::Paragraph
            ]
        );
        let inside = analyze(&chars(src))[2].clone();
        assert_eq!(inside.spans.len(), 1);
        assert_eq!(inside.spans[0].style, Style::Code);
    }

    #[test]
    fn rules_and_blank_lines() {
        assert_eq!(blocks("---"), [Block::Rule]);
        assert_eq!(blocks("***"), [Block::Rule]);
        assert_eq!(blocks(""), [Block::Blank]);
        assert_eq!(
            blocks("a\n\nb"),
            [Block::Paragraph, Block::Blank, Block::Paragraph]
        );
    }

    #[test]
    fn markup_works_on_chinese_prose() {
        assert_eq!(
            labelled("## 第一章"),
            [
                ("## ".into(), Style::Syntax),
                ("第一章".into(), Style::Text)
            ]
        );
        assert_eq!(
            labelled("他说**你好**世界"),
            [
                ("他说".into(), Style::Text),
                ("**".into(), Style::Syntax),
                ("你好".into(), Style::Strong),
                ("**".into(), Style::Syntax),
                ("世界".into(), Style::Text),
            ]
        );
    }

    #[test]
    fn spans_always_tile_their_line() {
        for src in [
            "plain",
            "## heading",
            "a *b* **c** `d` [e](f) g",
            "- item with *emphasis*",
            "> quote with `code`",
            "```\ncode\n```",
            "---",
            "",
            "他说「你好，世界」**再见**。",
            "trailing *unclosed",
            "[link](url)",
        ] {
            assert_tiles(src);
        }
    }

    #[test]
    fn line_ranges_cover_the_document() {
        let src = "one\ntwo\n\nfour";
        let cs = chars(src);
        let lines = analyze(&cs);
        assert_eq!(lines.len(), 4);
        assert_eq!(lines[0].range, 0..3);
        assert_eq!(lines[2].range, 8..8);
        assert_eq!(lines[3].range, 9..13);
    }

    #[test]
    fn struck_out_text_is_its_own_style() {
        assert_eq!(
            labelled("keep ~~cut this~~ keep"),
            [
                ("keep ", Style::Text),
                ("~~", Style::Syntax),
                ("cut this", Style::Strikethrough),
                ("~~", Style::Syntax),
                (" keep", Style::Text),
            ]
            .map(|(s, k)| (s.to_string(), k))
        );
    }

    /// Flat inside: one span cannot carry both a face and a rule.
    #[test]
    fn emphasis_inside_a_strike_stays_struck() {
        assert_eq!(
            labelled("~~a *b* c~~"),
            [
                ("~~", Style::Syntax),
                ("a *b* c", Style::Strikethrough),
                ("~~", Style::Syntax),
            ]
            .map(|(s, k)| (s.to_string(), k))
        );
    }

    #[test]
    fn a_lone_tilde_is_prose() {
        assert_eq!(
            labelled("~one~ and ~~unclosed"),
            [("~one~ and ~~unclosed".to_string(), Style::Text)]
        );
    }

    #[test]
    fn a_bullet_with_a_box_is_a_task() {
        assert_eq!(
            blocks("- [ ] to do\n- [x] done\n- [X] done too\n- ordinary"),
            [
                Block::Task { done: false },
                Block::Task { done: true },
                Block::Task { done: true },
                Block::ListItem { ordered: false },
            ]
        );
        // The box has to be a box. Anything else after the bullet is prose,
        // brackets included.
        assert_eq!(
            blocks("- [1] a citation"),
            [Block::ListItem { ordered: false }]
        );
        assert_eq!(blocks("- [] no room"), [Block::ListItem { ordered: false }]);
    }

    /// The whole marker is punctuation the writer typed, and a done one reads
    /// as struck-out prose.
    #[test]
    fn a_done_task_is_drawn_struck_through() {
        assert_eq!(
            labelled("- [x] posted the letter"),
            [
                ("- [x] ", Style::Syntax),
                ("posted the letter", Style::Strikethrough),
            ]
            .map(|(s, k)| (s.to_string(), k))
        );
        assert_eq!(
            labelled("- [ ] post the letter"),
            [("- [ ] ", Style::Syntax), ("post the letter", Style::Text)]
                .map(|(s, k)| (s.to_string(), k))
        );
    }

    /// Enter carries the list on, never the tick.
    #[test]
    fn a_task_continues_unticked() {
        assert_eq!(
            continues(&chars("- [x] done")),
            Continue::Marker("- [ ] ".into())
        );
        assert_eq!(
            continues(&chars("  * [ ] nested")),
            Continue::Marker("  * [ ] ".into())
        );
        // And an empty one ends the list, like any other empty item.
        assert_eq!(continues(&chars("- [ ] ")), Continue::End(6));
    }

    #[test]
    fn the_box_is_found_where_a_tick_goes() {
        assert_eq!(task_box(&chars("- [ ] a")), Some((3, false)));
        assert_eq!(task_box(&chars("  - [x] a")), Some((5, true)));
        assert_eq!(task_box(&chars("- a")), None);
        assert_eq!(task_box(&chars("plain")), None);
        // The offset names the character a tick replaces.
        let line = chars("- [ ] a");
        let (at, _) = task_box(&line).expect("a task");
        assert_eq!(line[at], ' ');
    }

    /// The line as `plain` reads it, for the outline's column of names.
    fn plainly(src: &str) -> String {
        let cs = chars(src);
        plain(&cs, &analyze(&cs)[0])
    }

    #[test]
    fn a_heading_reads_without_its_hashes() {
        assert_eq!(plainly("## The plan"), "The plan");
        // A hash with no space after it is not a marker at all, so there is
        // nothing to take off — the same reading `analyze` gives it.
        assert_eq!(plainly("#Title"), "#Title");
    }

    #[test]
    fn emphasis_inside_a_heading_keeps_the_words_and_loses_the_stars() {
        assert_eq!(plainly("# The **whole** *point*"), "The whole point");
    }

    #[test]
    fn a_link_in_a_heading_reads_as_its_text() {
        // The URL is dropped with the brackets. A column of section names is
        // not the place to read an address, and it would be the longest thing
        // on the line.
        assert_eq!(
            plainly("### See [the notes](https://example.com/x)"),
            "See the notes"
        );
    }

    #[test]
    fn a_heading_that_is_only_a_marker_reads_as_nothing() {
        // Which is what the outline needs to know: there is no name here to
        // list, so it can say so rather than drawing an empty row.
        assert_eq!(plainly("## "), "");
        assert_eq!(plainly("##   "), "");
    }
}
