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

/// The least white space either side of the text column.
///
/// Only ever reached by the largest sizes on the narrow edge of the panel: the
/// column they ask for is wider than a portrait page, and a page with no margin
/// is one whose descenders touch the bezel.
const SIDE_MARGIN: u16 = 70;

/// Page geometry and type sizes, in pixels.
pub struct Theme {
    pub body_px: f32,
    /// Width of the text column, before it is fitted to the surface.
    ///
    /// A measure, centred, rather than full-bleed: long lines are harder to
    /// read and there is no reason to use the full 1860 px.
    pub measure: u16,
    pub margin_y: u16,
    /// Multiplier on the face's own line height.
    pub leading: f32,
    /// Extra space above a heading, as a multiple of the body line height.
    pub heading_space: f32,
}

impl Default for Theme {
    fn default() -> Self {
        Self::at(DEFAULT_SIZE)
    }
}

impl Theme {
    /// The page set at `body_px`.
    ///
    /// **The measure follows the type**, and has to: a fixed 1280 px column
    /// held ~58 Latin characters at 46 px, which is where a line should be —
    /// but the same column is ~78 characters at 34 px and ~42 at 64, one past
    /// each end of the comfortable 45–75. Scaling the column with the size
    /// keeps the line length where it belongs and turns this into a control
    /// over how much page a writer sees rather than one that quietly ruins the
    /// setting of it. [`Page::new`] caps it to what the surface can hold.
    ///
    /// The margin and the leading follow too. Large type on a tight margin
    /// reads as a page that is too full, and it wants proportionally *less*
    /// leading than small type does.
    pub fn at(body_px: f32) -> Self {
        let scale = body_px / DEFAULT_SIZE;
        Self {
            body_px,
            measure: (1280.0 * scale) as u16,
            margin_y: (160.0 * scale) as u16,
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

/// How wide the caret is drawn, at a given type size.
///
/// Scaled rather than fixed, so it stays the same weight against the text
/// whatever `body_px` becomes. At 46 px that is 6 px — about 0.5 mm on a 300 ppi
/// panel. 2 px is 0.17 mm and reads as a hairline. The floor keeps it visible
/// if the type is ever set very small.
///
/// A bar rather than a block: this editor is always inserting and the caret sits
/// *between* two characters, so a block would sit on top of the one after it.
fn caret_width(px: f32) -> u16 {
    (px / 8.0).round().max(3.0) as u16
}

/// The text column on a surface `width` wide: where it starts, and how wide.
///
/// **Fitted to the surface, not taken from the theme.** The measure scales with
/// the type size, so the largest sizes ask for a column wider than a portrait
/// page — unfitted that gives `left = 0` and lines running under both bezels.
///
/// One statement of the geometry, because two things need it: the page draws
/// inside the column, and [`edge_at`] reads the margins either side of it.
pub fn column(theme: &Theme, width: u16) -> (u16, u16) {
    let measure = theme.measure.min(width.saturating_sub(SIDE_MARGIN * 2));
    (width.saturating_sub(measure) / 2, measure)
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
        window.fill(rect, BLACK);
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

    /// **The line length is what the size control is really setting.** A fixed
    /// column would make the smallest size ~78 characters and the largest ~42,
    /// one past each end of the comfortable 45–75, so the column moves with the
    /// type and the writer gets more or less page rather than a worse-set one.
    #[test]
    fn the_column_follows_the_type_so_the_line_stays_readable() {
        // Measured off the column the page will actually wrap to, which is the
        // only number that can be wrong: the theme's own measure scales exactly
        // with the size and so is 58 characters by construction, but the cap
        // takes width back at the top of the ladder on the narrow edge.
        let chars: Vec<char> = "a".chars().collect();
        let markup = karyll_core::markdown::analyze(&chars);
        for surface in [(1860u16, 2480u16), (2480, 1860)] {
            for px in SIZES {
                let theme = Theme::at(px);
                let page = Page::new(&chars, &markup, &theme, surface, surface.1 - 120);
                // 1280 px at 46 px is ~58 Latin characters; everything else is
                // that ratio.
                let per_line = 58.0 * (page.measure as f32 / 1280.0) * (46.0 / px);
                assert!(
                    (45.0..=75.0).contains(&per_line),
                    "{px} px on {surface:?} sets {per_line:.0} characters to the line"
                );
            }
        }
    }

    /// The largest sizes ask for a column wider than a portrait page. Unfitted,
    /// `left` saturates to zero and the lines run under both bezels.
    #[test]
    fn the_column_is_capped_to_what_the_surface_holds() {
        let chars: Vec<char> = "a".chars().collect();
        let markup = karyll_core::markdown::analyze(&chars);
        for px in SIZES {
            let theme = Theme::at(px);
            let page = Page::new(&chars, &markup, &theme, (1860, 2480), 2360);
            assert!(page.measure + 2 * SIDE_MARGIN <= 1860, "{px} px overflows");
            assert!(page.left >= SIDE_MARGIN, "{px} px leaves no margin");
            assert!(page.measure > 0);
        }
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
        // ~300 ppi, so 1 pt is about 4.17 px. The old 34 px was 8.2 pt —
        // footnote size on a page nearly as large as A4 — and it also pushed the
        // line to ~78 characters, past the comfortable 45–75.
        let pt = theme.body_px / 4.17;
        assert!((10.0..=12.0).contains(&pt), "body is {pt} pt");
        // The measure is fixed, so a larger body is what shortens the line.
        let latin_advance = theme.body_px * 0.5;
        let chars_per_line = theme.measure as f32 / latin_advance;
        assert!(
            (45.0..=75.0).contains(&chars_per_line),
            "{chars_per_line} characters per line"
        );
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

}
