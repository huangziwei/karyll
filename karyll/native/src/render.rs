//! Laying a document out and drawing it.
//!
//! Markdown source is shown styled, never previewed: the markers stay on screen
//! and are drawn quiet. Prose is thresholded to one bit because the panel's
//! partial waveform is two-level and an antialiased glyph comes out muddy.
//!
//! Syntax marks are drawn in a **flat grey** ([`crate::window::QUIET`]) rather
//! than black. They were dithered, on the theory that a checkerboard at ~300 ppi
//! would read as grey; the first device screenshot showed that it does not, for
//! the reason recorded on that constant.
//!
//! A selected run inverts — filled black, glyphs drawn white — which is what
//! one bit has instead of a tint.
//!
//! Layout runs per logical line. Each one is wrapped with real advances from
//! the faces that will actually draw it, so the measuring pass and the drawing
//! pass can never disagree about where a character sits.

use anyhow::Result;
use karyll_core::markdown::{Block, LineMarkup, Style};
use karyll_core::script::{Role, role_for, script_of};
use karyll_core::wrap;

use crate::font::{Fonts, Metrics};
use crate::window::{BLACK, Rect, Window};

/// The body sizes on offer, smallest first.
///
/// **A ladder rather than a range**, because every step has to be a size the
/// page still reads well at, and because Config shows every option at once.
/// 46 px is ~11 pt on this 300 ppi panel and the top of the ladder is ~19 pt.
/// Nothing under 10 pt is offered: a 10.2″ page is not a place to set footnote
/// type, and every one of these panels is read closer than the laptop iA
/// Writer is used on.
pub const SIZES: [f32; 7] = [42.0, 46.0, 52.0, 58.0, 64.0, 72.0, 80.0];

/// The size a page opens at.
pub const DEFAULT_SIZE: f32 = 46.0;

/// The line lengths on offer, in characters.
///
/// **The margin is what is left over, which is why there is no margin
/// setting.** iA Writer's model, and it carries between panels in a way a
/// margin in pixels cannot: 55 characters is 55 characters on a 10.2″ Scribe
/// and on a 7″ Colorsoft, where the same 140 px of white is a thirteenth of
/// one page and a ninth of the other.
///
/// **One ladder for both orientations.** A measure belongs to the eye rather
/// than to the panel, so a writer who reads 55 characters comfortably in
/// portrait reads 55 in landscape; the wider surface spends the difference on
/// margin. What the orientation decides is whether the setting can be honoured
/// at all — [`column`] fits it to the surface, and on the narrow edge at the
/// top of [`SIZES`] that cap is what actually sets the line.
///
/// iA Writer offers 64/72/80, which are laptop numbers: 80 characters of 64 px
/// type wants 2400 px and a portrait Scribe is 1860 wide. The bottom of this
/// ladder is what the top of [`SIZES`] needs — 80 px type fills a portrait page
/// at 49 characters, so 40 is the first setting that leaves a margin there —
/// and seven entries because that is what the size row above it has.
pub const LINE_LENGTHS: [u16; 7] = [40, 45, 50, 55, 60, 65, 70];

/// The line length a page opens at, which is what the fixed 1280 px column it
/// replaces held at the default size.
pub const DEFAULT_LINE_LENGTH: u16 = 60;

/// One character of prose as a fraction of the type size, in the face a page
/// opens in.
///
/// iA Writer Duo S is duospace — every glyph 0.46 em except six widened to
/// 0.69 — so an average over a pangram is 0.47 and barely moves with the text.
/// The other two shipped faces are 0.46 (Mono) and 0.42 (Quattro). An eighth
/// between the ends is four characters on a sixty-character line, which is why
/// a page measures the face it is actually set in through
/// [`crate::font::average_advance`] rather than reading this. It is what
/// [`Theme::default`] and the tests set.
pub const DEFAULT_ADVANCE: f32 = 0.47;

/// The least white space either side of the text column.
///
/// Reached when the size and the line length together ask for more than the
/// narrow edge of the panel holds, and a page with no margin at all is one
/// whose descenders touch the bezel. A page sitting on this floor is one whose
/// line is being set by the panel rather than by the writer.
const SIDE_MARGIN: u16 = 70;

/// The ladder entry nearest `px`, so a remembered size from another build lands
/// on something that exists rather than being refused.
pub fn nearest_size(px: f32) -> f32 {
    SIZES
        .into_iter()
        .min_by(|a, b| (a - px).abs().total_cmp(&(b - px).abs()))
        .unwrap_or(DEFAULT_SIZE)
}

/// The ladder entry nearest `chars`, for the same reason [`nearest_size`]
/// exists: a length stored by another build has to land on one that is offered.
pub fn nearest_line_length(chars: u16) -> u16 {
    LINE_LENGTHS
        .into_iter()
        .min_by_key(|offered| offered.abs_diff(chars))
        .unwrap_or(DEFAULT_LINE_LENGTH)
}

/// The next size up or down, or the one given at either end of the ladder.
pub fn step_size(px: f32, larger: bool) -> f32 {
    let at = SIZES
        .iter()
        .position(|s| *s == nearest_size(px))
        .unwrap_or(0);
    let next = if larger { at + 1 } else { at.wrapping_sub(1) };
    SIZES.get(next).copied().unwrap_or(SIZES[at])
}

/// Page geometry and type sizes, in pixels.
pub struct Theme {
    pub body_px: f32,
    /// Width of the text column, before it is fitted to the surface.
    ///
    /// A measure, centred, rather than full-bleed: long lines are harder to
    /// read and there is no reason to use the full 1860 px.
    pub measure: u16,
    /// The line length the measure was set from, in characters.
    pub chars: u16,
    pub margin_y: u16,
    /// Multiplier on the face's own line height.
    pub leading: f32,
    /// Extra space above a heading, as a multiple of the body line height.
    pub heading_space: f32,
}

impl Default for Theme {
    fn default() -> Self {
        Self::at(
            DEFAULT_SIZE,
            DEFAULT_LINE_LENGTH,
            DEFAULT_SIZE * DEFAULT_ADVANCE,
        )
    }
}

impl Theme {
    /// The page set at `body_px`, `chars` to the line.
    ///
    /// **The measure is a line length, and the margin is what the surface has
    /// left over.** The two settings are independent because they answer
    /// different questions — how large the type is, and how much of it goes on
    /// a line — and a column derived from the size alone can only answer the
    /// first. `advance_px` is what one character of prose costs at this size in
    /// the face the body is set in, which [`crate::font::average_advance`]
    /// measures, so the same setting gives the same line in all three shipped
    /// faces and spends the difference between them on white space.
    ///
    /// [`column`] fits the result to the surface. That cap is not a fallback:
    /// on the narrow edge at the top of [`SIZES`] a long line does not fit at
    /// all, and shortening it there is the only thing that leaves a margin to
    /// hold the page by.
    ///
    /// The margin and the leading follow the type. Large type on a tight margin
    /// reads as a page that is too full, and it wants proportionally *less*
    /// leading than small type does.
    pub fn at(body_px: f32, chars: u16, advance_px: f32) -> Self {
        Self {
            body_px,
            measure: (chars as f32 * advance_px) as u16,
            chars,
            margin_y: (160.0 * (body_px / DEFAULT_SIZE)) as u16,
            leading: if body_px >= DEFAULT_SIZE { 1.30 } else { 1.35 },
            heading_space: 0.75,
        }
    }
}

/// The value a glyph is drawn in: three cases, and the awkward one is the pair.
///
/// Inside an inverted run everything is white, quiet marks included. There is no
/// room for a third value on top of an inversion — a recessive mark on a black
/// band reads as damage rather than as a mark — so the selection wins and the
/// syntax mark gives up its greyness for as long as it is selected.
fn ink(inverted: bool, quiet: bool) -> u8 {
    use crate::window::{QUIET, WHITE};
    match (inverted, quiet) {
        (true, _) => WHITE,
        (false, true) => QUIET,
        (false, false) => BLACK,
    }
}

/// Type size for a block. Headings step down towards the body size, so a
/// document of mostly `##` does not waste the page on chrome.
///
/// The steps are tight against a 46 px body: 1.6 / 1.35 / 1.15 would put an `#`
/// at 74 px, a poster on a 1280 px measure. Source shown styled needs the
/// hierarchy legible, not loud — the heading is marked by its `#` as well, so
/// size is not carrying the distinction alone.
pub fn block_px(theme: &Theme, block: Block) -> f32 {
    match block {
        Block::Heading(1) => theme.body_px * 1.45,
        Block::Heading(2) => theme.body_px * 1.25,
        Block::Heading(3) => theme.body_px * 1.10,
        Block::Heading(_) => theme.body_px,
        _ => theme.body_px,
    }
}

/// How wide the caret is drawn, at a given type size.
///
/// Scaled rather than fixed, so it stays the same weight against the text
/// whatever `body_px` becomes. At 46 px that is 6 px — about 0.5 mm on a 300 ppi
/// panel. 2 px is 0.17 mm and reads as a hairline. The floor keeps it visible
/// if the type is ever set very small.
///
/// A bar rather than a block: this editor is always inserting and the caret sits
/// *between* two characters, so a block would sit on top of the one after it.
///
/// **One width, whatever the panel can show.** The bar is drawn on the 300 ppi
/// ink layer and stays crisp; a colour panel resolves its hue at about half
/// that, which is thin enough to be worth knowing about and not thin enough to
/// widen the caret for.
fn caret_width(px: f32) -> u16 {
    (px / 8.0).round().max(3.0) as u16
}

/// The text column on a surface `width` wide: where it starts, and how wide.
///
/// **Fitted to the surface, not taken from the theme.** The measure is a line
/// length in characters at the size the body is set in, so the two settings
/// together can ask for a column wider than a portrait page — unfitted that
/// gives `left = 0` and lines running under both bezels.
///
/// One statement of the geometry, because two things need it: the page draws
/// inside the column, and [`edge_at`] reads the margins either side of it.
pub fn column(theme: &Theme, width: u16) -> (u16, u16) {
    let measure = theme.measure.min(width.saturating_sub(SIDE_MARGIN * 2));
    (width.saturating_sub(measure) / 2, measure)
}

/// Which edge of the page a tap fell on.
///
/// **The margins are the page's own controls**, which is what makes a document
/// readable with nothing paired: a tap places the cursor, a drag selects, and
/// neither moves the page — so without these there is one way through a long
/// draft and it is the keyboard.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Edge {
    /// The left margin: back a screen.
    Back,
    /// The right margin: on a screen. The way round a Kindle already reads.
    On,
    /// The band along the top: the start of the document.
    Start,
    /// The band above the strip: the end of it.
    End,
}

/// How much of an edge answers a tap.
///
/// The strip's own height, which is the target size this app already settled on
/// for a finger: 120 px is 10 mm on a 300 ppi panel.
const EDGE: u16 = 120;

/// Which edge a tap at `(x, y)` fell on, or `None` for the page itself.
///
/// **The sides run the full height and the bands sit between them**, so a
/// corner turns a page rather than jumping to an end — the corner is where a
/// thumb strays, and a page turn is the cheaper mistake.
///
/// The sides are the real margins, widened to [`EDGE`] when the type is large
/// enough to leave less. The cost of that widening is a character or two at
/// each end of a line that a tap can no longer put the cursor in; the cost of
/// not widening it is a control too narrow to hit.
pub fn edge_at(theme: &Theme, width: u16, bottom: u16, x: u16, y: u16) -> Option<Edge> {
    let (left, _) = column(theme, width);
    let side = left.max(EDGE);
    if x < side {
        return Some(Edge::Back);
    }
    if x >= width.saturating_sub(side) {
        return Some(Edge::On);
    }
    if y < EDGE {
        return Some(Edge::Start);
    }
    if y >= bottom.saturating_sub(EDGE) {
        return Some(Edge::End);
    }
    None
}

/// Left inset for a block, so lists and quotes hang.
pub fn block_indent(theme: &Theme, block: Block) -> u16 {
    match block {
        Block::Quote | Block::ListItem { .. } | Block::Task { .. } => (theme.body_px * 0.9) as u16,
        _ => 0,
    }
}

/// Everything a layout or paint needs about the document and the page, so the
/// functions below take one bundle instead of the same five arguments each.
pub struct Page<'a> {
    pub chars: &'a [char],
    pub markup: &'a [LineMarkup],
    pub theme: &'a Theme,
    /// The text column actually used: the theme's measure, capped to what the
    /// surface can hold. **Wrapping asks here, not the theme**, or the largest
    /// type sizes would wrap to a column wider than the page they are drawn on.
    pub measure: u16,
    /// Left edge of the text column, centring the measure on the surface.
    pub left: u16,
    /// Where the page ends. The action strip lives below this, and text drawn
    /// under it would be both invisible and untappable.
    pub bottom: u16,
    /// Every face this document draws from.
    ///
    /// Rows are one height across the page — a paragraph whose lines were
    /// different heights because one of them happened to contain a kanji would
    /// be worse than a slightly taller row — so the box is sized once, from
    /// everything in the document rather than from what is on any one line.
    /// Scanning for it is pure and costs one pass; the alternative is asking the
    /// faces, and that is what would load 10 MB of Han for an English draft.
    pub roles: Vec<Role>,
    /// The one sentence drawn solid while the rest of the page is set back.
    ///
    /// `None` is focus mode switched off, and leaves every character solid.
    pub focus: Option<std::ops::Range<usize>>,
    /// Text being composed by the IME, drawn underlined because it is not yet
    /// part of the document.
    pub underline: Option<std::ops::Range<usize>>,
}

impl<'a> Page<'a> {
    pub fn new(
        chars: &'a [char],
        markup: &'a [LineMarkup],
        theme: &'a Theme,
        surface: (u16, u16),
        bottom: u16,
    ) -> Self {
        let (left, measure) = column(theme, surface.0);
        Self {
            chars,
            markup,
            theme,
            measure,
            left,
            bottom,
            roles: roles_in(chars, markup),
            focus: None,
            underline: None,
        }
    }

    /// Set everything back but `span`. `None` switches focus mode off.
    pub fn focused_on(mut self, span: Option<std::ops::Range<usize>>) -> Self {
        self.focus = span;
        self
    }

    /// Mark `span` as text the IME is still composing.
    pub fn composing(mut self, span: Option<std::ops::Range<usize>>) -> Self {
        self.underline = span;
        self
    }

    /// Left edge of a block's text, past any hanging indent.
    fn origin(&self, block: Block) -> f32 {
        (self.left + block_indent(self.theme, block)) as f32
    }
}

/// One line as it will be drawn.
pub struct VisualLine {
    /// Character range into the document.
    pub range: std::ops::Range<usize>,
    /// Index of the logical line this came from, so drawing can reach its
    /// styles without searching for the entry that contains it.
    pub markup: usize,
    pub block: Block,
    /// Top edge, in window coordinates.
    pub y: i32,
    pub px: f32,
    /// How tall the row is. Damage and clipping ask the layout for this rather
    /// than deriving it from `px`, so the rectangle that repaints a row is the
    /// same box the row was laid out in.
    pub height: i32,
    /// The leading, split — half above the glyph box and half below.
    ///
    /// Leading is extra space around a line, and putting all of it under the
    /// text pushes every glyph to the top of its row. Latin hides that, because
    /// a Latin face's ascent has air above the cap height and the letters end up
    /// near the middle anyway; Han fills its em box to the top and goes exactly
    /// where the arithmetic puts it. Splitting it is what centres the text in
    /// its row, and what makes the caret sit around the glyphs rather than
    /// hanging below them.
    pub inset: i32,
    /// Offset from [`VisualLine::y`] to the baseline, [`VisualLine::inset`]
    /// included. Layout works this out and drawing uses it, so the two cannot
    /// disagree about where the text sits.
    pub baseline: i32,
    /// How much of this row is in focus, **relative to its own start**.
    ///
    /// Relative rather than absolute so that inserting a character earlier in
    /// the document does not make every row below it look changed. Focus mode
    /// off is the whole row, so nothing is set back and the comparison below
    /// stays stable.
    ///
    /// This is on the line, rather than read from the page while drawing,
    /// because [`Frame::unchanged`] has to see it. A cursor moving to the next
    /// sentence changes no text, no size and no position — so a row whose ink
    /// is about to change would otherwise count as unchanged and never be
    /// repainted, leaving the dimming a sentence behind the cursor.
    pub focus: std::ops::Range<usize>,
    /// How much of this row the IME is still composing, relative to its start.
    ///
    /// Here for the same reason as `focus`, and with a sharper case: committing
    /// a Japanese preedit can leave the characters **identical** — `あ` composed
    /// and `あ` committed — so a frame that compared only text would call the
    /// row unchanged and leave the underline drawn under settled prose.
    pub underline: std::ops::Range<usize>,
}

/// Every distinct role the document draws from, in one pass over its markup.
///
/// Used to size the row box, so it holds the tallest face on the page rather
/// than only the Latin one.
fn roles_in(chars: &[char], markup: &[LineMarkup]) -> Vec<Role> {
    let mut seen = Vec::new();
    for line in markup {
        for span in &line.spans {
            for i in span.range.clone() {
                let Some(&ch) = chars.get(i) else { continue };
                let role = role_for(line.block, span.style, script_of(ch));
                if !seen.contains(&role) {
                    seen.push(role);
                }
            }
        }
    }
    seen
}

/// The style of each character of a logical line, so measuring and drawing
/// agree on which face every character uses.
fn roles_for_line(line: &LineMarkup, chars: &[char]) -> Vec<Role> {
    let mut roles = Vec::with_capacity(line.range.len());
    for span in &line.spans {
        for i in span.range.clone() {
            roles.push(role_for(line.block, span.style, script_of(chars[i])));
        }
    }
    roles
}

/// The style of each character, for deciding what is dithered.
fn styles_for_line(line: &LineMarkup) -> Vec<Style> {
    let mut styles = Vec::with_capacity(line.range.len());
    for span in &line.spans {
        styles.extend(std::iter::repeat_n(span.style, span.range.len()));
    }
    styles
}

/// Lay the whole document out. `top` is the y of the first line, which the
/// caller moves to scroll.
pub fn layout(page: &Page, fonts: &mut impl Metrics, top: i32) -> Vec<VisualLine> {
    let theme = page.theme;
    let body_height = fonts.line_height(theme.body_px, &page.roles) * theme.leading;
    let mut out = Vec::new();
    let mut y = top;

    for (index, line) in page.markup.iter().enumerate() {
        let px = block_px(theme, line.block);
        let glyph_box = fonts.line_height(px, &page.roles);
        let height = glyph_box * theme.leading;
        let inset = ((height - glyph_box) / 2.0).max(0.0);
        let baseline = inset + fonts.ascent(px, &page.roles);
        if matches!(line.block, Block::Heading(_)) && !out.is_empty() {
            y += (body_height * theme.heading_space) as i32;
        }

        let indent = block_indent(theme, line.block);
        let width = page.measure.saturating_sub(indent) as u32;
        let roles = roles_for_line(line, page.chars);
        let base = line.range.start;

        // `wrap` indexes the slice it is given, so `roles` — built over the
        // same line — is indexed the same way.
        let text = &page.chars[line.range.clone()];
        let broken = wrap::wrap(text, width, |i, c| {
            let role = roles.get(i).copied().unwrap_or(Role::Body);
            fonts.advance(role, px, c).ceil() as u32
        });

        for vl in broken {
            let range = base + vl.range.start..base + vl.range.end;
            let focus = focus_within(&page.focus, &range);
            let underline = span_within(&page.underline, &range);
            out.push(VisualLine {
                range,
                markup: index,
                block: line.block,
                y,
                px,
                height: height as i32,
                inset: inset as i32,
                baseline: baseline as i32,
                focus,
                underline,
            });
            y += height as i32;
        }
    }
    out
}

/// The part of `row` that falls inside the focused span, as an offset into the
/// row itself.
///
/// No focus at all means the whole row, because "everything is solid" and
/// "focus mode is off" have to draw identically and there is no reason for them
/// to be two states.
fn focus_within(
    focus: &Option<std::ops::Range<usize>>,
    row: &std::ops::Range<usize>,
) -> std::ops::Range<usize> {
    let Some(span) = focus else {
        return 0..row.end - row.start;
    };
    clip_to_row(span, row)
}

/// Rule a line under composing text, from `from` to `to`.
///
/// Below the baseline rather than on it, by a fraction of the type size so it
/// holds its distance as the size changes, and clamped inside the row so it
/// cannot land in the leading of the line beneath — which the row's own damage
/// rectangle would not repaint.
fn underline(window: &mut Window, line: &VisualLine, from: f32, to: f32) {
    let drop = (line.px / 9.0).round().max(2.0) as i32;
    let thickness = (line.px / 20.0).round().max(1.0) as i32;
    let top = (line.y + line.baseline + drop).min(line.y + line.height - thickness);
    rule(window, top, thickness, from, to);
}

/// Rule a line through struck-out text, from `from` to `to`.
///
/// **Through the lowercase, not through the middle of the row.** A row is as
/// tall as its leading and its tallest face, so its centre sits below the
/// x-height of Latin prose. A third of the type size above the baseline lands
/// on the middle of the lowercase.
///
/// Clamped inside the row at the top, the way [`underline`] is at the bottom:
/// the row's own damage rectangle is what repaints this, and ink outside it is
/// left behind by the next update.
fn strikethrough(window: &mut Window, line: &VisualLine, from: f32, to: f32) {
    let rise = (line.px / 3.5).round().max(2.0) as i32;
    let thickness = (line.px / 20.0).round().max(1.0) as i32;
    let top = (line.y + line.baseline - rise).max(line.y);
    rule(window, top, thickness, from, to);
}

/// Rule the bottom edge of a highlight field.
///
/// Inside the field rather than under it, so it cannot land in the leading of
/// the row beneath — which this row's damage rectangle would not repaint. It is
/// what gives the run an edge on a panel where the field itself is only a few
/// levels away from paper.
fn field_rule(window: &mut Window, rect: Rect, ink: u8) {
    let thickness = (rect.height / 16).max(2).min(rect.height);
    let top = rect.y + rect.height - thickness;
    for y in top..top + thickness {
        for x in rect.x..rect.x + rect.width {
            window.put_pixel(x, y, ink);
        }
    }
}

/// The pixels of a horizontal rule, shared by the two things that draw one.
fn rule(window: &mut Window, top: i32, thickness: i32, from: f32, to: f32) {
    for y in top..top + thickness {
        for x in from as i32..to as i32 {
            if x >= 0 && y >= 0 {
                window.put_pixel(x as u16, y as u16, BLACK);
            }
        }
    }
}

/// The part of `row` that `span` covers, as an offset into the row. Empty when
/// there is no span or it lies elsewhere — the opposite default to
/// [`focus_within`], where nothing means everything.
fn span_within(
    span: &Option<std::ops::Range<usize>>,
    row: &std::ops::Range<usize>,
) -> std::ops::Range<usize> {
    span.as_ref().map_or(0..0, |span| clip_to_row(span, row))
}

fn clip_to_row(
    span: &std::ops::Range<usize>,
    row: &std::ops::Range<usize>,
) -> std::ops::Range<usize> {
    let start = span.start.clamp(row.start, row.end) - row.start;
    let end = span.end.clamp(row.start, row.end) - row.start;
    start..end
}

/// The visual line holding `cursor`.
///
/// After a soft wrap the cursor sits at the end of one line and the start of
/// the next; it belongs to the later one, which is where the next character
/// will appear. The trailing search catches a cursor at the very end of the
/// document, which no half-open range contains.
fn line_of(lines: &[VisualLine], cursor: usize) -> Option<usize> {
    lines
        .iter()
        .position(|l| l.range.contains(&cursor))
        .or_else(|| lines.iter().rposition(|l| l.range.end == cursor))
}

/// Horizontal position of `cursor` along `line`.
fn pen_at(page: &Page, fonts: &mut impl Metrics, line: &VisualLine, cursor: usize) -> Option<f32> {
    let entry = page.markup.get(line.markup)?;
    let roles = roles_for_line(entry, page.chars);
    let mut x = page.origin(line.block);
    for (i, ch) in page
        .chars
        .iter()
        .enumerate()
        .take(cursor)
        .skip(line.range.start)
    {
        let role = roles
            .get(i - entry.range.start)
            .copied()
            .unwrap_or(Role::Body);
        x += fonts.advance(role, line.px, *ch);
    }
    Some(x)
}

/// The character index on `line` nearest horizontal position `x`.
///
/// Nearest, not the one containing `x`: clicking or arrowing into the right
/// half of a glyph should land after it, which is what a caret between
/// characters means.
fn index_at(page: &Page, fonts: &mut impl Metrics, line: &VisualLine, x: f32) -> usize {
    let Some(entry) = page.markup.get(line.markup) else {
        return line.range.start;
    };
    let roles = roles_for_line(entry, page.chars);
    let mut pen = page.origin(line.block);
    for i in line.range.clone() {
        let role = roles
            .get(i - entry.range.start)
            .copied()
            .unwrap_or(Role::Body);
        let advance = fonts.advance(role, line.px, page.chars[i]);
        if x < pen + advance / 2.0 {
            return i;
        }
        pen += advance;
    }
    line.range.end
}

/// The character index nearest a point on the page.
///
/// In window coordinates, against the frame the reader is actually looking at.
/// A point below the last line lands at the end of it and one above the first
/// lands at its start: a finger aimed past the text still meant somewhere, and
/// refusing to answer would make a tap in the margin do nothing at all.
pub fn index_at_point(
    page: &Page,
    fonts: &mut impl Metrics,
    frame: &Frame,
    x: f32,
    y: f32,
) -> Option<usize> {
    let index = frame
        .lines
        .iter()
        .rposition(|line| (line.y as f32) <= y)
        .unwrap_or(0);
    let line = frame.lines.get(index)?;
    Some(index_at(page, fonts, line, x))
}

/// Move `cursor` by `delta` visual lines, holding `goal` as its column.
///
/// `goal` is the horizontal position the cursor is trying to keep. Editors
/// carry it across a run of vertical moves so that passing through a short line
/// does not drag the cursor permanently left; the caller keeps it and clears it
/// on any other kind of movement.
pub fn move_vertical(
    page: &Page,
    fonts: &mut impl Metrics,
    frame: &Frame,
    cursor: usize,
    delta: i32,
    goal: Option<f32>,
) -> Option<(usize, f32)> {
    let lines = &frame.lines;
    let from = line_of(lines, cursor)?;
    let goal = match goal {
        Some(goal) => goal,
        None => pen_at(page, fonts, &lines[from], cursor)?,
    };
    let to = (from as i32 + delta).clamp(0, lines.len().saturating_sub(1) as i32) as usize;
    if to == from {
        // Already at the top or bottom: hold the column so a further move in
        // the other direction still returns to it.
        return Some((cursor, goal));
    }
    Some((index_at(page, fonts, &lines[to], goal), goal))
}

/// How many visual lines fit on the surface, for page movement.
pub fn lines_per_page(fonts: &mut impl Metrics, theme: &Theme, roles: &[Role], height: u16) -> i32 {
    let line = fonts.line_height(theme.body_px, roles) * theme.leading;
    if line <= 0.0 {
        return 1;
    }
    // One line of overlap, so the reader keeps a foothold across a page turn.
    ((height as f32 / line) as i32 - 1).max(1)
}

/// How the page follows the cursor.
///
/// **Two different modes, and the second alone is not enough.** Unconditional
/// typewriter scrolling, clamped at the top, makes ordinary writing behave as
/// half a focus mode: text fills down from the first line, stops at the pin,
/// and thereafter scrolls under a fixed writing line. iA Writer only does that
/// in focus mode; its normal mode lets
/// the cursor run to the foot of the page like any other editor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scroll {
    /// Move only when the cursor would otherwise leave the page, and keep
    /// whatever offset the page already had. This is what makes the cursor run
    /// down to the bottom instead of sticking to a line partway up.
    Follow { top: i32, bottom: i32 },
    /// Hold the focused sentence's own middle on the middle of the page,
    /// wherever in the document it is — including the first sentence, which
    /// puts blank paper above it rather than refusing to scroll.
    ///
    /// **The sentence, not the caret's row.** Measured off an iA Writer
    /// capture: with a sentence wrapped across two rows the caret sat half a
    /// line *below* the middle, and the midpoint of the two rows sat on it.
    ///
    /// `top` and `bottom` are the edges of the **page**, not of the text
    /// column — see [`scroll_mode`].
    Centre { top: i32, bottom: i32 },
    /// Put the cursor's own row at the top of the text column, with the
    /// document below it.
    ///
    /// **For arriving at a section rather than at a word.** A search hit is a
    /// word wanted in context and is centred; a heading is a place to start
    /// reading, and everything above it is what the writer chose to leave.
    ///
    /// `top` is the text column's top margin, as `Follow`'s is.
    Top { top: i32 },
}

/// Which way the page follows the cursor.
///
/// **The two modes measure different boxes, and that is the whole reason this
/// is a function.**
///
/// Following is about the text column as it currently is: it starts at the top
/// margin, because the caret should not be dragged above where text begins, and
/// ends at `page_bottom`, because the caret must stay off the chrome.
///
/// Centring is about the **panel**, top to bottom, and neither the margin nor
/// the chrome moves it:
///
/// - The margin is where text starts, not where the page does. Using it as the
///   top put the focused sentence half a margin low.
/// - The strip comes and goes while writing. Measuring to it moved the focused
///   sentence by half the strip's height every time the chrome appeared, so the
///   page shifted under the reader for a reason that had nothing to do with
///   what they were writing.
///
/// **A jump centres too**, for a reason that is not focus mode's. Following
/// only moves the page as far as it must, so a cursor that arrived from off
/// screen lands flush against whichever edge it came in by — and a search hit
/// pinned to the last line of the page shows the writer everything before their
/// match and nothing after it. Every editor centres what it found; this is that,
/// and it reuses the machinery focus mode already needed.
/// **Landing on a section is the third case, and it outranks focus mode**: it is
/// the one paint where the writer has said where they want to be. The sentence
/// they land on is centred again by the next keystroke.
pub fn scroll_mode(
    focus: bool,
    jumped: bool,
    landing: bool,
    margin_y: i32,
    page_bottom: i32,
    panel: i32,
) -> Scroll {
    if landing {
        Scroll::Top { top: margin_y }
    } else if focus || jumped {
        Scroll::Centre {
            top: 0,
            bottom: panel,
        }
    } else {
        Scroll::Follow {
            top: margin_y,
            bottom: page_bottom,
        }
    }
}

/// Where the page should sit, given where it sat before.
///
/// Pure, and separate from moving the lines, because the interesting part is
/// the decision. `was` is the offset currently applied; the answer is the new
/// one.
///
/// **`Centre` is deliberately unclamped.** A negative offset pushes the text
/// down so the first sentence of a document can still sit in the middle of the
/// page, which is what focus mode does and what a clamp at zero prevents.
pub fn scroll_for(lines: &[VisualLine], cursor: usize, was: i32, how: Scroll) -> i32 {
    let Some(index) = line_of(lines, cursor) else {
        return was;
    };
    let row = &lines[index];
    match how {
        Scroll::Follow { top, bottom } => {
            // The row is visible while the offset is between these. Staying
            // put inside that window is the whole point: an editor that
            // re-centres on every keystroke is a focus mode nobody asked for.
            let highest = row.y - top;
            let lowest = row.y + row.height - bottom;
            was.min(highest).max(lowest).max(0)
        }
        Scroll::Centre { top, bottom } => {
            let middle = (top + bottom) / 2;
            let caret = (row.y, row.y + row.height);
            let (first, last) = focused_extent(lines).unwrap_or(caret);
            // A sentence taller than the page cannot be centred without
            // taking the caret off it, so the caret wins that argument.
            if last - first > bottom - top {
                return (caret.0 + caret.1) / 2 - middle;
            }
            (first + last) / 2 - middle
        }
        // Clamped at zero, so landing on the first heading of a document does
        // not push blank paper above it.
        Scroll::Top { top } => (row.y - top).max(0),
    }
}

/// Top and bottom of the rows the focused sentence covers.
///
/// `None` when nothing is focused, which is a cursor on a blank line — the
/// sentence there is empty and has no rows of its own.
fn focused_extent(lines: &[VisualLine]) -> Option<(i32, i32)> {
    let mut rows = lines.iter().filter(|l| !l.focus.is_empty());
    let first = rows.next()?;
    let last = rows.next_back().unwrap_or(first);
    Some((first.y, last.y + last.height))
}

/// Slide every row up by `offset`, so the page sits where `scroll_for` said.
pub fn shift(lines: &mut [VisualLine], offset: i32) {
    if offset == 0 {
        return;
    }
    for line in lines.iter_mut() {
        line.y -= offset;
    }
}

/// Where the caret sits: the visual line holding `cursor`, and how far along it.
///
/// The cursor can land at the end of one visual line and the start of the next
/// after a soft wrap. It is shown at the *start of the later line*, which is
/// where the next character will appear.
fn caret(
    page: &Page,
    fonts: &mut impl Metrics,
    lines: &[VisualLine],
    cursor: usize,
) -> Option<Rect> {
    let index = line_of(lines, cursor)?;
    let line = &lines[index];
    let x = pen_at(page, fonts, line, cursor)?;
    // The glyph box, not the leaded row: a caret marks where the text is, and
    // one spanning the leading stands taller than everything beside it. Taken
    // from the layout rather than measured again, so it cannot disagree with the
    // row it sits in — or with the rectangle that repaints it, which is the row
    // and so contains this.
    let top = (line.y + line.inset).max(0);
    let height = line.height - 2 * line.inset;
    Some(Rect {
        x: x as u16,
        y: top as u16,
        width: caret_width(line.px),
        height: height.max(1) as u16,
    })
}

/// The rectangles covering a selection, one per visual line it touches.
///
/// Per *visual* line, so a selection across a soft wrap is drawn as the runs it
/// visually is rather than as one impossible box.
///
/// A line whose selection carries on to the next gets a short nub past its last
/// glyph: the newline is inside the selection, and without it a multi-line
/// selection reads as several unrelated runs. A nub rather than a fill out to
/// the margin, because on this panel a full-width black band is a lot of ink to
/// lay down and then lift again.
fn selection_rects(
    page: &Page,
    fonts: &mut impl Metrics,
    lines: &[VisualLine],
    selection: &Option<std::ops::Range<usize>>,
) -> Vec<Rect> {
    let Some(selection) = selection else {
        return Vec::new();
    };
    lines
        .iter()
        .filter_map(|line| run_rect(page, fonts, line, selection, true))
        .collect()
}

/// The box `run` covers on one visual line, or `None` if it misses the line
/// entirely.
///
/// `nub` adds a space's width when the run carries on past this line, which is
/// what draws the newline a selection swallowed. A `==highlight==` passes
/// `false`: it never crosses a newline, so a run that continues does so at a
/// soft wrap, where there is no character between the two halves to stand for.
fn run_rect(
    page: &Page,
    fonts: &mut impl Metrics,
    line: &VisualLine,
    run: &std::ops::Range<usize>,
    nub: bool,
) -> Option<Rect> {
    let start = run.start.max(line.range.start);
    let end = run.end.min(line.range.end);
    if start >= end {
        return None;
    }
    let left = pen_at(page, fonts, line, start)?;
    let mut right = pen_at(page, fonts, line, end)?;
    if nub && run.end > line.range.end {
        right += fonts.advance(Role::Body, line.px, ' ');
    }
    // A line scrolled half off the top keeps the half that is still on the
    // page rather than being dropped or drawn above it.
    let top = line.y.max(0);
    let height = line.height - (top - line.y);
    if height <= 0 {
        return None;
    }
    Some(Rect {
        x: left as u16,
        y: top as u16,
        width: (right - left).max(0.0) as u16,
        height: height as u16,
    })
}

/// The fields behind every `==highlight==` on the page, each with whether focus
/// mode has set its row back.
///
/// **A field that is partly in the focused sentence is drawn lit**, whole. One
/// phrase is one box: a field split at the focus boundary would read as two
/// phrases, one of them set back.
fn highlight_fields(
    page: &Page,
    fonts: &mut impl Metrics,
    lines: &[VisualLine],
) -> Vec<(Rect, bool)> {
    let mut fields = Vec::new();
    for line in lines {
        let Some(entry) = page.markup.get(line.markup) else {
            continue;
        };
        for run in &entry.highlights {
            let Some(rect) = run_rect(page, fonts, line, run, false) else {
                continue;
            };
            let from = run.start.max(line.range.start) - line.range.start;
            let to = run.end.min(line.range.end) - line.range.start;
            let lit = (from..to).any(|at| line.focus.contains(&at));
            fields.push((rect, !lit));
        }
    }
    fields
}

/// The vertical extent of a set of selection rectangles.
///
/// Only the vertical matters: the damage rectangle is always full width, so a
/// y-span is exactly as much as it can express.
fn selection_span(rects: &[Rect]) -> Option<(i32, i32)> {
    let top = rects.iter().map(|r| r.y as i32).min()?;
    let bottom = rects.iter().map(|r| r.y as i32 + r.height as i32).max()?;
    Some((top, bottom))
}

/// What was last drawn, so the next paint can work out what actually changed.
///
/// Holds its own copy of the document because the comparison is exact — line
/// contents are compared character by character rather than hashed, so no
/// collision can silently skip a repaint.
pub struct Frame {
    chars: Vec<char>,
    lines: Vec<VisualLine>,
    caret: Option<Rect>,
    /// The vertical extent of the selection that was drawn, so that clearing
    /// one repaints where it was. Without this a dropped selection leaves a
    /// black band on the page.
    selection: Option<(i32, i32)>,
    /// Where the candidate box was, for the same reason as the selection: it
    /// covers prose, and committing a word makes it vanish without any line
    /// changing. Unremembered, it would leave a bordered white hole in the page.
    candidates: Option<Rect>,
}

impl Frame {
    /// The candidate box on screen, for the tap test to hit against.
    pub fn candidate_box(&self) -> Option<Rect> {
        self.candidates
    }
}

impl Frame {
    /// Whether visual line `i` is identical to the one drawn last time.
    ///
    /// Geometry as well as text: a line whose characters are unchanged but
    /// which has moved down the page still has to be redrawn.
    fn unchanged(&self, i: usize, chars: &[char], line: &VisualLine) -> bool {
        let Some(old) = self.lines.get(i) else {
            return false;
        };
        old.y == line.y
            && old.px == line.px
            && old.height == line.height
            && old.baseline == line.baseline
            && old.block == line.block
            && old.focus == line.focus
            && old.underline == line.underline
            && self.chars.get(old.range.clone()) == chars.get(line.range.clone())
    }
}

/// The smallest rectangle covering everything that differs between two frames.
///
/// `None` when nothing changed. Typing inside a line dirties that line, which is
/// the case worth keeping cheap.
///
/// **A vertical shift dirties the whole page, upwards as well as down.** A
/// newline or a rewrap moves content down and leaves ink below; scrolling
/// towards the start of the document moves content down the screen and leaves
/// ink *above* the new lines. `top` is measured from where the lines are now, so
/// it cannot see the second case on its own.
fn damage(
    previous: Option<&Frame>,
    chars: &[char],
    lines: &[VisualLine],
    caret: Option<Rect>,
    selection: Option<(i32, i32)>,
    candidates: Option<Rect>,
    surface: Rect,
) -> Option<Rect> {
    let Some(previous) = previous else {
        return Some(surface);
    };

    let mut top = i32::MAX;
    let mut bottom = i32::MIN;
    let mut shifted = false;

    for (i, line) in lines.iter().enumerate() {
        if previous.unchanged(i, chars, line) {
            continue;
        }
        top = top.min(line.y);
        bottom = bottom.max(line.y + line.height);
        if previous.lines.get(i).is_some_and(|old| old.y != line.y) {
            shifted = true;
        }
    }
    // Lines that existed last time and no longer do leave ink behind.
    if previous.lines.len() != lines.len() {
        shifted = true;
        if let Some(old) = previous.lines.get(lines.len().min(previous.lines.len())) {
            top = top.min(old.y);
        }
    }

    // The caret moves without any line changing.
    for rect in [previous.caret, caret].into_iter().flatten() {
        top = top.min(rect.y as i32);
        bottom = bottom.max(rect.y as i32 + rect.height as i32);
    }

    // A selection appears, grows and vanishes without any line changing
    // either — and unlike the caret it leaves a black band behind if the old
    // one is not included.
    for (t, b) in [previous.selection, selection].into_iter().flatten() {
        top = top.min(t);
        bottom = bottom.max(b);
    }

    // And so does the candidate box, which covers prose and vanishes on commit
    // without any line changing. Both the previous box and the next: the first
    // to clear where it was, the second because the rows under it are about to
    // be hidden.
    for rect in [previous.candidates, candidates].into_iter().flatten() {
        top = top.min(rect.y as i32);
        bottom = bottom.max(rect.y as i32 + rect.height as i32);
    }

    if top == i32::MAX {
        return None;
    }
    if shifted {
        top = 0;
        bottom = surface.height as i32;
    }

    let y = top.clamp(0, surface.height as i32) as u16;
    let end = bottom.clamp(0, surface.height as i32) as u16;
    Some(Rect {
        x: 0,
        y,
        width: surface.width,
        height: end.saturating_sub(y),
    })
}

/// Draw the document, presenting only what changed since `previous`.
///
/// Pass `None` for the first paint. The returned frame is what the next call
/// compares against.
/// What the editor is doing right now, as against what the document says.
///
/// Bundled because all three are the same kind of thing — transient state that
/// the page is drawn *with* rather than part of the page — and because passing
/// them separately took `paint` past the number of arguments anyone can read.
///
/// Indices here are **display** indices, matching [`Page::chars`]: with a
/// preedit spliced in, document positions and screen positions are not the
/// same number.
pub struct Editing<'a> {
    pub cursor: usize,
    pub selection: Option<std::ops::Range<usize>>,
    /// What floats over the page beside the caret, if anything.
    pub overlay: crate::ui::Overlay<'a>,
    /// What the overlay hangs off when the caret is the wrong thing to hang it
    /// off: the find bar's field, which is on the strip while the caret is
    /// wherever the last match was. `None` means the caret, which is the
    /// ordinary case of composing into the prose.
    pub anchor: Option<Rect>,
}

pub fn paint(
    window: &mut Window,
    fonts: &mut Fonts,
    page: &Page,
    lines: Vec<VisualLine>,
    editing: &Editing,
    previous: Option<&Frame>,
) -> Result<Frame> {
    let Editing {
        cursor,
        selection,
        overlay,
        anchor,
    } = editing;
    let labels = overlay.labels();
    let surface = window.full();
    let caret = caret(page, fonts, &lines, *cursor);
    let selected = selection_rects(page, fonts, &lines, selection);
    let span = selection_span(&selected);
    let fields = highlight_fields(page, fonts, &lines);

    // Damage is clipped to the page. A rewrap extends it to the foot of the
    // surface, and without this that clears the action strip — which is drawn
    // once and not repainted per keystroke, so the buttons simply vanish while
    // typing.
    let surface = Rect {
        height: page.bottom.min(surface.height),
        ..surface
    };
    // Where the box will go, worked out before the damage so that the
    // rectangle covers it — and before drawing, so the box is not measured
    // against a page that has already been cleared.
    let width = window.width();
    let box_now = anchor.or(caret).and_then(|at| {
        crate::ui::overlay_rect(width, fonts, at, page.theme.body_px, page.bottom, &labels)
    });
    let Some(dirty) = damage(previous, page.chars, &lines, caret, span, box_now, surface) else {
        return Ok(Frame {
            chars: page.chars.to_vec(),
            lines,
            caret,
            selection: span,
            candidates: box_now,
        });
    };

    // Clearing first means every line that touches the damage has to be drawn
    // again, including ones that did not themselves change — a line half inside
    // the rectangle would otherwise lose its other half.
    window.fill(dirty, crate::window::WHITE);
    let top = dirty.y as i32;
    let bottom = top + dirty.height as i32;

    // Highlight fields first of all: they are the only thing on the page that
    // is genuinely *behind* the text, and a selection drawn over one has to
    // win, which it cannot do from underneath.
    //
    // Skipped outside the damage on the same test the line loop uses below —
    // a field filled where its line is not being redrawn would leave a band
    // with nothing written on it.
    for (rect, quiet) in &fields {
        let rect_bottom = rect.y as i32 + rect.height as i32;
        if rect_bottom <= top || rect.y as i32 >= bottom {
            continue;
        }
        let ink = window.field_ink(*quiet);
        window.fill(*rect, ink);
        field_rule(window, *rect, window.field_rule_ink(*quiet));
    }

    // The inverted runs go down before the glyphs, which are drawn in white
    // where they land on one.
    //
    // Skipped entirely outside the damage, on the same test the line loop uses
    // below: a rectangle filled where its line is not being redrawn would leave
    // a black band with nothing written on it.
    for rect in &selected {
        let rect_bottom = rect.y as i32 + rect.height as i32;
        if rect_bottom <= top || rect.y as i32 >= bottom {
            continue;
        }
        window.fill(*rect, crate::window::BLACK);
    }

    for line in &lines {
        let line_bottom = line.y + line.height;
        // Clipped to the page as well as to the damage: a line that would
        // reach under the action strip is not drawn at all.
        if line_bottom <= top || line.y >= bottom || line.y >= page.bottom as i32 {
            continue;
        }
        let Some(entry) = page.markup.get(line.markup) else {
            continue;
        };
        let roles = roles_for_line(entry, page.chars);
        let styles = styles_for_line(entry);

        let mut pen = page.origin(line.block);
        let baseline = (line.y + line.baseline) as f32;
        // Collected as the pen passes, rather than measured again afterwards:
        // the rule has to start and stop under the glyphs actually drawn.
        let mut rule: Option<(f32, f32)> = None;
        // The same, for struck-out text — but a *run* at a time: a line can
        // hold two struck phrases with prose between them, and one span from
        // the first to the last would rule through the prose.
        let mut struck: Option<(f32, f32)> = None;

        for i in line.range.clone() {
            let at = i - entry.range.start;
            let role = roles.get(at).copied().unwrap_or(Role::Body);
            let style = styles.get(at).copied().unwrap_or(Style::Text);
            let ch = page.chars[i];
            // Two unrelated reasons to recede, one ink level. A syntax mark is
            // always quiet; anything outside the focused sentence is quiet
            // while focus mode is on.
            let quiet = matches!(style, Style::Syntax | Style::Url)
                || !line.focus.contains(&(i - line.range.start));
            let inverted = selection.as_ref().is_some_and(|s| s.contains(&i));
            let ink = ink(inverted, quiet);

            let origin_x = pen;
            fonts.draw(role, line.px, ch, |gx, gy, coverage| {
                // The rasterizer reports coverage; the panel takes ink. One bit,
                // because a two-level waveform turns grey into mud.
                if coverage <= 0.5 {
                    return;
                }
                let x = origin_x as i32 + gx;
                let y = baseline as i32 + gy;
                if x < 0 || y < 0 {
                    return;
                }
                window.put_pixel(x as u16, y as u16, ink);
            });
            pen += fonts.advance(role, line.px, ch);
            if line.underline.contains(&(i - line.range.start)) {
                let (from, _) = rule.unwrap_or((origin_x, origin_x));
                rule = Some((from, pen));
            }
            if style == Style::Strikethrough {
                let (from, _) = struck.unwrap_or((origin_x, origin_x));
                struck = Some((from, pen));
            } else if let Some((from, to)) = struck.take() {
                strikethrough(window, line, from, to);
            }
        }
        if let Some((from, to)) = rule {
            underline(window, line, from, to);
        }
        if let Some((from, to)) = struck {
            strikethrough(window, line, from, to);
        }
    }

    if let Some(rect) = caret {
        let ink = window.caret_ink();
        window.fill(rect, ink);
    }

    // Last, over everything: the box floats above the page, and drawing it
    // before the glyphs would put the prose on top of it.
    if let Some(rect) = box_now {
        crate::ui::draw_overlay(window, fonts, rect, page.theme.body_px, &labels);
    }

    window.present(dirty)?;
    Ok(Frame {
        chars: page.chars.to_vec(),
        lines,
        caret,
        selection: span,
        candidates: box_now,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A page at `px` and the default line length, set in the default face.
    fn theme_at(px: f32) -> Theme {
        Theme::at(px, DEFAULT_LINE_LENGTH, px * DEFAULT_ADVANCE)
    }

    /// **Every size has to leave a line worth reading**, whichever way up the
    /// panel is. The setting asks for [`DEFAULT_LINE_LENGTH`] characters and
    /// gets them until the surface runs out; past that the cap is what sets the
    /// line, and the top of the ladder on a portrait page is where it bites.
    #[test]
    fn the_column_follows_the_type_so_the_line_stays_readable() {
        // Measured off the column the page will actually wrap to, not off the
        // theme: the theme asks for the same count at every size by
        // construction, and the cap is the only thing that can be wrong.
        let chars: Vec<char> = "a".chars().collect();
        let markup = karyll_core::markdown::analyze(&chars);
        for surface in [(1860u16, 2480u16), (2480, 1860)] {
            for px in SIZES {
                let theme = theme_at(px);
                let page = Page::new(&chars, &markup, &theme, surface, surface.1 - 120);
                let per_line = page.measure as f32 / (px * DEFAULT_ADVANCE);
                assert!(
                    (45.0..=75.0).contains(&per_line),
                    "{px} px on {surface:?} sets {per_line:.0} characters to the line"
                );
            }
        }
    }

    /// The line length is the writer's, and the size is not allowed to take it
    /// back: the same setting has to give the same line at every size the
    /// surface can hold it at.
    #[test]
    fn the_line_length_is_the_line_length_at_every_size_that_fits() {
        for chars in LINE_LENGTHS {
            for px in SIZES {
                let theme = Theme::at(px, chars, px * DEFAULT_ADVANCE);
                let (_, measure) = column(&theme, 2480);
                if measure == theme.measure {
                    let per_line = measure as f32 / (px * DEFAULT_ADVANCE);
                    assert!(
                        (per_line - chars as f32).abs() < 1.0,
                        "{chars} characters at {px} px came out as {per_line:.1}"
                    );
                }
            }
        }
    }

    /// A shorter line is the margin control, and the only one: the column is
    /// what was asked for and the margin is the rest of the surface.
    #[test]
    fn a_shorter_line_is_a_wider_margin() {
        let mut last = u16::MAX;
        for chars in LINE_LENGTHS {
            let theme = Theme::at(DEFAULT_SIZE, chars, DEFAULT_SIZE * DEFAULT_ADVANCE);
            let (left, _) = column(&theme, 1860);
            assert!(left < last, "{chars} characters did not narrow the margin");
            assert!(left >= SIDE_MARGIN, "{chars} characters leaves no margin");
            last = left;
        }
    }

    /// A page of `text`, laid out at the default size on a Colorsoft-shaped
    /// surface, with everything needed to ask where its highlight fields land.
    fn fielded(text: &str, focus: Option<std::ops::Range<usize>>) -> Vec<(Rect, bool)> {
        let chars: Vec<char> = text.chars().collect();
        let markup = karyll_core::markdown::analyze(&chars);
        let theme = theme_at(DEFAULT_SIZE);
        let page = Page::new(&chars, &markup, &theme, (1272, 1696), 1600).focused_on(focus);
        let lines = layout(&page, &mut crate::font::Proportional, 0);
        highlight_fields(&page, &mut crate::font::Proportional, &lines)
    }

    #[test]
    fn a_highlight_draws_one_field_covering_its_markers() {
        let fields = fielded("the ==marked== word", None);
        assert_eq!(fields.len(), 1, "one run, one field");
        let (rect, quiet) = fields[0];
        assert!(!quiet, "focus mode is off, so nothing is set back");
        assert!(rect.width > 0 && rect.height > 0);
        // It starts after "the " and is as wide as `==marked==` — fourteen
        // characters of the run against four before it.
        let (rect_all, _) = fielded("==the marked word==", None)[0];
        assert!(rect_all.width > rect.width, "a longer run is a wider field");
        assert!(rect_all.x < rect.x, "and it starts further left");
    }

    #[test]
    fn two_runs_on_a_line_are_two_fields() {
        assert_eq!(fielded("==one== and ==two==", None).len(), 2);
    }

    #[test]
    fn no_highlight_is_no_field() {
        assert!(fielded("nothing marked here", None).is_empty());
    }

    #[test]
    fn focus_mode_sets_back_the_fields_it_is_not_on() {
        // With the cursor in the second run, the first is set back and the
        // second is not.
        let text = "==one== and ==two==";
        let second = text.find("==two==").unwrap();
        let fields = fielded(text, Some(second..second + 7));
        assert_eq!(fields.len(), 2);
        assert!(fields[0].1, "the run off the focused sentence is set back");
        assert!(!fields[1].1, "the one under it is not");
    }

    #[test]
    fn a_field_the_focus_only_partly_covers_stays_lit() {
        // Splitting the box at the boundary would draw one phrase as two.
        let fields = fielded("==one two==", Some(0..5));
        assert_eq!(fields.len(), 1);
        assert!(!fields[0].1);
    }

    #[test]
    fn a_highlight_that_wraps_is_a_field_per_visual_line() {
        // Long enough to wrap the 1272 px surface at the default size, so the
        // run crosses a soft wrap and has to be drawn as the runs it visually
        // is — the same rule a selection follows.
        //
        // Trimmed, because a closing marker has to follow a non-space the way
        // an emphasis one does, and a trailing space would leave the run open.
        let long = format!("=={}==", "word ".repeat(60).trim_end());
        let fields = fielded(&long, None);
        assert!(
            fields.len() > 1,
            "a wrapped run draws one box per row, got {}",
            fields.len()
        );
    }

    #[test]
    fn the_ladder_steps_and_stops_at_both_ends() {
        assert_eq!(step_size(46.0, true), 52.0);
        assert_eq!(step_size(46.0, false), 42.0);
        // No wrapping: the smallest is not one press from the largest, which
        // would be a surprise rather than a convenience.
        assert_eq!(step_size(SIZES[0], false), SIZES[0]);
        assert_eq!(
            step_size(SIZES[SIZES.len() - 1], true),
            SIZES[SIZES.len() - 1]
        );
    }

    /// A size remembered by another build is snapped rather than refused, the
    /// way a stored cursor past the end is clamped.
    #[test]
    fn a_size_that_is_no_longer_offered_lands_on_the_nearest() {
        assert_eq!(nearest_size(47.0), 46.0);
        assert_eq!(nearest_size(1.0), SIZES[0]);
        assert_eq!(nearest_size(500.0), SIZES[SIZES.len() - 1]);
        for px in SIZES {
            assert_eq!(nearest_size(px), px, "an offered size is left alone");
        }
    }

    /// The largest sizes ask for a column wider than a portrait page. Unfitted,
    /// `left` saturates to zero and the lines run under both bezels.
    #[test]
    fn the_column_is_capped_to_what_the_surface_holds() {
        let chars: Vec<char> = "a".chars().collect();
        let markup = karyll_core::markdown::analyze(&chars);
        for px in SIZES {
            let theme = theme_at(px);
            let page = Page::new(&chars, &markup, &theme, (1860, 2480), 2360);
            assert!(page.measure + 2 * SIDE_MARGIN <= 1860, "{px} px overflows");
            assert!(page.left >= SIDE_MARGIN, "{px} px leaves no margin");
            assert!(page.measure > 0);
        }
    }

    mod margins {
        use super::*;

        /// A portrait panel with the strip on it, which is what a reader with
        /// no keyboard is looking at.
        const W: u16 = 1860;
        const BOTTOM: u16 = 2480 - crate::ui::STRIP_H;

        fn at(theme: &Theme, x: u16, y: u16) -> Option<Edge> {
            edge_at(theme, W, BOTTOM, x, y)
        }

        #[test]
        fn each_margin_moves_the_page_its_own_way() {
            let theme = Theme::default();
            let middle = BOTTOM / 2;
            assert_eq!(at(&theme, 20, middle), Some(Edge::Back));
            assert_eq!(at(&theme, W - 20, middle), Some(Edge::On));
            assert_eq!(at(&theme, W / 2, 20), Some(Edge::Start));
            assert_eq!(at(&theme, W / 2, BOTTOM - 20), Some(Edge::End));
        }

        /// The page itself is the largest part of the page, and a tap there
        /// still places the cursor.
        #[test]
        fn the_column_between_them_is_not_an_edge() {
            let theme = Theme::default();
            assert_eq!(at(&theme, W / 2, BOTTOM / 2), None);
            let (left, measure) = column(&theme, W);
            assert_eq!(at(&theme, left, BOTTOM / 2), None, "the first character");
            assert_eq!(
                at(&theme, left + measure - 1, BOTTOM / 2),
                None,
                "and the last"
            );
        }

        /// A corner turns a page rather than jumping to an end: it is where a
        /// thumb strays, and a page turn is the cheaper mistake.
        #[test]
        fn the_corners_belong_to_the_sides() {
            let theme = Theme::default();
            assert_eq!(at(&theme, 20, 20), Some(Edge::Back));
            assert_eq!(at(&theme, 20, BOTTOM - 20), Some(Edge::Back));
            assert_eq!(at(&theme, W - 20, 20), Some(Edge::On));
            assert_eq!(at(&theme, W - 20, BOTTOM - 20), Some(Edge::On));
        }

        /// The margins narrow as the type grows, and a control narrower than a
        /// fingertip is not one. Every size has to leave a reachable edge.
        #[test]
        fn every_type_size_leaves_an_edge_worth_aiming_at() {
            for px in SIZES {
                let theme = theme_at(px);
                let middle = BOTTOM / 2;
                assert_eq!(at(&theme, EDGE - 1, middle), Some(Edge::Back), "{px} px");
                assert_eq!(
                    at(&theme, W - EDGE, middle),
                    Some(Edge::On),
                    "{px} px on the right"
                );
                // And the two sides are the same width, whichever it is.
                let (left, _) = column(&theme, W);
                let side = left.max(EDGE);
                assert_eq!(at(&theme, side, middle), None);
                assert_eq!(at(&theme, W - side - 1, middle), None);
            }
        }

        /// The band is above the strip, not on it: the strip is hit-tested
        /// first and its buttons are not page turns.
        #[test]
        fn the_bottom_band_stops_where_the_page_does() {
            let theme = Theme::default();
            assert_eq!(at(&theme, W / 2, BOTTOM - 1), Some(Edge::End));
            assert_eq!(at(&theme, W / 2, BOTTOM - EDGE), Some(Edge::End));
            assert_eq!(at(&theme, W / 2, BOTTOM - EDGE - 1), None);
        }
    }

    #[test]
    fn headings_step_down_towards_the_body_size() {
        let theme = Theme::default();
        let h1 = block_px(&theme, Block::Heading(1));
        let h2 = block_px(&theme, Block::Heading(2));
        let h3 = block_px(&theme, Block::Heading(3));
        let body = block_px(&theme, Block::Paragraph);
        assert!(h1 > h2 && h2 > h3 && h3 > body);
        // Deep headings stop shrinking rather than going below body text.
        assert_eq!(block_px(&theme, Block::Heading(6)), body);
    }

    #[test]
    fn a_syntax_mark_is_grey_and_body_text_is_black() {
        use crate::window::{QUIET, WHITE};
        assert_eq!(ink(false, false), BLACK);
        assert_eq!(ink(false, true), QUIET);
        // Three distinct values, which is the whole claim: one extra ink level,
        // not a ramp. If QUIET ever collapses onto BLACK the marks stop being
        // recessive and nothing else in the app would say so.
        assert_ne!(ink(false, true), ink(false, false));
        // A selection inverts everything it covers, quiet marks included.
        assert_eq!(ink(true, true), WHITE);
        assert_eq!(ink(true, false), WHITE);
    }

    #[test]
    fn focus_off_leaves_every_row_solid_from_end_to_end() {
        let row = 10..20;
        assert_eq!(focus_within(&None, &row), 0..10);
    }

    #[test]
    fn a_row_holding_the_focused_sentence_reports_it_relative_to_itself() {
        // The span is in document indices; the row wants its own offsets, so
        // that a line further down the page is not described in terms of how
        // much text precedes it.
        assert_eq!(focus_within(&Some(12..18), &(10..20)), 2..8);
    }

    #[test]
    fn a_sentence_running_past_a_row_is_clipped_to_it_at_both_ends() {
        assert_eq!(focus_within(&Some(0..100), &(10..20)), 0..10);
        assert_eq!(focus_within(&Some(5..15), &(10..20)), 0..5);
        assert_eq!(focus_within(&Some(15..40), &(10..20)), 5..10);
    }

    #[test]
    fn a_row_the_sentence_never_reaches_has_nothing_in_focus() {
        assert!(focus_within(&Some(0..5), &(10..20)).is_empty());
        assert!(focus_within(&Some(40..50), &(10..20)).is_empty());
    }

    #[test]
    fn moving_the_cursor_to_the_next_sentence_makes_the_rows_it_touches_dirty() {
        // The trap this whole field exists for. A cursor moving between
        // sentences changes no text, no size and no position — so without
        // focus in the comparison both rows count as unchanged, never repaint,
        // and the dimming lags a sentence behind the cursor.
        let text = "One here. Two there.";
        let chars: Vec<char> = text.chars().collect();
        let mut before = line(0..20, 0);
        before.focus = 0..9;
        let previous = frame(text, vec![before], None);

        let mut after = line(0..20, 0);
        after.focus = 10..20;
        assert!(!previous.unchanged(0, &chars, &after));

        // And a row whose focus did not move is still left alone, or every
        // keystroke would repaint the page.
        let mut same = line(0..20, 0);
        same.focus = 0..9;
        assert!(previous.unchanged(0, &chars, &same));
    }

    #[test]
    fn the_body_size_is_readable_on_a_ten_inch_page() {
        let theme = Theme::default();
        // ~300 ppi, so 1 pt is about 4.17 px. Below 10 pt is footnote type on a
        // page nearly as large as A4, which is why the ladder starts where it
        // does.
        let pt = theme.body_px / 4.17;
        assert!((10.0..=12.0).contains(&pt), "body is {pt} pt");
        // Against a nominal half-em character rather than the face's own, so
        // this checks the page a writer opens and not the arithmetic that set
        // it.
        let latin_advance = theme.body_px * 0.5;
        let chars_per_line = theme.measure as f32 / latin_advance;
        assert!(
            (45.0..=75.0).contains(&chars_per_line),
            "{chars_per_line} characters per line"
        );
    }

    #[test]
    fn lists_and_quotes_hang_but_paragraphs_do_not() {
        let theme = Theme::default();
        assert!(block_indent(&theme, Block::Quote) > 0);
        assert!(block_indent(&theme, Block::ListItem { ordered: true }) > 0);
        assert_eq!(block_indent(&theme, Block::Paragraph), 0);
        assert_eq!(block_indent(&theme, Block::Heading(1)), 0);
    }

    #[test]
    fn the_measure_leaves_margins_on_this_panel() {
        let theme = Theme::default();
        // 1860 px wide panel; a fixed column with real margins either side.
        assert!(theme.measure < 1860);
        assert!(
            (1860 - theme.measure) / 2 > 200,
            "margins should be generous"
        );
    }

    use crate::font::Stub;

    const SURFACE: Rect = Rect {
        x: 0,
        y: 0,
        width: 1860,
        height: 2480,
    };

    /// One paragraph line covering `range`, sitting at `y`. Fully in focus,
    /// which is what focus mode switched off looks like.
    fn line(range: std::ops::Range<usize>, y: i32) -> VisualLine {
        let focus = 0..range.end - range.start;
        VisualLine {
            range,
            markup: 0,
            block: Block::Paragraph,
            y,
            px: 34.0,
            height: 34,
            inset: 4,
            baseline: 30,
            focus,
            underline: 0..0,
        }
    }

    fn frame(text: &str, lines: Vec<VisualLine>, caret: Option<Rect>) -> Frame {
        Frame {
            chars: text.chars().collect(),
            lines,
            caret,
            selection: None,
            candidates: None,
        }
    }

    #[test]
    fn the_first_paint_covers_the_whole_surface() {
        let chars: Vec<char> = "abc".chars().collect();
        assert_eq!(
            damage(None, &chars, &[line(0..3, 100)], None, None, None, SURFACE),
            Some(SURFACE)
        );
    }

    #[test]
    fn an_unchanged_document_costs_no_update_at_all() {
        let chars: Vec<char> = "abc".chars().collect();
        let previous = frame("abc", vec![line(0..3, 100)], None);
        assert_eq!(
            damage(
                Some(&previous),
                &chars,
                &[line(0..3, 100)],
                None,
                None,
                None,
                SURFACE
            ),
            None
        );
    }

    #[test]
    fn typing_inside_a_line_dirties_only_that_line() {
        // Three lines; the middle one gains a character without moving.
        let previous = frame(
            "one\ntwo\nsix",
            vec![line(0..3, 100), line(4..7, 200), line(8..11, 300)],
            None,
        );
        let chars: Vec<char> = "one\ntwoo\nsix".chars().collect();
        let now = [line(0..3, 100), line(4..8, 200), line(9..12, 300)];

        let dirty = damage(Some(&previous), &chars, &now, None, None, None, SURFACE).unwrap();
        assert_eq!(dirty.y, 200, "should start at the changed line");
        assert!(
            (dirty.height as i32) < 200,
            "one line, not the rest of the page: {dirty:?}"
        );
    }

    #[test]
    fn a_line_that_moves_dirties_everything_below_it() {
        // A newline pushes the following lines down, so their old ink has to go.
        let previous = frame("a\nb", vec![line(0..1, 100), line(2..3, 200)], None);
        let chars: Vec<char> = "a\n\nb".chars().collect();
        let now = [line(0..1, 100), line(2..2, 200), line(3..4, 300)];

        let dirty = damage(Some(&previous), &chars, &now, None, None, None, SURFACE).unwrap();
        assert_eq!(
            dirty.y as i32 + dirty.height as i32,
            SURFACE.height as i32,
            "damage must run to the bottom once anything shifts"
        );
    }

    #[test]
    fn a_vertical_shift_clears_the_band_above_the_new_lines() {
        let previous = frame("a\nb", vec![line(0..1, 0), line(2..3, 100)], None);
        let chars: Vec<char> = "a\nb".chars().collect();
        let scrolled = [line(0..1, 400), line(2..3, 500)];

        let dirty = damage(
            Some(&previous),
            &chars,
            &scrolled,
            None,
            None,
            None,
            SURFACE,
        )
        .unwrap();
        assert_eq!(dirty.y, 0);
        assert_eq!(dirty.y as i32 + dirty.height as i32, SURFACE.height as i32);
    }

    #[test]
    fn a_rows_damage_is_the_height_the_layout_measured() {
        let previous = frame("ab", vec![line(0..2, 100)], None);
        let chars: Vec<char> = "aXb".chars().collect();
        let mut taller = line(0..3, 100);
        taller.height = 300;

        let dirty = damage(
            Some(&previous),
            &chars,
            &[taller],
            None,
            None,
            None,
            SURFACE,
        )
        .unwrap();
        assert_eq!(dirty.y, 100);
        assert_eq!(dirty.height, 300);
    }

    #[test]
    fn a_page_with_han_on_it_gets_taller_rows_than_a_latin_one() {
        let theme = Theme::default();
        let rows = |text: &str| {
            let chars: Vec<char> = text.chars().collect();
            let markup = karyll_core::markdown::analyze(&chars);
            let page = Page::new(&chars, &markup, &theme, (1860, 2480), 2360);
            layout(&page, &mut Stub, 0)[0].height
        };
        assert!(rows("hello 你好") > rows("hello"));
    }

    #[test]
    fn the_leading_is_split_above_and_below_the_glyph_box() {
        let theme = Theme::default();
        let chars: Vec<char> = "hello 你好".chars().collect();
        let markup = karyll_core::markdown::analyze(&chars);
        let page = Page::new(&chars, &markup, &theme, (1860, 2480), 2360);
        let line = &layout(&page, &mut Stub, 0)[0];

        let glyph_box = line.height - 2 * line.inset;
        assert!(line.inset > 0, "1.30 leading leaves space to split");
        // Same air above the text as below it. All of it underneath is what put
        // Han at the top of its row and left the caret hanging below the words.
        assert_eq!(line.height - line.inset - glyph_box, line.inset);
        // The baseline is inside the glyph box, not above it.
        assert!(line.baseline > line.inset);
        assert!(line.baseline <= line.inset + glyph_box);
    }

    #[test]
    fn the_caret_stands_where_the_text_does() {
        let theme = Theme::default();
        let chars: Vec<char> = "你好".chars().collect();
        let markup = karyll_core::markdown::analyze(&chars);
        let page = Page::new(&chars, &markup, &theme, (1860, 2480), 2360);
        let lines = layout(&page, &mut Stub, 0);
        let bar = caret(&page, &mut Stub, &lines, 1).expect("a caret on the first line");

        let line = &lines[0];
        assert_eq!(bar.y as i32, line.y + line.inset);
        assert_eq!(bar.height as i32, line.height - 2 * line.inset);
        // And still inside the rectangle that repaints the row, or clearing it
        // would leave a stripe behind.
        assert!(bar.y as i32 >= line.y);
        assert!(bar.y as i32 + bar.height as i32 <= line.y + line.height);
    }

    #[test]
    fn the_caret_is_wide_enough_to_see_on_this_panel() {
        let theme = Theme::default();
        assert_eq!(caret_width(theme.body_px), 6);
        // A heading's caret is proportionally the same weight as the body's.
        assert!(caret_width(block_px(&theme, Block::Heading(1))) > caret_width(theme.body_px));
        assert!(caret_width(8.0) >= 3, "never back to a hairline");
    }

    #[test]
    fn the_roles_of_a_document_are_the_faces_it_will_need() {
        let chars: Vec<char> = "hello".chars().collect();
        let markup = karyll_core::markdown::analyze(&chars);
        assert_eq!(roles_in(&chars, &markup), vec![Role::Body]);

        let chars: Vec<char> = "hello 你好".chars().collect();
        let markup = karyll_core::markdown::analyze(&chars);
        let roles = roles_in(&chars, &markup);
        assert!(roles.contains(&Role::Body) && roles.contains(&Role::Han));
        assert_eq!(roles.len(), 2, "one entry per face, not per character");
    }

    #[test]
    fn deleting_a_line_clears_the_ink_it_left_behind() {
        let previous = frame(
            "a\nb\nc",
            vec![line(0..1, 100), line(2..3, 200), line(4..5, 300)],
            None,
        );
        let chars: Vec<char> = "a\nb".chars().collect();
        let now = [line(0..1, 100), line(2..3, 200)];

        let dirty = damage(Some(&previous), &chars, &now, None, None, None, SURFACE).unwrap();
        assert!(dirty.y <= 300, "the removed line's row must be repainted");
        assert_eq!(dirty.y as i32 + dirty.height as i32, SURFACE.height as i32);
    }

    #[test]
    fn moving_the_caret_alone_still_repaints_both_positions() {
        let chars: Vec<char> = "abc".chars().collect();
        let lines = [line(0..3, 100)];
        let was = Rect {
            x: 10,
            y: 100,
            width: 2,
            height: 40,
        };
        let now = Rect {
            x: 90,
            y: 100,
            width: 2,
            height: 40,
        };
        let previous = frame("abc", vec![line(0..3, 100)], Some(was));

        let dirty = damage(
            Some(&previous),
            &chars,
            &lines,
            Some(now),
            None,
            None,
            SURFACE,
        )
        .unwrap();
        assert!(dirty.y <= 100 && dirty.y + dirty.height >= 140, "{dirty:?}");
    }

    fn navigable(text: &str) -> (Vec<char>, Vec<karyll_core::markdown::LineMarkup>) {
        let chars: Vec<char> = text.chars().collect();
        let markup = karyll_core::markdown::analyze(&chars);
        (chars, markup)
    }

    fn para(range: std::ops::Range<usize>, markup: usize, y: i32) -> VisualLine {
        let focus = 0..range.end - range.start;
        VisualLine {
            range,
            markup,
            block: Block::Paragraph,
            y,
            px: 34.0,
            height: 34,
            inset: 4,
            baseline: 30,
            focus,
            underline: 0..0,
        }
    }

    #[test]
    fn the_cursor_belongs_to_the_line_that_starts_at_it() {
        // After a soft wrap the cursor sits between two lines; it belongs to
        // the later one, where the next character will appear.
        let lines = [line(0..5, 100), line(5..10, 140)];
        assert_eq!(line_of(&lines, 5), Some(1));
        assert_eq!(line_of(&lines, 3), Some(0));
        // The very end of the document is inside no half-open range.
        assert_eq!(line_of(&lines, 10), Some(1));
    }

    #[test]
    fn arrowing_down_keeps_the_column() {
        let (chars, markup) = navigable("abcdef\nghijkl");
        let theme = Theme::default();
        let page = Page::new(
            &chars,
            &markup,
            &theme,
            (SURFACE.width, SURFACE.height),
            SURFACE.height,
        );
        let frame = Frame {
            chars: chars.clone(),
            lines: vec![para(0..6, 0, 0), para(7..13, 1, 40)],
            caret: None,
            selection: None,
            candidates: None,
        };

        let (cursor, goal) = move_vertical(&page, &mut Stub, &frame, 3, 1, None).unwrap();
        assert_eq!(cursor, 10, "same column, next line");
        let (back, _) = move_vertical(&page, &mut Stub, &frame, cursor, -1, Some(goal)).unwrap();
        assert_eq!(back, 3, "and back up returns to where it started");
    }

    #[test]
    fn a_short_line_does_not_drag_the_column_left_permanently() {
        // Passing through a line shorter than the held column must not reset it.
        let (chars, markup) = navigable("abcdef\nx\nabcdef");
        let theme = Theme::default();
        let page = Page::new(
            &chars,
            &markup,
            &theme,
            (SURFACE.width, SURFACE.height),
            SURFACE.height,
        );
        let frame = Frame {
            chars: chars.clone(),
            lines: vec![para(0..6, 0, 0), para(7..8, 1, 40), para(9..15, 2, 80)],
            caret: None,
            selection: None,
            candidates: None,
        };

        let (middle, goal) = move_vertical(&page, &mut Stub, &frame, 5, 1, None).unwrap();
        assert_eq!(middle, 8, "clamped to the end of the short line");
        let (bottom, _) = move_vertical(&page, &mut Stub, &frame, middle, 1, Some(goal)).unwrap();
        assert_eq!(bottom, 14, "the held column comes back on the long line");
    }

    #[test]
    fn moving_past_the_ends_holds_still_rather_than_wrapping() {
        let (chars, markup) = navigable("abc");
        let theme = Theme::default();
        let page = Page::new(
            &chars,
            &markup,
            &theme,
            (SURFACE.width, SURFACE.height),
            SURFACE.height,
        );
        let frame = Frame {
            chars: chars.clone(),
            lines: vec![para(0..3, 0, 0)],
            caret: None,
            selection: None,
            candidates: None,
        };

        assert_eq!(
            move_vertical(&page, &mut Stub, &frame, 2, -1, None)
                .unwrap()
                .0,
            2
        );
        assert_eq!(
            move_vertical(&page, &mut Stub, &frame, 2, 1, None)
                .unwrap()
                .0,
            2
        );
    }

    /// The text area of a page, for the scrolling tests.
    const FOLLOW: Scroll = Scroll::Follow {
        top: 100,
        bottom: 1000,
    };

    #[test]
    fn writing_normally_lets_the_cursor_run_to_the_foot_of_the_page() {
        // A page that slides as soon as the cursor passes 40% down is half a
        // focus mode. Nothing may move while the row is still on the page.
        let lines = vec![
            para(0..6, 0, 100),
            para(7..13, 1, 500),
            para(14..20, 2, 900),
        ];
        assert_eq!(scroll_for(&lines, 15, 0, FOLLOW), 0);
    }

    #[test]
    fn a_row_past_the_foot_of_the_page_drags_it_up_by_just_enough() {
        // 1040 + 34 high against a foot at 1000 — 74 too far, and no more.
        let lines = vec![para(0..6, 0, 100), para(7..13, 1, 1040)];
        assert_eq!(scroll_for(&lines, 9, 0, FOLLOW), 74);
    }

    #[test]
    fn a_page_already_scrolled_stays_where_it_is_while_the_cursor_is_visible() {
        // Standing still is what makes this an editor rather than a focus
        // mode: only leaving the page moves it.
        let lines = vec![para(0..6, 0, 500), para(7..13, 1, 900)];
        assert_eq!(scroll_for(&lines, 9, 300, FOLLOW), 300);
    }

    #[test]
    fn a_cursor_above_the_page_pulls_it_back_down_but_never_past_the_start() {
        let lines = vec![para(0..6, 0, 100), para(7..13, 1, 900)];
        assert_eq!(scroll_for(&lines, 9, 700, FOLLOW), 700);
        // Back to the first row, which sits above the scrolled window.
        assert_eq!(scroll_for(&lines, 2, 700, FOLLOW), 0);
    }

    const CENTRE: Scroll = Scroll::Centre {
        top: 100,
        bottom: 1000,
    };

    /// A row with only part of it in focus, for the centring tests. The middle
    /// of the page is 550 and rows here are 34 tall.
    fn focused(range: std::ops::Range<usize>, markup: usize, y: i32) -> VisualLine {
        let mut line = para(range, markup, y);
        line.focus = 0..1;
        line
    }

    fn unfocused(range: std::ops::Range<usize>, markup: usize, y: i32) -> VisualLine {
        let mut line = para(range, markup, y);
        line.focus = 0..0;
        line
    }

    #[test]
    fn focus_mode_centres_the_first_sentence_by_pushing_blank_paper_above_it() {
        // The whole difference between a typewriter pin and focus mode: a clamp
        // at zero leaves the opening sentence at the top while every later one
        // sits at the centre.
        let lines = vec![focused(0..6, 0, 100), unfocused(7..13, 1, 134)];
        // The row spans 100..134, middle 117, against a page middle of 550.
        assert_eq!(scroll_for(&lines, 3, 0, CENTRE), -433);
    }

    #[test]
    fn focus_mode_centres_the_sentence_rather_than_the_caret_row() {
        // Measured off an iA Writer capture: a sentence wrapped across two
        // rows put the caret half a line below the middle and the midpoint of
        // the two rows on it. Centring the caret row would answer 4467 here.
        let lines = vec![
            unfocused(0..6, 0, 100),
            focused(7..13, 1, 4900),
            focused(14..20, 2, 4934),
        ];
        // The sentence spans 4900..4968, midpoint 4934, less the middle 550.
        assert_eq!(scroll_for(&lines, 15, 0, CENTRE), 4384);
    }

    #[test]
    fn focus_mode_ignores_where_the_page_happened_to_be() {
        let lines = vec![unfocused(0..6, 0, 100), focused(7..13, 1, 5000)];
        assert_eq!(scroll_for(&lines, 9, 0, CENTRE), 4467);
        assert_eq!(scroll_for(&lines, 9, 999, CENTRE), 4467);
    }

    #[test]
    fn a_sentence_taller_than_the_page_keeps_the_caret_on_it_instead() {
        // Centring a sentence longer than the page would scroll the caret off
        // the bottom, which is worse than not centring.
        let mut lines: Vec<VisualLine> = (0..40)
            .map(|i| focused(i * 2..i * 2 + 2, i, 100 + i as i32 * 34))
            .collect();
        lines.push(unfocused(80..82, 40, 1460));
        // The sentence spans 100..1460, far taller than the 900 of page. The
        // caret is on the last of its rows, 1426..1460, whose middle is 1443.
        assert_eq!(scroll_for(&lines, 78, 0, CENTRE), 1443 - 550);
        // Which is the point of the guard: centring the sentence would answer
        // 230, and the caret row would then be drawn below the foot of the
        // page rather than on it.
        let centred = (100 + 1460) / 2 - 550;
        assert!(1426 - centred > 1000, "the caret would be off the page");
    }

    #[test]
    fn centring_measures_the_panel_and_following_measures_the_text_column() {
        // The panel is 1860x2480 and the strip 120 tall, both logged rather
        // than guessed, so landscape is 1860 of panel and 1740 of page.
        let panel = 1860;
        let strip_top = panel - crate::ui::STRIP_H as i32;
        assert_eq!(strip_top, 1740);
        assert_eq!(
            scroll_mode(false, false, false, 160, strip_top, panel),
            Scroll::Follow {
                top: 160,
                bottom: 1740
            }
        );
        // Centring against (160 + 1740) / 2 put the sentence at 950 rather
        // than 930, half a top margin low; against 1740 alone it moved by half
        // the strip whenever the chrome came back.
        assert_eq!(
            scroll_mode(true, false, false, 160, strip_top, panel),
            Scroll::Centre {
                top: 0,
                bottom: 1860
            }
        );
        // A jump centres by the same measure and for its own reason: following
        // would pin a search hit to whichever edge it arrived by, so the writer
        // sees everything before their match and nothing after it.
        assert_eq!(
            scroll_mode(false, true, false, 160, strip_top, panel),
            scroll_mode(true, false, false, 160, strip_top, panel)
        );
    }

    /// Landing outranks both, because it is the one paint where the writer has
    /// said where they want to be.
    #[test]
    fn landing_on_a_section_beats_focus_and_beats_a_search_hit() {
        let panel = 1860;
        let strip_top = panel - crate::ui::STRIP_H as i32;
        assert_eq!(
            scroll_mode(true, true, true, 160, strip_top, panel),
            Scroll::Top { top: 160 }
        );
    }

    /// `Follow` moves the page as little as it can, so a jump forwards lands
    /// the destination on the **last** line, with the whole section the writer
    /// asked for above the fold.
    #[test]
    fn a_section_chosen_from_the_outline_arrives_at_the_top() {
        // A page of rows, and a jump to one far down it from a page still at
        // the top of the document.
        let rows = || -> Vec<VisualLine> {
            (0..40)
                .map(|i| para(i * 10..i * 10 + 6, i, 160 + i as i32 * 60))
                .collect()
        };
        let lines = rows();
        let heading = 30 * 10;
        let top = Scroll::Top { top: 160 };
        let landed = scroll_for(&lines, heading, 0, top);
        // The heading's row is exactly at the top margin afterwards.
        assert_eq!(landed, 160 + 30 * 60 - 160);
        let mut shifted = rows();
        shift(&mut shifted, landed);
        assert_eq!(shifted[30].y, 160);
        // Following would have put it at the foot of the page instead, with
        // every word of the section below the fold.
        let followed = scroll_for(
            &lines,
            heading,
            0,
            Scroll::Follow {
                top: 160,
                bottom: 1740,
            },
        );
        let mut under_follow = rows();
        shift(&mut under_follow, followed);
        assert!(
            under_follow[30].y > 1740 - 100,
            "which is the whole point of the mode"
        );
    }

    #[test]
    fn landing_on_the_first_heading_does_not_push_blank_paper_above_it() {
        let lines = vec![para(0..6, 0, 160), para(7..13, 1, 220)];
        assert_eq!(scroll_for(&lines, 0, 0, Scroll::Top { top: 160 }), 0);
        // And from further down the document, it still comes back to zero.
        assert_eq!(scroll_for(&lines, 0, 900, Scroll::Top { top: 160 }), 0);
    }

    #[test]
    fn the_chrome_coming_and_going_does_not_move_the_focused_sentence() {
        // The sentence must sit in the same place whether the strip is there or
        // not: the chrome has nothing to do with what is being written.
        let panel = 1860;
        let with_strip = scroll_mode(true, false, false, 160, panel - 120, panel);
        let without = scroll_mode(true, false, false, 160, panel, panel);
        assert_eq!(with_strip, without);
    }

    #[test]
    fn a_cursor_on_a_blank_line_centres_the_row_it_is_on() {
        // An empty sentence covers no rows at all, so there is nothing to
        // centre but the caret.
        let lines = vec![unfocused(0..1, 0, 900)];
        assert_eq!(scroll_for(&lines, 0, 0, CENTRE), 917 - 550);
    }

    #[test]
    fn shifting_moves_every_row_by_the_same_amount() {
        let mut lines = vec![para(0..6, 0, 100), para(7..13, 1, 500)];
        shift(&mut lines, 400);
        assert_eq!(lines[0].y, -300);
        assert_eq!(lines[1].y, 100);
    }

    #[test]
    fn a_cursor_on_no_line_leaves_the_page_where_it_was() {
        let lines = vec![para(0..6, 0, 100)];
        assert_eq!(scroll_for(&lines, 99, 42, FOLLOW), 42);
    }

    #[test]
    fn a_page_turn_keeps_one_line_of_overlap() {
        let theme = Theme::default();
        let height = 2480u16;
        let fits = (height as f32 / (theme.body_px * theme.leading)) as i32;
        let latin: &[Role] = &[Role::Body];
        assert_eq!(lines_per_page(&mut Stub, &theme, latin, height), fits - 1);
        // Never zero, however small the surface.
        assert!(lines_per_page(&mut Stub, &theme, latin, 1) >= 1);
        // A page with Han on it has taller rows, so fewer of them fit — and
        // paging has to know, or it steps past a line every screen.
        let han: &[Role] = &[Role::Body, Role::Han];
        assert!(lines_per_page(&mut Stub, &theme, han, height) < fits - 1);
    }

    #[test]
    fn per_character_roles_and_styles_line_up_with_the_text() {
        let chars: Vec<char> = "## 第一章 and *more*".chars().collect();
        let markup = karyll_core::markdown::analyze(&chars);
        let line = &markup[0];
        let roles = roles_for_line(line, &chars);
        let styles = styles_for_line(line);
        assert_eq!(roles.len(), chars.len());
        assert_eq!(styles.len(), chars.len());
        // A heading sets everything bold, Han included.
        assert_eq!(roles[3], Role::HanBold);
        assert_eq!(styles[0], Style::Syntax);
    }

    mod selection {
        use super::*;

        fn page_of<'a>(
            chars: &'a [char],
            markup: &'a [karyll_core::markdown::LineMarkup],
            theme: &'a Theme,
        ) -> Page<'a> {
            Page::new(
                chars,
                markup,
                theme,
                (SURFACE.width, SURFACE.height),
                SURFACE.height,
            )
        }

        /// A selection across a soft wrap is the runs it visually is, not one
        /// box spanning the gap between them.
        #[test]
        fn a_selection_gets_one_rectangle_per_visual_line() {
            let (chars, markup) = navigable("one two");
            let theme = Theme::default();
            let page = page_of(&chars, &markup, &theme);
            let lines = vec![para(0..3, 0, 100), para(4..7, 0, 140)];

            let rects = selection_rects(&page, &mut Stub, &lines, &Some(0..7));
            assert_eq!(rects.len(), 2);
            assert_eq!((rects[0].y, rects[1].y), (100, 140));
            // Ten units a character, so the second run is its three characters
            // wide and the first carries a nub for the break as well.
            assert_eq!(rects[1].width, 30);
            assert!(
                rects[0].width > rects[1].width,
                "the line the selection continues past should show the break"
            );
        }

        #[test]
        fn a_selection_inside_one_line_is_one_rectangle_and_no_nub() {
            let (chars, markup) = navigable("one two");
            let theme = Theme::default();
            let page = page_of(&chars, &markup, &theme);
            let lines = vec![para(0..7, 0, 100)];

            let rects = selection_rects(&page, &mut Stub, &lines, &Some(4..7));
            assert_eq!(rects.len(), 1);
            assert_eq!(rects[0].width, 30);
            assert_eq!(rects[0].x, page.left + 40);
        }

        #[test]
        fn nothing_selected_draws_nothing() {
            let (chars, markup) = navigable("one two");
            let theme = Theme::default();
            let page = page_of(&chars, &markup, &theme);
            let lines = vec![para(0..7, 0, 100)];

            assert!(selection_rects(&page, &mut Stub, &lines, &None).is_empty());
            // And a selection nowhere near the lines on screen.
            assert!(selection_rects(&page, &mut Stub, &lines, &Some(90..99)).is_empty());
        }

        #[test]
        fn a_line_scrolled_half_off_the_top_keeps_the_half_still_showing() {
            let (chars, markup) = navigable("one two");
            let theme = Theme::default();
            let page = page_of(&chars, &markup, &theme);
            let lines = vec![para(0..7, 0, -10)];

            let rects = selection_rects(&page, &mut Stub, &lines, &Some(0..7));
            assert_eq!(rects[0].y, 0);
            assert_eq!(rects[0].height, 34 - 10, "clipped, not moved");
        }

        /// The failure this exists to stop: a selection is a filled black run,
        /// so a dropped one that is not repainted leaves a band on the page
        /// with nothing left to remove it.
        #[test]
        fn clearing_a_selection_repaints_where_it_was() {
            let chars: Vec<char> = "abc".chars().collect();
            let lines = vec![line(0..3, 100)];
            let mut previous = frame("abc", vec![line(0..3, 100)], None);
            previous.selection = Some((100, 140));

            let dirty = damage(Some(&previous), &chars, &lines, None, None, None, SURFACE)
                .expect("dropping a selection is damage");
            assert!(dirty.y <= 100 && dirty.y + dirty.height >= 140, "{dirty:?}");
        }

        #[test]
        fn making_a_selection_is_damage_though_no_line_changed() {
            let chars: Vec<char> = "abc".chars().collect();
            let lines = vec![line(0..3, 100)];
            let previous = frame("abc", vec![line(0..3, 100)], None);

            let dirty = damage(
                Some(&previous),
                &chars,
                &lines,
                None,
                Some((100, 140)),
                None,
                SURFACE,
            )
            .expect("a new selection is damage");
            assert!(dirty.y <= 100 && dirty.y + dirty.height >= 140, "{dirty:?}");
        }

        /// Touch selection rests on this: a point on the panel has to name a
        /// character before anything can be selected with a finger.
        #[test]
        fn a_point_on_the_page_names_the_character_nearest_it() {
            let (chars, markup) = navigable("one two");
            let theme = Theme::default();
            let page = page_of(&chars, &markup, &theme);
            let frame = Frame {
                chars: chars.clone(),
                lines: vec![para(0..3, 0, 100), para(4..7, 0, 140)],
                caret: None,
                selection: None,
                candidates: None,
            };
            let left = page.left as f32;

            // Ten units a character, and the caret goes to the nearer side of
            // the glyph the finger landed on.
            assert_eq!(
                index_at_point(&page, &mut Stub, &frame, left + 25.0, 110.0),
                Some(3)
            );
            assert_eq!(
                index_at_point(&page, &mut Stub, &frame, left, 110.0),
                Some(0)
            );
            // The second visual line, by its y alone.
            assert_eq!(
                index_at_point(&page, &mut Stub, &frame, left, 150.0),
                Some(4)
            );
        }

        /// A finger in the margin still meant somewhere, so this answers rather
        /// than declining — otherwise a tap below the last line does nothing at
        /// all, which reads as the app having missed it.
        #[test]
        fn a_point_past_the_text_lands_at_the_near_end_of_it() {
            let (chars, markup) = navigable("one two");
            let theme = Theme::default();
            let page = page_of(&chars, &markup, &theme);
            let frame = Frame {
                chars: chars.clone(),
                lines: vec![para(0..3, 0, 100), para(4..7, 0, 140)],
                caret: None,
                selection: None,
                candidates: None,
            };
            let left = page.left as f32;

            assert_eq!(
                index_at_point(&page, &mut Stub, &frame, left + 9000.0, 9000.0),
                Some(7),
                "below and right of everything is the end"
            );
            assert_eq!(
                index_at_point(&page, &mut Stub, &frame, left, 0.0),
                Some(0),
                "above the first line is its start"
            );
        }

        #[test]
        fn the_span_covers_every_rectangle() {
            let rects = [
                Rect {
                    x: 0,
                    y: 100,
                    width: 10,
                    height: 34,
                },
                Rect {
                    x: 0,
                    y: 140,
                    width: 10,
                    height: 34,
                },
            ];
            assert_eq!(selection_span(&rects), Some((100, 174)));
            assert_eq!(selection_span(&[]), None);
        }
    }
}
