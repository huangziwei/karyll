//! On-screen UI, in `sidle/native`'s idiom.
//!
//! Its conventions, not invented ones — they are proven on this hardware and
//! there is no reason for karyll to look or behave differently:
//!
//! - **Geometry comes from the font**, not from magic pixel constants. Rows are
//!   `(line_height * 2).max(96)`, the title sits `line_height * 3` down. A
//!   larger face therefore gives larger targets rather than a broken layout.
//! - **A full-width strip along the bottom** carries the actions. In a panel it
//!   is `[ Done ]`; in the editor it is the way to Files and Config.
//! - **Pure hit-testing over a measured layout**, so it is testable against a
//!   stub metric without a screen or the device's faces. [`Layout::row_at`]
//!   answers the vertical axis; [`hit`] adds the horizontal one, which a page
//!   of settings needs because its controls sit side by side.
//!
//! Coordinates arriving here are already in window space — [`crate::orientation`]
//! has been applied, the way sidle applies it before its own corner and row
//! tests.

use anyhow::Result;
use karyll_core::script::{Role, chrome_role_for, script_of};

use crate::font::{Fonts, Metrics};
use crate::window::{BLACK, QUIET, Rect, WHITE, Window};

/// Bottom action strip, matching sidle's generous button row.
pub const STRIP_H: u16 = 120;
/// Left inset for titles and row labels.
pub const MARGIN_X: u16 = 60;

pub const TITLE_PX: f32 = 50.0;
pub const TEXT_PX: f32 = 38.0;

/// Vertical geometry, derived from the face actually in use.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Layout {
    pub line_height: u16,
    pub title_top: u16,
    pub status_top: u16,
    pub rows_top: u16,
    pub row_h: u16,
    pub strip_top: u16,
}

impl Layout {
    /// `text_lh` and `title_lh` are the line heights of the faces actually
    /// drawn, so nothing has to be guessed from the body size — deriving the
    /// spacing from the wrong face is what made the title and the status line
    /// overlap.
    pub fn compute(text_lh: u16, title_lh: u16, height: u16) -> Self {
        let lh = text_lh.max(1);
        let title_lh = title_lh.max(1);
        let title_top = title_lh / 2;
        let status_top = title_top + title_lh;
        Layout {
            line_height: lh,
            title_top,
            status_top,
            // A clear line of air between the status and the first row, so the
            // list reads as a separate thing from the heading.
            rows_top: status_top + lh * 2,
            // Generous tap targets — a 96 px floor whatever the font size.
            row_h: (lh * 2).max(96),
            strip_top: height.saturating_sub(STRIP_H),
        }
    }

    /// How many rows fit between the top of the list and the strip.
    ///
    /// **The panel does not scroll**, so a list longer than this has to be
    /// paged — and the caller can only page it if it can ask how much fits.
    /// Without it [`paint_items`] stops drawing at the foot of the page and
    /// everything past that is invisible and unreachable: on a landscape panel,
    /// the seventeenth document onwards.
    pub fn capacity(&self) -> usize {
        (self.strip_top.saturating_sub(self.rows_top) / self.row_h.max(1)) as usize
    }

    /// Which row a tap at `y` fell on, or `None` above the list.
    ///
    /// The strip is the caller's business: it spans the full bottom width and
    /// is checked before this.
    pub fn row_at(&self, y: u16, rows: usize) -> Option<usize> {
        if y >= self.strip_top || y < self.rows_top {
            return None;
        }
        let row = ((y - self.rows_top) / self.row_h) as usize;
        (row < rows).then_some(row)
    }

    /// The list region, for repainting a selection change without touching the
    /// rest of the screen.
    pub fn rows_rect(&self, width: u16) -> Rect {
        Rect {
            x: 0,
            y: self.rows_top,
            width,
            height: self.strip_top.saturating_sub(self.rows_top),
        }
    }

    pub fn strip_rect(&self, width: u16) -> Rect {
        Rect {
            x: 0,
            y: self.strip_top,
            width,
            height: STRIP_H,
        }
    }
}

/// One line of a panel.
///
/// **A settings page is not a list**, and building it out of one is what made
/// the first Config screen eleven identical rules with no shape to it: five
/// languages, four faces and a keyboard, all the same weight, each hiding its
/// choices behind a tap that cycled them one at a time. This panel is 1860 px
/// across on a 10.2″ panel — there is room to show every option at once and say
/// what belongs with what, and no reason to make a writer tap three times to
/// see three faces.
pub enum Item {
    /// A section heading, with a rule under it. Not tappable: it names what
    /// follows rather than doing anything.
    Heading(String),
    /// A tappable line with something to say about itself: a name on the left,
    /// what is worth knowing about it in the detail column, and whether it is
    /// the one in use. The Files panel is a list of these.
    ///
    /// **The detail sits in the same column the chips do**, so a page of files
    /// and a page of settings are laid out to one grid rather than two.
    Row {
        label: String,
        detail: String,
        /// Drawn bold — the current one, marked the way this page marks any
        /// current thing, and without spending a column on saying so.
        on: bool,
        /// One chip of its own, pinned to the right margin.
        ///
        /// **For the destructive one, and it is right-aligned because of
        /// that.** Tapping a row opens it, so a control that removes it wants
        /// to be as far from the name as the row is wide — a thumb reaching for
        /// a filename on a 10.2″ page is nowhere near the other edge of it.
        action: Option<String>,
    },
    /// A setting: a label, and the values it can take, drawn side by side.
    /// Each value is its own tap target.
    Choice {
        label: String,
        options: Vec<String>,
        /// One per option, and the reason this is not a single index: the
        /// language row is a set with several on, the type rows are a single
        /// pick, and a control that navigates has none.
        on: Vec<bool>,
        /// Options that say where the row stands rather than offering to change
        /// it. Drawn in [`QUIET`] and not tappable — the word stays so the rows
        /// read as one column, and the grey is what says it is not a control.
        ///
        /// Short is fine and empty is the common case: an option past the end
        /// of this is live, the way one past the end of `on` is off.
        inert: Vec<bool>,
    },
    /// A colour setting: the same row, with the values drawn *as* themselves.
    ///
    /// A chip reading "Yellow" says what it is; a yellow circle shows it, and
    /// on the one panel this row appears on there is a colour to show. iA
    /// Writer's picker is a line of filled circles and this is the same line.
    Swatches {
        label: String,
        /// One [`crate::window::ink`] index per swatch, in the order drawn.
        inks: Vec<u8>,
        on: Vec<bool>,
    },
}

/// Which line of a panel the keyboard is on, and which of that line's chips.
///
/// **Page-relative, like everything else that draws**: the row indexes the
/// items handed to the paint rather than the whole list, so a focus mark cannot
/// land a page away from the row it belongs to.
///
/// `chip` means nothing on a row that has none, and is left at zero there.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Focus {
    pub row: usize,
    pub chip: usize,
}

/// The chips of `item` a press can actually take, in the order they are drawn.
///
/// A row's own action chip is not among them: it is reached by the key that
/// names what it does rather than by arrowing onto it, which keeps a
/// destructive control off the path between one setting and the next.
pub fn takeable(item: &Item) -> Vec<usize> {
    match item {
        Item::Choice { options, inert, .. } => (0..options.len())
            .filter(|o| !inert.get(*o).copied().unwrap_or(false))
            .collect(),
        Item::Swatches { inks, .. } => (0..inks.len()).collect(),
        Item::Heading(_) | Item::Row { .. } => Vec::new(),
    }
}

/// The chip `item` is currently on, when it is on one.
pub fn current(item: &Item) -> Option<usize> {
    let on = match item {
        Item::Choice { on, .. } | Item::Swatches { on, .. } => on,
        Item::Heading(_) | Item::Row { .. } => return None,
    };
    on.iter().position(|set| *set)
}

/// A full-screen panel: a title, a status line, a list, and a bottom strip.
pub struct Panel<'a> {
    pub title: &'a str,
    pub status: &'a str,
    pub items: &'a [Item],
    /// Buttons along the bottom, left to right. Owned strings because some of
    /// them say what was typed rather than a fixed word — the find bar's field
    /// and its count.
    pub strip: &'a [String],
    /// What the IME is offering, if a word is being composed into this panel.
    ///
    /// A panel takes text too — a filename is typed on one — so it needs the
    /// same box the page has. Empty whenever nothing is being composed, which
    /// is every panel but Naming and most of the time on that one.
    pub overlay: Overlay<'a>,
    /// Which line the keyboard is on, when a keyboard is being used on it.
    pub focus: Option<Focus>,
}

impl Panel<'_> {
    pub fn paint(&self, window: &mut Window, fonts: &mut Fonts, layout: Layout) -> Result<()> {
        let full = window.full();
        window.fill(full, WHITE);

        draw_line(
            window,
            fonts,
            self.title,
            MARGIN_X,
            layout.title_top as i32,
            TITLE_PX,
            true,
            BLACK,
        );
        if !self.status.is_empty() {
            draw_line(
                window,
                fonts,
                self.status,
                MARGIN_X,
                layout.status_top as i32,
                TEXT_PX,
                false,
                BLACK,
            );
        }
        paint_items(window, fonts, layout, self.items, self.focus);
        paint_strip(window, fonts, layout, self.strip);
        // Last, over everything, and hung off the status line — which on the
        // Naming panel *is* the field, the same way the caret is the field on
        // the page. The empty body below it is where the box lands.
        let labels = self.overlay.labels();
        let anchor = Rect {
            x: MARGIN_X,
            y: layout.status_top,
            width: 0,
            height: layout.line_height,
        };
        if let Some(rect) = overlay_rect(
            window.width(),
            fonts,
            anchor,
            TEXT_PX,
            layout.strip_top,
            &labels,
        ) {
            draw_overlay(window, fonts, rect, TEXT_PX, &labels);
        }
        window.present(full)
    }
}

/// Where a row's own text starts, inside the margin.
pub const ROW_INSET: u16 = MARGIN_X + 24;

/// How wide the bar marking the line the keyboard is on.
const FOCUS_BAR: u16 = 8;

/// The gap between a chip and the ring marking it, and how thick that ring is.
///
/// Outside the chip rather than inside it, because a chip has two states of its
/// own already — filled when it is what the setting is on, outlined when it is
/// merely available — and a mark drawn *in* one of them would have to compete
/// with both. The row's own bar says which line; this says which chip on it.
const RING_GAP: u16 = 5;
const RING: u16 = 3;

/// Blank space either side of a chip's text.
const CHIP_PAD: u16 = 24;
/// Between one chip and the next.
const CHIP_GAP: u16 = 20;

/// Where the second column of *every* row starts — the chips, the swatches and
/// a row's detail alike.
///
/// One column across the whole panel, from the widest label on it, so the
/// values line up down the page instead of stepping in and out behind labels
/// of different lengths — which is most of what makes a settings page read as
/// a table rather than as a pile.
///
/// **Measured against the panel it is drawn on, not against a fixed fraction of
/// it.** The column is pulled back far enough that the widest thing in the
/// second column — a detail, a run of chips, a row of swatches — still finishes
/// inside the right margin, so a page is given whatever width its own content
/// leaves it. The same list therefore lays itself out differently on a 1272 px
/// portrait panel and a 1696 px landscape one, which on Help is the difference
/// between two columns and one column written over another.
///
/// **Between a third and half of the line.** Past half, a page has stopped
/// being a table and the label gives way instead — [`elided`] cuts it, which
/// matters for the labels karyll does not choose: a Bluetooth keyboard is named
/// by whoever made it and a document by whoever wrote it. The floor is the same
/// rule from the other side: one long detail may not squeeze every label on the
/// page.
pub fn chip_column(items: &[Item], width: u16, mut measure_text: impl FnMut(&str) -> u16) -> u16 {
    let widest = items
        .iter()
        .filter_map(|item| match item {
            Item::Choice { label, .. } | Item::Row { label, .. } | Item::Swatches { label, .. } => {
                Some(measure_text(label))
            }
            // A heading is not a label; it starts at the margin and owns its
            // whole line, so however long it is it moves nothing.
            Item::Heading(_) => None,
        })
        .max()
        .unwrap_or(0);
    let wanted = ROW_INSET
        .saturating_add(widest)
        .saturating_add(CHIP_GAP * 3);
    let room = items
        .iter()
        .map(|item| second_column_room(item, width, &mut measure_text))
        .min()
        .unwrap_or(u16::MAX);
    wanted.min(room.max(width / 3).min(width / 2))
}

/// The furthest right this row's second column may start and still finish
/// inside the right margin.
///
/// [`u16::MAX`] for a row with no second column at all: it constrains nothing,
/// and a page of them lets the labels have the whole band.
fn second_column_room(item: &Item, width: u16, measure_text: &mut impl FnMut(&str) -> u16) -> u16 {
    let right = width.saturating_sub(MARGIN_X);
    match item {
        Item::Heading(_) => u16::MAX,
        Item::Row { detail, action, .. } => {
            if detail.is_empty() {
                return u16::MAX;
            }
            // The action chip holds the right margin whatever the row is
            // called, so the detail's room ends a gap short of it.
            let right = match action {
                Some(label) => right
                    .saturating_sub(action_width(label, measure_text))
                    .saturating_sub(CHIP_GAP),
                None => right,
            };
            right.saturating_sub(measure_text(detail))
        }
        Item::Choice { options, .. } => {
            right.saturating_sub(run_width(chip_widths(options, measure_text)))
        }
        Item::Swatches { inks, .. } => {
            right.saturating_sub(run_width(std::iter::repeat_n(SWATCH_W, inks.len())))
        }
    }
}

/// How wide each chip of a row is: its own text, and the blank either side.
fn chip_widths(options: &[String], measure_text: &mut impl FnMut(&str) -> u16) -> Vec<u16> {
    options
        .iter()
        .map(|option| measure_text(option).saturating_add(CHIP_PAD * 2))
        .collect()
}

/// How wide a row of cells is once tiled, the gaps between them included.
///
/// The one place that arithmetic lives, because [`chip_column`] has to know
/// what [`chip_bounds`] and [`swatch_bounds`] are about to lay out: a second
/// copy of it would leave room for a run of a different length than the one
/// drawn.
fn run_width(widths: impl IntoIterator<Item = u16>) -> u16 {
    let mut total = 0u16;
    let mut cells = 0u16;
    for w in widths {
        total = total.saturating_add(w);
        cells = cells.saturating_add(1);
    }
    total.saturating_add(CHIP_GAP.saturating_mul(cells.saturating_sub(1)))
}

/// How much of its line a row's label may be drawn on before it runs into its
/// own value — a gap short of it, so the two columns read as two even on the
/// panel that only just has room for both.
///
/// **Drawing and press feedback both come through here**, for the reason
/// [`chip_bounds`] is shared between drawing and hit-testing: a label cut at
/// rest and drawn whole under the finger would change length as it is touched.
pub fn label_room(
    item: &Item,
    column: u16,
    width: u16,
    measure_text: &mut impl FnMut(&str) -> u16,
) -> u16 {
    let right = width.saturating_sub(MARGIN_X);
    let gap = |start: u16| start.saturating_sub(ROW_INSET + CHIP_GAP);
    match item {
        // Not a label: it starts at the margin and owns its whole line.
        Item::Heading(_) => right.saturating_sub(MARGIN_X),
        Item::Row { detail, action, .. } => {
            if !detail.is_empty() {
                return gap(column);
            }
            // Nothing in the second column: the whole line, up to its own
            // action chip.
            match action {
                Some(text) => right
                    .saturating_sub(action_width(text, measure_text))
                    .saturating_sub(CHIP_GAP),
                None => right,
            }
            .saturating_sub(ROW_INSET)
        }
        // Its own chips rather than the column, because a row that could not
        // afford the column keeps them just past its label.
        Item::Choice { label, options, .. } => {
            gap(chip_bounds(column, width, label, options, measure_text)
                .first()
                .map_or(column, |(x, _)| *x))
        }
        Item::Swatches { .. } => gap(column),
    }
}

/// `text`, cut to `room` with an ellipsis, or whole when it fits.
///
/// **What a label runs into is its own value** — a filename over its word
/// count, a keyboard's name over the button that forgets it — so a label too
/// long for its column is cut rather than drawn on top of the thing it names.
/// Nothing else on a panel is drawn past its bounds either: a chip too wide for
/// the margin is dropped, and a candidate bar too wide for the panel is paged.
///
/// Cut by character, so a label in Chinese loses characters rather than bytes.
pub fn elided(text: &str, room: u16, mut measure_text: impl FnMut(&str) -> u16) -> String {
    if measure_text(text) <= room {
        return text.to_string();
    }
    let mark = "…";
    let mut used = measure_text(mark);
    if used > room {
        return String::new();
    }
    let mut kept = String::new();
    for ch in text.chars() {
        let mut buf = [0u8; 4];
        let w = measure_text(ch.encode_utf8(&mut buf));
        if used.saturating_add(w) > room {
            break;
        }
        used += w;
        kept.push(ch);
    }
    // A space before the mark reads as a gap rather than as a cut.
    while kept.ends_with(' ') {
        kept.pop();
    }
    kept.push_str(mark);
    kept
}

/// Where each chip of one choice row sits, in window x.
///
/// **The single source for drawing, hit-testing and press feedback**, for the
/// reason [`cell_bounds`] is: a chip is only as wide as its own text, so
/// anything that measured them a second time would put a finger on a different
/// one. A chip that runs past the right margin is dropped rather than shrunk,
/// the rule the candidate bar follows in [`candidate_pages`].
///
/// **The shared column is a courtesy, and a row that cannot afford it keeps its
/// own.** [`chip_column`] leaves room for the widest run on the page, so this
/// is the case where a row is not that one and still cannot fit — the column
/// having been floored at a third of the line to keep the labels readable. Such
/// a row starts its chips just past its own label instead, which on a short
/// label like `Size` is a long way further left. It steps that row out of the
/// table, and that is the trade the page is laid out under: a control that is
/// not on the page is worse than a row that is not in line with its neighbours.
pub fn chip_bounds(
    column: u16,
    width: u16,
    label: &str,
    options: &[String],
    mut measure_text: impl FnMut(&str) -> u16,
) -> Vec<(u16, u16)> {
    let right = width.saturating_sub(MARGIN_X);
    let widths = chip_widths(options, &mut measure_text);
    let wanted = run_width(widths.iter().copied());
    // Its own label only when that is further left than the column: a row with
    // the longest label on the page is the one the column was measured from,
    // and starting after it again would gain nothing.
    let mut x = if column.saturating_add(wanted) <= right {
        column
    } else {
        (ROW_INSET + measure_text(label) + CHIP_GAP).min(column)
    };
    let mut out = Vec::new();
    for w in widths {
        if x.saturating_add(w) > right {
            break;
        }
        out.push((x, w));
        x = x.saturating_add(w + CHIP_GAP);
    }
    out
}

/// A swatch's slot, wide enough to be a thumb's target rather than a dot.
const SWATCH_W: u16 = 72;

/// Where a row of swatches sits, from the same column the chips start at.
///
/// Fixed width rather than measured: a swatch has no text to be as wide as, and
/// six circles of one size read as a set of choices where six of different
/// sizes would read as a ranking.
pub fn swatch_bounds(column: u16, width: u16, count: usize) -> Vec<(u16, u16)> {
    let right = width.saturating_sub(MARGIN_X);
    let mut x = column;
    let mut out = Vec::new();
    for _ in 0..count {
        if x.saturating_add(SWATCH_W) > right {
            break;
        }
        out.push((x, SWATCH_W));
        x = x.saturating_add(SWATCH_W + CHIP_GAP);
    }
    out
}

/// The rectangle of one chip, given the bounds its row was laid out at.
pub fn chip_rect(layout: Layout, item: usize, bounds: &[(u16, u16)], option: usize) -> Rect {
    let (x, width) = bounds.get(option).copied().unwrap_or((0, 0));
    chip_slot(layout, item, x, width)
}

/// Where a row's own action chip sits: the right margin, and the same height
/// and vertical placing as any other chip.
///
/// It is measured from the right rather than laid out from the left, so it
/// holds the same edge whatever the row is called — a column of them lines up
/// down the page, and none of them moves when a document is renamed.
pub fn action_rect(
    layout: Layout,
    item: usize,
    width: u16,
    label: &str,
    mut measure_text: impl FnMut(&str) -> u16,
) -> Rect {
    let w = action_width(label, &mut measure_text);
    let x = width.saturating_sub(MARGIN_X).saturating_sub(w);
    chip_slot(layout, item, x, w)
}

/// How wide an action chip is, which is how much of the right margin it takes
/// away from the detail beside it.
fn action_width(label: &str, measure_text: &mut impl FnMut(&str) -> u16) -> u16 {
    measure_text(label).saturating_add(CHIP_PAD * 2)
}

fn chip_slot(layout: Layout, item: usize, x: u16, width: u16) -> Rect {
    let height = layout.row_h * 3 / 4;
    Rect {
        x,
        y: layout.rows_top + item as u16 * layout.row_h + (layout.row_h - height) / 2,
        width,
        height,
    }
}

/// Whether a point is inside a rectangle.
fn inside(rect: Rect, x: u16, y: u16) -> bool {
    x >= rect.x && x < rect.x + rect.width && y >= rect.y && y < rect.y + rect.height
}

/// What a tap at `(x, y)` in the list landed on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Hit {
    Row(usize),
    /// A chip: which item it is on, and which of its options.
    Option(usize, usize),
}

/// The one function that decides what a tap is on.
///
/// Drawing and dispatch both come through here, because they have to: a second
/// copy of this arithmetic puts the invert on one control and runs another. See
/// [`cell_bounds`].
///
/// `None` for a heading, for a label, and for the space past the last chip.
/// None of them is a control, and a settings page where the gaps do something
/// is a settings page you cannot rest a hand on.
pub fn hit(
    items: &[Item],
    layout: Layout,
    width: u16,
    x: u16,
    y: u16,
    mut measure_text: impl FnMut(&str) -> u16,
) -> Option<Hit> {
    let index = layout.row_at(y, items.len())?;
    match &items[index] {
        Item::Heading(_) => None,
        // The action chip is asked before the row it sits on, the same way the
        // candidate box is asked before the page it covers: a tap on the chip is
        // pressing it, not opening whatever it happens to be pinned to.
        Item::Row { action, .. } => {
            if let Some(label) = action
                && inside(
                    action_rect(layout, index, width, label, &mut measure_text),
                    x,
                    y,
                )
            {
                return Some(Hit::Option(index, 0));
            }
            Some(Hit::Row(index))
        }
        Item::Choice {
            label,
            options,
            inert,
            ..
        } => {
            let column = chip_column(items, width, &mut measure_text);
            let bounds = chip_bounds(column, width, label, options, &mut measure_text);
            cell_at(&bounds, x)
                .filter(|option| !inert.get(*option).copied().unwrap_or(false))
                .map(|option| Hit::Option(index, option))
        }
        Item::Swatches { inks, .. } => {
            let column = chip_column(items, width, &mut measure_text);
            let bounds = swatch_bounds(column, width, inks.len());
            cell_at(&bounds, x).map(|option| Hit::Option(index, option))
        }
    }
}

/// Draw the list. Separated so a selection change can repaint just these.
pub fn paint_items(
    window: &mut Window,
    fonts: &mut Fonts,
    layout: Layout,
    items: &[Item],
    focus: Option<Focus>,
) {
    let width = window.width();
    window.fill(layout.rows_rect(width), WHITE);
    let column = chip_column(items, width, |s| measure(fonts, s, TEXT_PX) as u16);
    // `capacity` rather than a break inside the loop, because that is the
    // number the caller pages by. Two ways of saying where the list stops is
    // two ways for them to disagree, and this one would hide a document.
    for i in 0..items.len().min(layout.capacity()) {
        let chip = focus.filter(|f| f.row == i).map(|f| f.chip);
        draw_item(window, fonts, layout, items, i, column, chip);
    }
}

/// Redraw one line of the list, with whatever focus mark it now carries.
///
/// **The rest of the page is left alone.** Moving the keyboard down a list
/// touches the line it left and the line it arrived on; a panel-wide repaint
/// for a mark that moves on every press is half a second of ink to change two
/// rows of it. Full width, so a ring drawn outside the last chip is inside what
/// this erases.
pub fn paint_focus_row(
    window: &mut Window,
    fonts: &mut Fonts,
    layout: Layout,
    items: &[Item],
    index: usize,
    chip: Option<usize>,
) -> Rect {
    let width = window.width();
    let rect = Rect {
        x: 0,
        width,
        ..row_rect(layout, width, index)
    };
    window.fill(rect, WHITE);
    let column = chip_column(items, width, |s| measure(fonts, s, TEXT_PX) as u16);
    draw_item(window, fonts, layout, items, index, column, chip);
    rect
}

/// One line of the list, and the chip the keyboard is on if it is on this line.
fn draw_item(
    window: &mut Window,
    fonts: &mut Fonts,
    layout: Layout,
    items: &[Item],
    i: usize,
    column: u16,
    focus: Option<usize>,
) {
    let width = window.width();
    let Some(item) = items.get(i) else {
        return;
    };
    let top = layout.rows_top + i as u16 * layout.row_h;
    let middle = top as i32 + (layout.row_h as i32 - TEXT_PX as i32) / 2;
    if focus.is_some() {
        // In the air [`ROW_INSET`] leaves, so the mark sits beside the label
        // rather than pushing it along: a line that shifts when it takes focus
        // is a list that shuffles as you walk down it.
        let height = layout.row_h / 2;
        window.fill(
            Rect {
                x: MARGIN_X,
                y: top + (layout.row_h - height) / 2,
                width: FOCUS_BAR,
                height,
            },
            BLACK,
        );
    }
    match item {
        // The text sits at the foot of its row and the rule directly under
        // it, so the empty half above reads as the gap between sections.
        // Rows are a uniform height — `row_at` divides by one number — so
        // the air has to come from inside the row rather than from a taller
        // one.
        Item::Heading(text) => {
            let baseline = top + layout.row_h.saturating_sub(TEXT_PX as u16 + 20);
            draw_line(
                window,
                fonts,
                text,
                MARGIN_X,
                baseline as i32,
                TEXT_PX,
                true,
                BLACK,
            );
            window.fill(
                Rect {
                    x: MARGIN_X,
                    y: top + layout.row_h - 3,
                    width: width - MARGIN_X * 2,
                    height: 3,
                },
                BLACK,
            );
        }
        // No rule of its own: a rule above every entry is a stack of
        // identical bars saying nothing. What separates one line from the
        // next is the detail beside it, which is the part worth reading.
        Item::Row {
            label,
            detail,
            on,
            action,
        } => {
            let room = label_room(item, column, width, &mut |s| {
                measure(fonts, s, TEXT_PX) as u16
            });
            let label = elided(label, room, |s| measure(fonts, s, TEXT_PX) as u16);
            draw_line(
                window, fonts, &label, ROW_INSET, middle, TEXT_PX, *on, BLACK,
            );
            if !detail.is_empty() {
                draw_line(window, fonts, detail, column, middle, TEXT_PX, false, BLACK);
            }
            if let Some(text) = action {
                let rect = action_rect(layout, i, width, text, |s| {
                    measure(fonts, s, TEXT_PX) as u16
                });
                // Never filled at rest. A chip that removes something must
                // not look like the marked, current, on thing — filled is
                // what this page says "yes, this one" with.
                draw_chip(window, fonts, rect, text, ChipState::default());
            }
        }
        // No rule: the chips are visibly bounded already, and a line under
        // every setting buries the structure of the page.
        Item::Choice {
            label,
            options,
            on,
            inert,
        } => {
            let bounds = chip_bounds(column, width, label, options, |s| {
                measure(fonts, s, TEXT_PX) as u16
            });
            let room = label_room(item, column, width, &mut |s| {
                measure(fonts, s, TEXT_PX) as u16
            });
            let label = elided(label, room, |s| measure(fonts, s, TEXT_PX) as u16);
            draw_line(
                window, fonts, &label, ROW_INSET, middle, TEXT_PX, false, BLACK,
            );
            for (o, _) in bounds.iter().enumerate() {
                let rect = chip_rect(layout, i, &bounds, o);
                draw_chip(
                    window,
                    fonts,
                    rect,
                    &options[o],
                    ChipState {
                        on: on.get(o).copied().unwrap_or(false),
                        inert: inert.get(o).copied().unwrap_or(false),
                        focused: focus == Some(o),
                        ..ChipState::default()
                    },
                );
            }
        }
        Item::Swatches { label, inks, on } => {
            let room = label_room(item, column, width, &mut |s| {
                measure(fonts, s, TEXT_PX) as u16
            });
            let label = elided(label, room, |s| measure(fonts, s, TEXT_PX) as u16);
            draw_line(
                window, fonts, &label, ROW_INSET, middle, TEXT_PX, false, BLACK,
            );
            let bounds = swatch_bounds(column, width, inks.len());
            for (o, _) in bounds.iter().enumerate() {
                let rect = chip_rect(layout, i, &bounds, o);
                draw_swatch(window, rect, inks[o], on.get(o).copied().unwrap_or(false));
                if focus == Some(o) {
                    draw_ring(window, rect);
                }
            }
        }
    }
}

/// The ring around the chip the keyboard is on.
fn draw_ring(window: &mut Window, rect: Rect) {
    let outer = Rect {
        x: rect.x.saturating_sub(RING_GAP + RING),
        y: rect.y.saturating_sub(RING_GAP + RING),
        width: rect.width + 2 * (RING_GAP + RING),
        height: rect.height + 2 * (RING_GAP + RING),
    };
    for edge in [
        Rect {
            height: RING,
            ..outer
        },
        Rect {
            y: outer.y + outer.height - RING,
            height: RING,
            ..outer
        },
        Rect {
            width: RING,
            ..outer
        },
        Rect {
            x: outer.x + outer.width - RING,
            width: RING,
            ..outer
        },
    ] {
        window.fill(edge, BLACK);
    }
}

/// One swatch: a filled circle, with a hole punched in the chosen one.
///
/// **Not the page's filled-versus-outlined idiom**, because that idiom spends
/// the fill on saying "this one" and here the fill *is* the value — an
/// unfilled swatch would be a colour setting with no colour in it. iA Writer
/// marks its own picker with a dot in the middle and so does this, which
/// survives the one thing that could take the colour away: on a panel showing
/// these in grey, the mark is still a mark.
fn draw_swatch(window: &mut Window, rect: Rect, ink: u8, on: bool) {
    let radius = rect.width.min(rect.height) / 2;
    let cx = rect.x + rect.width / 2;
    let cy = rect.y + rect.height / 2;
    disc(window, cx, cy, radius, ink);
    if on {
        disc(window, cx, cy, radius / 3, WHITE);
    }
}

/// A filled circle, scanline by scanline.
///
/// Shared with the page, which marks Han emphasis with one.
///
/// Coverage is one bit everywhere else on this panel and it is one bit here:
/// the edge is hard, which at this size reads as a circle and not as a
/// staircase.
pub fn disc(window: &mut Window, cx: u16, cy: u16, radius: u16, value: u8) {
    let r = radius as i32;
    for dy in -r..=r {
        let half = ((r * r - dy * dy) as f32).sqrt() as i32;
        let y = cy as i32 + dy;
        let x = cx as i32 - half;
        if y < 0 || x < 0 {
            continue;
        }
        window.fill(
            Rect {
                x: x as u16,
                y: y as u16,
                width: (half * 2) as u16,
                height: 1,
            },
            value,
        );
    }
}

/// What a chip is, and what is happening to it. Every one of these is off by
/// default: a chip that says nothing about itself is one the setting is not on,
/// nothing is touching, and the keyboard is elsewhere.
#[derive(Debug, Clone, Copy, Default)]
struct ChipState {
    /// What the setting is currently on.
    on: bool,
    /// Held under a finger.
    pressed: bool,
    /// Saying where the row stands rather than offering to change it.
    inert: bool,
    /// Where the keyboard is.
    focused: bool,
}

/// One chip: filled when it is what the setting is currently on, outlined when
/// it is merely available.
///
/// **Filled, not ticked.** The renderer cuts coverage to one bit, so a tick or
/// a grey wash is a smudge at this size; an inverted block is unambiguous
/// across the room, and it is the idiom the strip already uses for a press.
fn draw_chip(window: &mut Window, fonts: &mut Fonts, rect: Rect, label: &str, state: ChipState) {
    if state.focused {
        draw_ring(window, rect);
    }
    // A press inverts whatever the chip already was, so the feedback reads the
    // same on a chip that is on as on one that is off.
    let filled = state.on != state.pressed;
    let (ground, ink) = if filled {
        (BLACK, WHITE)
    } else if state.inert {
        // Border and word both recede. A chip that cannot be pressed drawn in
        // the same black as one that can is the page offering an action it does
        // not have.
        (WHITE, QUIET)
    } else {
        (WHITE, BLACK)
    };
    window.fill(rect, ground);
    if !filled {
        for edge in [
            Rect { height: 2, ..rect },
            Rect {
                y: rect.y + rect.height - 2,
                height: 2,
                ..rect
            },
            Rect { width: 2, ..rect },
            Rect {
                x: rect.x + rect.width - 2,
                width: 2,
                ..rect
            },
        ] {
            window.fill(edge, ink);
        }
    }
    let w = measure(fonts, label, TEXT_PX) as u16;
    let start = rect.x + rect.width.saturating_sub(w) / 2;
    let top = rect.y as i32 + (rect.height as i32 - TEXT_PX as i32) / 2 - 4;
    draw_line(window, fonts, label, start, top, TEXT_PX, false, ink);
}

/// Redraw one chip, inverted while held.
pub fn paint_chip(
    window: &mut Window,
    fonts: &mut Fonts,
    layout: Layout,
    items: &[Item],
    item: usize,
    option: usize,
    pressed: bool,
) -> Rect {
    let width = window.width();
    match items.get(item) {
        // A row's own action chip, which [`hit`] reports as option zero.
        Some(Item::Row {
            action: Some(label),
            ..
        }) => {
            let rect = action_rect(layout, item, width, label, |s| {
                measure(fonts, s, TEXT_PX) as u16
            });
            draw_chip(
                window,
                fonts,
                rect,
                label,
                ChipState {
                    pressed,
                    ..ChipState::default()
                },
            );
            rect
        }
        Some(Item::Choice {
            label,
            options,
            on,
            inert,
        }) => {
            let column = chip_column(items, width, |s| measure(fonts, s, TEXT_PX) as u16);
            let bounds = chip_bounds(column, width, label, options, |s| {
                measure(fonts, s, TEXT_PX) as u16
            });
            let rect = chip_rect(layout, item, &bounds, option);
            let Some(label) = options.get(option) else {
                return rect;
            };
            draw_chip(
                window,
                fonts,
                rect,
                label,
                ChipState {
                    on: on.get(option).copied().unwrap_or(false),
                    pressed,
                    inert: inert.get(option).copied().unwrap_or(false),
                    ..ChipState::default()
                },
            );
            rect
        }
        // A press marks the swatch the way a chosen one is marked, so the
        // feedback reads the same on the current colour as on any other.
        Some(Item::Swatches { inks, on, .. }) => {
            let column = chip_column(items, width, |s| measure(fonts, s, TEXT_PX) as u16);
            let bounds = swatch_bounds(column, width, inks.len());
            let rect = chip_rect(layout, item, &bounds, option);
            let Some(ink) = inks.get(option) else {
                return rect;
            };
            draw_swatch(
                window,
                rect,
                *ink,
                on.get(option).copied().unwrap_or(false) != pressed,
            );
            rect
        }
        _ => Rect::default(),
    }
}

/// Draw a panel's bottom strip: a rule, then its buttons packed from the left.
///
/// A panel has no status line — its title and its own status row say what this
/// screen is, and repeating a word count under a list of files would be
/// answering a question nobody asked there.
pub fn paint_strip(window: &mut Window, fonts: &mut Fonts, layout: Layout, cells: &[String]) {
    let cells: Vec<String> = cells.iter().map(|label| format!("[ {label} ]")).collect();
    paint_cells(window, fonts, layout, &cells, &[], "");
}

/// The least blank a cell may keep either side of its text.
///
/// **Padding is what a strip gives up before it gives up a control.** Six
/// cells at [`CELL_PAD`] spend 312 px on air, which on a 10.2″ panel is what
/// makes the bar look deliberate and on a 7″ one is the difference between
/// `[ Done ]` being on the strip and not being anywhere. A cramped button is
/// still a button.
const CELL_PAD_MIN: u16 = 8;

/// Blank space either side of a cell's text.
const CELL_PAD: u16 = 26;

/// Where each cell starts and how wide it is, in window x.
///
/// **Every cell is its own text's width**, packed from the left. Three short
/// labels stretched across a ten-inch panel look accidental, and chrome should
/// take only the room it needs — the same argument that hides it while writing.
/// Even division leaves `[ Exit ]` 620 px wide in landscape.
///
/// `stretch` names the cells that absorb whatever is left over, for the bar
/// that has a *field* on it rather than only buttons: the find bar's query grows
/// as it is typed into, so packing it like a label would shove the buttons along
/// under the writer's finger and eventually push `[ Done ]` off the end. Giving
/// it the slack instead holds every button still. Empty leaves the remainder
/// unclaimed, which is what the status line fills.
///
/// **Several of them share the slack equally**: the replace bar has two fields
/// and a writer comparing `colour` with `color` has to see both. Equal shares
/// and not shares by content, or a field would move its neighbour as it was
/// typed into.
///
/// **The single source for drawing, hit-testing and press feedback**, and it has
/// to be, because cells are *dropped* when the width runs out: the number drawn
/// is not `cells.len()`, so anything that divided the width for itself would
/// disagree about which cell a finger is on.
///
/// `measure_text` supplies the drawn width of a label, the way `wrap` takes its
/// metric: the packing arithmetic is then testable against a stub, which matters
/// because the device's faces do not exist on a development machine and this is
/// exactly the arithmetic that failed there.
pub fn cell_bounds(
    width: u16,
    cells: &[String],
    stretch: &[usize],
    mut measure_text: impl FnMut(&str) -> u16,
) -> Vec<(u16, u16)> {
    if cells.is_empty() {
        return Vec::new();
    }
    // The fixed cells first, because the elastic ones are defined as what they
    // leave — asking one how wide it wants to be would be circular, since its
    // text is trimmed to fit the room it is given.
    let text: u16 = cells
        .iter()
        .enumerate()
        .filter(|(i, _)| !stretch.contains(i))
        .map(|(_, label)| measure_text(label))
        .fold(0, u16::saturating_add);
    let pad = cell_pad(width, text, cells.len(), stretch.len());
    let fixed: u16 = cells
        .iter()
        .enumerate()
        .filter(|(i, _)| !stretch.contains(i))
        .map(|(_, label)| fitted(label, pad, &mut measure_text))
        .sum();
    let each = share(width, fixed, stretch.len());

    let mut out: Vec<(u16, u16)> = Vec::new();
    let mut x = 0u16;
    for (i, label) in cells.iter().enumerate() {
        let w = if stretch.contains(&i) {
            each
        } else {
            fitted(label, pad, &mut measure_text)
        };
        if x.saturating_add(w) > width {
            break;
        }
        out.push((x, w));
        x = x.saturating_add(w);
    }
    // One cell wider than the whole strip is still drawn, clipped, rather than
    // leaving the bar blank with something in progress on it.
    if out.is_empty() {
        out.push((0, width));
    }
    out
}

/// One cell's width: its text, and the blank either side of it.
fn fitted(label: &str, pad: u16, measure_text: &mut impl FnMut(&str) -> u16) -> u16 {
    measure_text(label).saturating_add(pad * 2)
}

/// The blank this strip can afford either side of each cell.
///
/// [`CELL_PAD`] whenever the words fit with it, and as little as
/// [`CELL_PAD_MIN`] when they do not — the whole strip tightening together, so
/// that it still reads as one bar rather than as cells of two kinds.
///
/// **`text` is the fixed cells' text and nothing else.** A stretch cell counts
/// towards `cells`, because it needs padding like any other, but its words are
/// trimmed to the room left over — so measuring what it currently says would
/// make the padding depend on what has been typed into it, and the two places
/// that ask for it are on opposite sides of that trimming. What each of them
/// wants instead is [`FIELD_MIN`], claimed here before any of the slack is
/// spent on air.
fn cell_pad(width: u16, text: u16, cells: usize, elastic: usize) -> u16 {
    let wanted = text.saturating_add(FIELD_MIN * elastic as u16);
    let each = width.saturating_sub(wanted) / (2 * cells.max(1)) as u16;
    each.clamp(CELL_PAD_MIN, CELL_PAD)
}

/// The least text a stretch cell is worth giving: eight Latin characters at
/// [`TEXT_PX`], which is a search query you can still read. Below that the
/// field is present and useless, which is a worse answer than a tighter bar.
pub const FIELD_MIN: u16 = (TEXT_PX * 8.0 / 2.0) as u16;

/// What one elastic cell gets of the room the fixed ones leave.
///
/// Zero when nothing is elastic, so the remainder falls to the status line.
fn share(width: u16, fixed: u16, elastic: usize) -> u16 {
    if elastic == 0 {
        return 0;
    }
    width.saturating_sub(fixed) / elastic as u16
}

/// How much *text* each stretch cell will have room for, given what the others
/// say.
///
/// The stretch cells' own labels are deliberately not among `others` and could
/// not be: their text is trimmed to fit the room they are given, so asking how
/// wide one wants to be first is circular. This is the same arithmetic
/// [`cell_bounds`] does, said once so the caller that has to trim cannot arrive
/// at a different answer from the layout that will draw it.
pub fn stretch_room(
    width: u16,
    others: &[String],
    elastic: usize,
    mut measure_text: impl FnMut(&str) -> u16,
) -> u16 {
    let text: u16 = others
        .iter()
        .map(|label| measure_text(label))
        .fold(0, u16::saturating_add);
    let pad = cell_pad(width, text, others.len() + elastic, elastic);
    let fixed: u16 = others
        .iter()
        .map(|label| fitted(label, pad, &mut measure_text))
        .sum();
    share(width, fixed, elastic).saturating_sub(pad * 2)
}

/// Where the packed cells end, which is where the rule above them stops and
/// where the status line may start.
pub fn cells_end(bounds: &[(u16, u16)]) -> u16 {
    bounds.last().map_or(0, |(x, w)| x.saturating_add(*w))
}

/// Which cell a tap at `x` fell on, or `None` for space no cell occupies.
///
/// `None` matters for a fitted bar: the strip runs the full width but the
/// candidates may not fill it, and a tap on the empty tail should do nothing
/// rather than commit the last candidate.
pub fn cell_at(bounds: &[(u16, u16)], x: u16) -> Option<usize> {
    bounds
        .iter()
        .position(|(cx, w)| x >= *cx && x < cx.saturating_add(*w))
}

/// Draw cells along the bottom, with the text given.
///
/// The action strip and the find bar are both this, differing only in what
/// their cells say and which of them is elastic. The geometry is written once
/// on purpose: two copies of this arithmetic drift the moment one is adjusted.
///
/// **The rule runs the full width, and there are no rules between the cells.**
/// Dividers make a table of the buttons, and a table's last cell wants a right
/// wall it cannot have, so the row reads as unfinished. They are also a second
/// delimiter for a boundary the brackets already draw.
///
/// The rule went full width with them. It stopped at the last cell on the
/// argument that a few left-packed buttons under a full-width rule read as an
/// empty shelf, and two things are wrong with that: the band is full width
/// because `status` lives in the other end of it, and the section headings on
/// the panel directly above rule the whole width. A strip that stopped short
/// was the one line on screen that did not.
///
/// `status` is right-aligned and quiet — the same band rather than a second one
/// stacked under it, and a thing you look at when you stop rather than while
/// typing.
pub fn paint_cells(
    window: &mut Window,
    fonts: &mut Fonts,
    layout: Layout,
    cells: &[String],
    stretch: &[usize],
    status: &str,
) {
    let width = window.width();
    window.fill(layout.strip_rect(width), WHITE);
    window.fill(
        Rect {
            x: 0,
            y: layout.strip_top,
            width,
            height: 3,
        },
        BLACK,
    );

    let bounds = cell_bounds(width, cells, stretch, |s| measure(fonts, s, TEXT_PX) as u16);
    for (i, (x, cw)) in bounds.iter().enumerate() {
        let label = &cells[i];
        let w = measure(fonts, label, TEXT_PX) as u16;
        let start = x + cw.saturating_sub(w) / 2;
        draw_line(
            window,
            fonts,
            label,
            start,
            cell_text_top(layout),
            TEXT_PX,
            false,
            BLACK,
        );
    }

    paint_status(window, fonts, layout, cells_end(&bounds), status);
}

/// Where a line of strip text sits, so the buttons and the status line share one
/// baseline instead of each finding their own.
fn cell_text_top(layout: Layout) -> i32 {
    layout.strip_top as i32 + (STRIP_H as i32 - TEXT_PX as i32) / 2 - 4
}

/// The status line, in the room the buttons leave.
///
/// **Right-aligned against the far margin**, so it reads as the other end of the
/// band rather than as a fourth button that lost its brackets. Quiet, because it
/// is something to glance at: the buttons are what the finger is looking for.
///
/// Dropped entirely when the buttons have taken the width — a status crushed
/// into forty pixels is worse than none.
fn paint_status(window: &mut Window, fonts: &mut Fonts, layout: Layout, from: u16, status: &str) {
    if status.is_empty() {
        return;
    }
    let room = window
        .width()
        .saturating_sub(from)
        .saturating_sub(MARGIN_X * 2);
    let w = measure(fonts, status, TEXT_PX) as u16;
    if w > room {
        return;
    }
    let x = window.width().saturating_sub(MARGIN_X).saturating_sub(w);
    draw_line(
        window,
        fonts,
        status,
        x,
        cell_text_top(layout),
        TEXT_PX,
        false,
        QUIET,
    );
}

/// The rectangle of one strip cell, given the bounds it was laid out at.
///
/// Takes the bounds rather than recomputing them, so a caller that has already
/// asked [`cell_bounds`] cannot get a second, different answer.
pub fn strip_cell_rect(layout: Layout, bounds: &[(u16, u16)], index: usize) -> Rect {
    let (x, w) = bounds.get(index).copied().unwrap_or((0, 0));
    Rect {
        x,
        y: layout.strip_top + 3,
        width: w,
        height: STRIP_H - 3,
    }
}

/// The rectangle of one list row.
pub fn row_rect(layout: Layout, width: u16, index: usize) -> Rect {
    Rect {
        x: MARGIN_X,
        y: layout.rows_top + index as u16 * layout.row_h + 2,
        width: width - MARGIN_X * 2,
        height: layout.row_h - 2,
    }
}

/// Redraw one strip cell, inverted while held.
///
/// This is the only acknowledgement a tap gets while the finger is still down.
/// Whatever the press does lands later — a panel repaint, a scan starting — so
/// without it a tap that registered looks exactly like one that missed.
pub fn paint_strip_cell(
    window: &mut Window,
    fonts: &mut Fonts,
    layout: Layout,
    cells: &[String],
    index: usize,
    pressed: bool,
    stretch: &[usize],
) -> Rect {
    let width = window.width();
    let bounds = cell_bounds(width, cells, stretch, |s| measure(fonts, s, TEXT_PX) as u16);
    let rect = strip_cell_rect(layout, &bounds, index);
    // A cell with nothing in it is nothing to press. The find bar's count is
    // blank until something has been typed, and inverting it would flash a
    // black block where there is no button.
    if cells.get(index).is_none_or(|text| text.is_empty()) {
        return rect;
    }
    let (ground, ink) = if pressed {
        (BLACK, WHITE)
    } else {
        (WHITE, BLACK)
    };
    window.fill(rect, ground);
    // The label exactly as [`paint_cells`] draws it, brackets included: adding
    // them again reads `[ [ Close ] ]` and jumps wider under the finger. Press
    // feedback must differ from the resting state only by the inversion.
    let Some(text) = cells.get(index) else {
        return rect;
    };
    let w = measure(fonts, text, TEXT_PX) as u16;
    let start = rect.x + rect.width.saturating_sub(w) / 2;
    draw_line(
        window,
        fonts,
        text,
        start,
        cell_text_top(layout),
        TEXT_PX,
        false,
        ink,
    );
    rect
}

/// Redraw one list row, inverted while held.
pub fn paint_row(
    window: &mut Window,
    fonts: &mut Fonts,
    layout: Layout,
    items: &[Item],
    index: usize,
    pressed: bool,
) -> Rect {
    let width = window.width();
    let rect = row_rect(layout, width, index);
    let (ground, ink) = if pressed {
        (BLACK, WHITE)
    } else {
        (WHITE, BLACK)
    };
    window.fill(rect, ground);
    let baseline = rect.y as i32 + (layout.row_h as i32 - TEXT_PX as i32) / 2 - 2;
    if let Some(
        item @ Item::Row {
            label,
            detail,
            on,
            action,
        },
    ) = items.get(index)
    {
        let column = chip_column(items, width, |s| measure(fonts, s, TEXT_PX) as u16);
        let room = label_room(item, column, width, &mut |s| {
            measure(fonts, s, TEXT_PX) as u16
        });
        let label = elided(label, room, |s| measure(fonts, s, TEXT_PX) as u16);
        draw_line(
            window, fonts, &label, ROW_INSET, baseline, TEXT_PX, *on, ink,
        );
        if !detail.is_empty() {
            draw_line(window, fonts, detail, column, baseline, TEXT_PX, false, ink);
        }
        // Drawn at rest even while the row around it is inverted, which is the
        // point: it says the chip is not the thing being pressed. Filling over
        // it instead would flash the whole row black including a control the
        // finger deliberately missed.
        if let Some(text) = action {
            let chip = action_rect(layout, index, width, text, |s| {
                measure(fonts, s, TEXT_PX) as u16
            });
            draw_chip(window, fonts, chip, text, ChipState::default());
        }
    }
    rect
}

pub fn measure(fonts: &mut Fonts, text: &str, px: f32) -> f32 {
    text.chars()
        .map(|c| fonts.advance(chrome_role_for(false, script_of(c)), px, c))
        .sum()
}

/// Draw one line of text with its top edge at `y`.
#[allow(clippy::too_many_arguments)]
pub fn draw_line(
    window: &mut Window,
    fonts: &mut Fonts,
    text: &str,
    x: u16,
    y: i32,
    px: f32,
    bold: bool,
    ink: u8,
) {
    let mut pen = x as f32;
    // The baseline sits low enough for the faces this label uses. A Han label —
    // the language button, a candidate, a Chinese filename — is taller than the
    // Latin face's ascent, and that ascent would draw it above its own row.
    let roles: Vec<Role> = text
        .chars()
        .map(|ch| chrome_role_for(bold, script_of(ch)))
        .collect();
    let baseline = y as f32 + fonts.ascent(px, &roles);
    for ch in text.chars() {
        let role = chrome_role_for(bold, script_of(ch));
        let origin = pen;
        fonts.draw(role, px, ch, |gx, gy, coverage| {
            if coverage <= 0.5 {
                return;
            }
            let px_x = origin as i32 + gx;
            let px_y = baseline as i32 + gy;
            if px_x >= 0 && px_y >= 0 {
                window.put_pixel(px_x as u16, px_y as u16, ink);
            }
        });
        pen += fonts.advance(role, px, ch);
    }
}

/// Where the candidate box goes, or `None` with nothing to choose from and no
/// room to put it.
///
/// It is drawn against the text being composed rather than in the action strip
/// at the foot of the page. Every desktop IME does it this way, and the reason
/// is the eye: you are writing in the middle of a ten-inch page, and choosing a
/// character should not mean looking 1740 px away and back. It also keeps
/// composing from dragging the auto-hidden chrome back on screen for every word.
///
/// **`anchor` is whatever is being typed into** — the caret on the page, the
/// find bar's field on the strip, the status line a filename goes into. A
/// rectangle and a type size rather than a page, because the box is a widget
/// and not a feature of the document: composing happens in the panels too, and
/// a writer who can only name files in Latin has the same gap this box exists
/// to close. `bottom` is the last row it may occupy.
///
/// Separate from the drawing because the damage rectangle has to know where the
/// box will be *before* anything is painted — including on the paths where
/// nothing is painted at all.
pub fn overlay_rect(
    surface_width: u16,
    fonts: &mut impl Metrics,
    anchor: Rect,
    body_px: f32,
    bottom: u16,
    labels: &[String],
) -> Option<Rect> {
    if labels.is_empty() {
        return None;
    }
    let px = body_px * CANDIDATE_SCALE;
    let (pad_x, pad_y) = padding(px);
    let gap = (px * 0.25) as u16;

    // **Each cell is its label plus the same padding on both sides**, and the
    // box is exactly the cells. An earlier version added the padding once per
    // label *and* once more to the total, which left `pad/2` at the left edge
    // and one and a half at the right — the box hugged its text on one side and
    // not the other.
    let cells: Vec<u16> = labels
        .iter()
        .map(|label| label_width(fonts, label, px) + pad_x * 2)
        .collect();
    let width = cells.iter().sum::<u16>().min(surface_width);
    // The tallest label's own glyph box, not a leaded row: leading is the space
    // between lines of prose and there is only one line here. Using it put all
    // of the extra space below the text and stood the label on the box's top
    // edge — the same mistake the page itself made before half-leading.
    let text_box = labels
        .iter()
        .map(|label| glyph_box(fonts, label, px))
        .fold(0, u16::max);
    let height = text_box + pad_y * 2;

    // Below the anchor when it fits and above it otherwise — composing on the
    // last line of a page must not put the choices off the bottom of it, and
    // the find bar sits on the strip, where there is no below at all.
    let below = anchor.y as i32 + anchor.height as i32 + gap as i32;
    let y = if below + height as i32 <= bottom as i32 {
        below
    } else {
        anchor.y as i32 - gap as i32 - height as i32
    };
    if y < 0 {
        return None;
    }
    // Pulled left by however much it overhangs, so the last candidate is never
    // cut off by the edge of the panel.
    let x = anchor.x.min(surface_width.saturating_sub(width));
    Some(Rect {
        x,
        y: y as u16,
        width,
        height,
    })
}

/// Space inside the box, horizontally and vertically.
///
/// Wider than it is tall, which is how a label in a box reads as deliberate
/// rather than cramped; equal padding on all four sides looks loose at the top
/// and bottom for a single line of text.
fn padding(px: f32) -> (u16, u16) {
    (
        (px * 0.30).round() as u16,
        (px * 0.20).round().max(2.0) as u16,
    )
}

/// The height of a label's own glyph box, from the faces it will be drawn with.
fn glyph_box(fonts: &mut impl Metrics, label: &str, px: f32) -> u16 {
    fonts.line_height(px, &label_roles(label)) as u16
}

fn label_roles(label: &str) -> Vec<Role> {
    label
        .chars()
        .map(|c| chrome_role_for(false, script_of(c)))
        .collect()
}

/// How wide a label draws. The same sum `ui::measure` does, but against
/// `Metrics` rather than the concrete faces, so the geometry above can be
/// checked on the host — which is the only place it ever is, the device's faces
/// not existing on a development machine.
pub fn label_width(fonts: &mut impl Metrics, label: &str, px: f32) -> u16 {
    let width: f32 = label
        .chars()
        .map(|c| fonts.advance(chrome_role_for(false, script_of(c)), px, c))
        .sum();
    width.round() as u16
}

/// Where each label sits inside the box, in absolute window x.
///
/// The one source for drawing and for the tap test, because they must agree
/// about which cell a finger is on.
pub fn overlay_cells(
    fonts: &mut impl Metrics,
    rect: Rect,
    body_px: f32,
    labels: &[String],
) -> Vec<(u16, u16)> {
    let px = body_px * CANDIDATE_SCALE;
    let (pad_x, _) = padding(px);
    let mut out = Vec::with_capacity(labels.len());
    let mut x = rect.x;
    for label in labels {
        let w = label_width(fonts, label, px) + pad_x * 2;
        out.push((x, w));
        x += w;
    }
    out
}

/// Where each page of candidates starts, given what the panel can hold.
///
/// **Ten is what the number row can pick, not what the panel can show.** The
/// box is drawn beside the caret at the writer's own type size, so ten
/// four-character phrases want more width than a 7″ panel has — and more than a
/// 10.2″ one has at the larger body sizes. What happened then was that
/// [`overlay_rect`] clamped the box to the panel and [`overlay_cells`] went on
/// laying candidates out past its edge: the last of them were drawn off the
/// screen while the number row went on selecting them. **Committing something
/// you cannot see is worse than not being offered it**, and paging is the way
/// out, because the writer can already reach the next page with an arrow.
///
/// So a page is as many as fit, at most `most`. The starts are worked out for
/// the whole list at once and kept, because the page a candidate is on is the
/// one thing drawing, tapping and the number row all have to agree about — the
/// rule [`cell_bounds`] follows for the same reason.
///
/// Empty for no candidates, and otherwise never empty: a candidate wider than
/// the whole panel gets a page to itself rather than no page at all.
pub fn candidate_pages(
    fonts: &mut impl Metrics,
    surface_width: u16,
    body_px: f32,
    candidates: &[String],
    most: usize,
) -> Vec<usize> {
    let px = body_px * CANDIDATE_SCALE;
    let (pad_x, _) = padding(px);
    let mut starts = Vec::new();
    let (mut start, mut used) = (0usize, 0u16);
    for (i, text) in candidates.iter().enumerate() {
        // Measured with a number in front, because that is what gets drawn.
        // Any digit is as wide as any other, so the tenth's `0` stands for all
        // of them.
        let cell = label_width(fonts, &format!("0 {text}"), px) + pad_x * 2;
        if i > start && (used.saturating_add(cell) > surface_width || i - start == most) {
            starts.push(start);
            (start, used) = (i, 0);
        }
        used = used.saturating_add(cell);
    }
    if !candidates.is_empty() {
        starts.push(start);
    }
    starts
}

pub fn draw_overlay(
    window: &mut Window,
    fonts: &mut Fonts,
    rect: Rect,
    body_px: f32,
    labels: &[String],
) {
    let px = body_px * CANDIDATE_SCALE;
    let (pad_x, _) = padding(px);
    // A filled panel with a rule around it: on one bit there is no shadow and
    // no tint, so the border is the only thing saying this floats above the
    // page rather than belonging to it.
    window.fill(rect, WHITE);
    frame_rect(window, rect, BORDER);

    let cells = overlay_cells(fonts, rect, body_px, labels);
    for (label, (x, _)) in labels.iter().zip(&cells) {
        // Each label centred on its own glyph box rather than dropped at the
        // top: `US` and `简体` have different heights, and one rule that centres
        // whichever it is sits both of them properly in the same box.
        let box_h = glyph_box(fonts, label, px);
        let top = rect.y as i32 + (rect.height as i32 - box_h as i32) / 2;
        draw_line(window, fonts, label, x + pad_x, top, px, false, BLACK);
    }
}

/// Draw a rule around the edge of `rect`.
fn frame_rect(window: &mut Window, rect: Rect, thickness: u16) {
    for i in 0..thickness {
        for x in rect.x..rect.x + rect.width {
            for y in [rect.y + i, rect.y + rect.height - 1 - i] {
                window.put_pixel(x, y, BLACK);
            }
        }
        for y in rect.y..rect.y + rect.height {
            for x in [rect.x + i, rect.x + rect.width - 1 - i] {
                window.put_pixel(x, y, BLACK);
            }
        }
    }
}

/// Candidates are set smaller than the prose: they are a tool rather than part
/// of the writing, and a box of body-sized Han would cover a third of the page.
pub const CANDIDATE_SCALE: f32 = 0.72;

/// Thickness of the candidate box's border.
const BORDER: u16 = 2;
/// The small box that appears next to the caret.
///
/// One slot, because the two things that use it cannot both apply: switching
/// language abandons any composition, so there is never a language to announce
/// *and* a word being converted.
pub enum Overlay<'a> {
    None,
    /// Numbered choices from the IME.
    Candidates(&'a [String]),
    /// A single unnumbered label, for saying which language the keyboard just
    /// became — the strip is hidden while writing and cannot say it.
    Notice(&'a str),
}

impl Overlay<'_> {
    /// The cells to draw, numbered or not.
    ///
    /// Numbering happens here rather than inside the drawing, so that the
    /// widths measured for the box are the widths of the strings actually put
    /// on screen — the same rule the strip already follows.
    pub fn labels(&self) -> Vec<String> {
        match self {
            Self::None => Vec::new(),
            // The tenth is picked with 0, as in every other pinyin IME.
            Self::Candidates(items) => items
                .iter()
                .enumerate()
                .map(|(i, text)| format!("{} {text}", (i + 1) % 10))
                .collect(),
            Self::Notice(text) => vec![(*text).to_string()],
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::font::{Proportional, Stub};

    const HEIGHT: u16 = 2480;
    const WIDTH: u16 = 1860;

    fn layout() -> Layout {
        Layout::compute(44, 58, HEIGHT)
    }

    #[test]
    fn the_title_status_and_rows_never_overlap() {
        // They did: spacing was derived from the body face while the title was
        // drawn much larger, so the status line was painted through it.
        let l = layout();
        assert!(l.status_top >= l.title_top + 58, "status clears the title");
        assert!(l.rows_top >= l.status_top + 44, "rows clear the status");
        assert!(l.rows_top < l.strip_top);
    }

    #[test]
    fn geometry_follows_the_faces_in_use() {
        // A larger face pushes everything down rather than colliding.
        let small = Layout::compute(30, 40, HEIGHT);
        let large = Layout::compute(60, 80, HEIGHT);
        assert!(large.title_top > small.title_top);
        assert!(large.status_top > small.status_top);
        assert!(large.rows_top > small.rows_top);
        assert_eq!(large.row_h, 120, "twice the line height, past the floor");
    }

    #[test]
    fn a_small_face_still_gets_a_reachable_row() {
        // The 96 px floor keeps a modest font from making tap targets a finger
        // cannot hit.
        assert_eq!(Layout::compute(20, 30, HEIGHT).row_h, 96);
        assert_eq!(layout().row_h, 96);
    }

    #[test]
    fn rows_resolve_below_the_title_and_above_the_strip() {
        let l = layout();
        assert_eq!(l.row_at(l.rows_top, 3), Some(0));
        assert_eq!(l.row_at(l.rows_top + l.row_h, 3), Some(1));
        // The title area is not a row.
        assert_eq!(l.row_at(l.rows_top - 1, 3), None);
        // Neither is the strip.
        assert_eq!(l.row_at(l.strip_top, 3), None);
        assert_eq!(l.row_at(l.strip_top + 10, 3), None);
    }

    #[test]
    fn a_tap_past_the_last_row_hits_nothing() {
        let l = layout();
        assert_eq!(l.row_at(l.rows_top + l.row_h * 5, 3), None);
        assert_eq!(l.row_at(l.rows_top + 10, 0), None);
    }

    /// A stub metric, as `wrap`'s tests use: ten pixels a character, so the
    /// arithmetic is checkable by hand.
    fn stub(text: &str) -> u16 {
        text.chars().count() as u16 * 10
    }

    fn strings(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    /// A settings row with `n` values, the first of them the one it is on.
    fn choice(n: usize) -> Item {
        Item::Choice {
            label: "Size".into(),
            options: (0..n).map(|o| o.to_string()).collect(),
            on: (0..n).map(|o| o == 0).collect(),
            inert: Vec::new(),
        }
    }

    /// A row that opens something, as every line of the Files list does.
    fn opener() -> Item {
        Item::Row {
            label: "draft.md".into(),
            detail: "300 words".into(),
            on: false,
            action: Some("Delete".into()),
        }
    }

    /// The chips the arrows walk are the ones a finger could press, and a row's
    /// own action chip is not among them: Delete is reached by the key that
    /// says so, not by arrowing past it on the way to the next document.
    #[test]
    fn the_keyboard_walks_the_chips_a_press_could_take() {
        assert_eq!(takeable(&choice(3)), vec![0, 1, 2]);
        assert!(takeable(&opener()).is_empty());
        assert_eq!(
            takeable(&Item::Choice {
                label: "Keyboard".into(),
                options: strings(&["Connected", "Forget"]),
                on: vec![false, false],
                inert: vec![true, false],
            }),
            vec![1]
        );
        assert_eq!(current(&choice(3)), Some(0));
        assert_eq!(current(&opener()), None);
    }

    /// Buttons take the width their own text needs, packed from the left. Even
    /// division leaves `[ Exit ]` 620 px wide in landscape.
    #[test]
    fn buttons_take_their_own_width_and_leave_the_rest() {
        let cells = strings(&["[ Exit ]", "[ Config ]", "[ Files ]"]);
        let bounds = cell_bounds(WIDTH, &cells, &[], stub);
        assert_eq!(bounds.len(), 3);
        for (i, (x, w)) in bounds.iter().enumerate() {
            assert_eq!(*w, stub(&cells[i]) + CELL_PAD * 2, "cell {i} is its text");
            assert!(x + w <= WIDTH);
        }
        // They tile left to right with no gap and no overlap.
        for pair in bounds.windows(2) {
            assert_eq!(pair[0].0 + pair[0].1, pair[1].0);
        }
        assert_eq!(bounds[0].0, 0, "packed into the left corner");
        assert!(
            cells_end(&bounds) < WIDTH / 2,
            "and leaving the status line most of the band"
        );
    }

    /// The whole point of packing left: the tail is the status line's, and a
    /// tap on a line that only reports must not run the button nearest it.
    #[test]
    fn a_tap_past_the_last_button_hits_nothing() {
        let cells = strings(&["[ Exit ]", "[ Config ]"]);
        let bounds = cell_bounds(WIDTH, &cells, &[], stub);
        assert_eq!(cell_at(&bounds, 5), Some(0));
        assert_eq!(cell_at(&bounds, WIDTH - 1), None);
    }

    /// The find bar's field grows as it is typed into. Packing it like a label
    /// would shove `Previous`, `Next` and `Done` along under the writer's
    /// finger and eventually push them off the strip; giving it the slack
    /// instead holds every button still.
    #[test]
    fn the_stretch_cell_takes_the_slack_and_the_buttons_hold_still() {
        let short = strings(&["[ Find: a_ ]", "[ 1 of 3 ]", "[ Next ]", "[ Done ]"]);
        let long = strings(&[
            "[ Find: a much longer query indeed_ ]",
            "[ 1 of 3 ]",
            "[ Next ]",
            "[ Done ]",
        ]);
        let a = cell_bounds(WIDTH, &short, &[0], stub);
        let b = cell_bounds(WIDTH, &long, &[0], stub);
        assert_eq!(a.len(), 4);
        assert_eq!(a, b, "a longer query moves nothing");
        assert_eq!(
            cells_end(&a),
            WIDTH,
            "and the bar fills the strip, because the field absorbed the rest"
        );
        // The field is what the others left.
        let others: Vec<String> = short[1..].to_vec();
        assert_eq!(
            a[0].1,
            WIDTH - others.iter().map(|s| stub(s) + CELL_PAD * 2).sum::<u16>()
        );
    }

    /// What the field may hold is the same subtraction the layout does, or the
    /// text is trimmed against one width and drawn into another.
    #[test]
    fn the_room_offered_matches_the_cell_given() {
        let others = strings(&["[ 1 of 3 ]", "[ Previous ]", "[ Next ]", "[ Done ]"]);
        let room = stretch_room(WIDTH, &others, 1, stub);
        let mut cells = vec![String::new()];
        cells.extend(others);
        let bounds = cell_bounds(WIDTH, &cells, &[0], stub);
        assert_eq!(
            room,
            bounds[0].1 - CELL_PAD * 2,
            "the cell, less its padding"
        );
    }

    /// Two fields, equal shares: typing into one does not move the other.
    #[test]
    fn two_fields_split_the_slack_between_them() {
        let cells = strings(&[
            "[ Find: colour_ ]",
            "[ With: color ]",
            "[ 1 of 3 ]",
            "[ Change ]",
            "[ All ]",
            "[ Done ]",
        ]);
        let bounds = cell_bounds(WIDTH, &cells, &[0, 1], stub);
        assert_eq!(bounds.len(), 6, "every button survives");
        assert_eq!(bounds[0].1, bounds[1].1, "and the two fields are one size");
        let fixed: u16 = cells[2..].iter().map(|s| stub(s) + CELL_PAD * 2).sum();
        assert_eq!(bounds[0].1, (WIDTH - fixed) / 2);
        // Typing into either one moves nothing: the width is the slack, not the
        // text.
        let mut typed = cells.clone();
        typed[1] = "[ With: color and then some_ ]".into();
        assert_eq!(cell_bounds(WIDTH, &typed, &[0, 1], stub), bounds);
        // And the room offered agrees with the cell given, for each of them.
        let others: Vec<String> = cells[2..].to_vec();
        assert_eq!(
            stretch_room(WIDTH, &others, 2, stub),
            bounds[0].1 - CELL_PAD * 2
        );
    }

    /// Better clipped than blank: a field long enough to fill the strip still
    /// has to show, because what has been typed is nowhere else on screen.
    #[test]
    fn one_over_wide_cell_is_still_drawn() {
        let cells = strings(&["a very long composition indeed"]);
        let bounds = cell_bounds(50, &cells, &[], stub);
        assert_eq!(bounds.len(), 1);
        assert_eq!(bounds[0], (0, 50));
    }

    #[test]
    fn an_empty_strip_has_no_cells() {
        assert!(cell_bounds(WIDTH, &[], &[], stub).is_empty());
        assert!(cell_bounds(WIDTH, &[], &[0], stub).is_empty());
        assert_eq!(cell_at(&[], 10), None);
        assert_eq!(cells_end(&[]), 0);
    }

    /// The panel does not scroll, so what fits is what exists — and a list
    /// longer than this was simply invisible past the last row that did fit,
    /// which on a landscape panel is the seventeenth document onwards.
    #[test]
    fn what_fits_is_what_the_caller_pages_by() {
        let l = layout();
        let fits = l.capacity();
        assert!(fits > 0);
        // The last row that fits resolves; the first that does not is nothing,
        // which is the same boundary `paint_items` draws to.
        assert_eq!(
            l.row_at(l.rows_top + (fits as u16 - 1) * l.row_h, fits),
            Some(fits - 1)
        );
        assert!(l.rows_top + fits as u16 * l.row_h + l.row_h > l.strip_top);
    }

    #[test]
    fn the_rows_region_stops_short_of_the_strip() {
        // Or a list would paint under the buttons and look tappable when it is
        // not.
        let l = layout();
        let rect = l.rows_rect(WIDTH);
        assert_eq!(rect.y, l.rows_top);
        assert_eq!(rect.y + rect.height, l.strip_top);
    }

    #[test]
    fn a_pressed_cell_covers_its_own_slot_and_no_other() {
        // The invert has to land exactly on the cell under the finger, or a
        // press smears into its neighbour.
        let l = layout();
        let cells = strings(&["[ Exit ]", "[ Config ]", "[ Files ]"]);
        let bounds = cell_bounds(WIDTH, &cells, &[], stub);
        let a = strip_cell_rect(l, &bounds, 0);
        let b = strip_cell_rect(l, &bounds, 1);
        assert_eq!(a.x, 0);
        assert_eq!(a.x + a.width, b.x, "cells abut without overlapping");
        assert!(a.y >= l.strip_top, "and stay inside the strip");
        assert_eq!(a.y + a.height, l.strip_top + STRIP_H);
        assert!(b.width > a.width, "the longer label gets the wider cell");
    }

    /// A row opens what it names, so the chip that removes it is pinned to the
    /// far margin — and has to be asked about *before* the row it sits on, or a
    /// tap meant to delete a document would open it instead.
    #[test]
    fn a_rows_action_chip_is_hit_before_the_row_under_it() {
        let l = layout();
        let items = vec![Item::Row {
            label: "Welcome.md".into(),
            detail: "482 words · just now".into(),
            on: true,
            action: Some("Delete".into()),
        }];
        let middle = l.rows_top + l.row_h / 2;
        let chip = action_rect(l, 0, WIDTH, "Delete", stub);

        assert_eq!(
            hit(&items, l, WIDTH, chip.x + chip.width / 2, middle, stub),
            Some(Hit::Option(0, 0)),
            "the chip is the chip"
        );
        assert_eq!(
            hit(&items, l, WIDTH, ROW_INSET + 10, middle, stub),
            Some(Hit::Row(0)),
            "and the name is still the row"
        );
        // Which is only safe because they are nowhere near each other.
        assert!(
            chip.x > WIDTH / 2,
            "the chip is at the far margin, not beside the name: {chip:?}"
        );
        assert_eq!(chip.x + chip.width, WIDTH - MARGIN_X);
    }

    /// The chip holds one edge whatever the row is called, so a column of them
    /// lines up and none of them moves when a document is renamed.
    #[test]
    fn action_chips_line_up_however_long_the_names_are() {
        let l = layout();
        let short = action_rect(l, 0, WIDTH, "Delete", stub);
        let same = action_rect(l, 3, WIDTH, "Delete", stub);
        assert_eq!(short.x, same.x);
        // And an armed one grows leftward rather than off the edge.
        let armed = action_rect(l, 0, WIDTH, "Delete?", stub);
        assert_eq!(armed.x + armed.width, short.x + short.width);
        assert!(armed.x < short.x);
    }

    /// A row with no action is untouched by any of it: every tap on it is the
    /// row, including one at the far margin where another row's chip would be.
    #[test]
    fn a_row_without_an_action_has_no_dead_corner() {
        let l = layout();
        let items = vec![Item::Row {
            label: "draft.md".into(),
            detail: "12 words · yesterday".into(),
            on: false,
            action: None,
        }];
        let middle = l.rows_top + l.row_h / 2;
        assert_eq!(
            hit(&items, l, WIDTH, WIDTH - MARGIN_X - 10, middle, stub),
            Some(Hit::Row(0))
        );
    }

    #[test]
    fn a_pressed_row_covers_its_own_row_and_no_other() {
        let l = layout();
        let a = row_rect(l, WIDTH, 0);
        let b = row_rect(l, WIDTH, 1);
        assert!(a.y >= l.rows_top);
        assert!(a.y + a.height <= b.y, "rows do not overlap");
        assert_eq!(a.x, MARGIN_X);
    }

    fn settings() -> Vec<Item> {
        vec![
            Item::Heading("Input".into()),
            Item::Choice {
                label: "Languages".into(),
                options: strings(&["EN", "DE", "简体"]),
                on: vec![true, true, false],
                inert: Vec::new(),
            },
            Item::Row {
                label: "draft.md".into(),
                detail: "842 words · yesterday".into(),
                action: None,
                on: false,
            },
            Item::Heading("Type".into()),
            Item::Choice {
                label: "Latin".into(),
                options: strings(&["Ember", "Bookerly"]),
                on: vec![true, false],
                inert: Vec::new(),
            },
        ]
    }

    /// Chips and details line up down the page rather than stepping in and out
    /// behind labels of different lengths, which is most of what makes it read
    /// as a page and not a pile. **One column for both kinds of line**, so a
    /// list of files and a page of settings are laid out to the same grid.
    #[test]
    fn every_line_starts_its_second_column_in_the_same_place() {
        let items = settings();
        let column = chip_column(&items, WIDTH, stub);
        assert!(column > ROW_INSET + stub("Languages"), "clears the widest");
        assert!(column > ROW_INSET + stub("Latin"));
        assert!(column > ROW_INSET + stub("draft.md"), "rows count too");

        // A heading is not a label — it starts at the margin and owns its whole
        // line, so however long it is it moves nothing.
        let mut longer = settings();
        longer.push(Item::Heading("A Very Long Section Heading Indeed".into()));
        assert_eq!(chip_column(&longer, WIDTH, stub), column);

        // A longer label does move it, and everything with it.
        longer.push(Item::Row {
            label: "a-rather-longer-filename.md".into(),
            detail: "1 word · just now".into(),
            action: None,
            on: false,
        });
        assert!(chip_column(&longer, WIDTH, stub) > column);
    }

    /// **The same list is laid out differently on different panels.** A wide one
    /// has room to put the column where the labels want it; a narrow one has to
    /// pull it back until the second column finishes inside the right margin.
    /// Nothing about the list changes — only how much of the panel is left.
    #[test]
    fn a_column_is_placed_from_what_the_panel_has_left() {
        let items = vec![Item::Row {
            label: "Press at one end of the run and lift at the other".into(),
            detail: "Places the cursor where you tapped, and never writes a mark on it.".into(),
            action: None,
            on: false,
        }];
        let wide = chip_column(&items, WIDE, stub);
        let narrow = chip_column(&items, NARROW, stub);
        assert!(
            narrow < wide,
            "the narrow panel took no less room: {narrow}"
        );
        for (panel, column) in [(WIDE, wide), (NARROW, narrow)] {
            let Item::Row { label, detail, .. } = &items[0] else {
                unreachable!()
            };
            assert!(
                column + stub(detail) <= panel - MARGIN_X,
                "{panel} px panel: the detail runs off the right margin"
            );
            let room = label_room(&items[0], column, panel, &mut stub);
            let drawn = elided(label, room, stub);
            assert!(
                ROW_INSET + stub(&drawn) < column,
                "{panel} px panel: {drawn:?} runs under its own detail"
            );
        }
    }

    /// A run of chips is what a settings row has instead of a detail, and it
    /// binds the column the same way: the seven type sizes are the widest run
    /// karyll draws, and on the narrow panel they are what decides where the
    /// labels stop.
    #[test]
    fn a_row_of_chips_leaves_the_column_as_much_room_as_a_detail_does() {
        let sizes: Vec<String> = crate::render::SIZES
            .iter()
            .map(|px| format!("{px:.0}"))
            .collect();
        let items = vec![Item::Choice {
            label: "A Long Bluetooth Keyboard Name".into(),
            options: sizes.clone(),
            on: vec![],
            inert: Vec::new(),
        }];
        for panel in [WIDE, NARROW] {
            let column = chip_column(&items, panel, stub);
            let bounds = chip_bounds(column, panel, "Size", &sizes, stub);
            assert_eq!(
                bounds.len(),
                sizes.len(),
                "{panel} px panel: a size fell off"
            );
            let (x, w) = bounds[bounds.len() - 1];
            assert!(
                x + w <= panel - MARGIN_X,
                "{panel} px panel: 80 is past the margin"
            );
        }
    }

    /// **A label is cut rather than drawn over its own value.** Nothing else on
    /// a panel is drawn past its bounds either: a chip too wide for the margin
    /// is dropped and a candidate bar too wide for the panel is paged.
    #[test]
    fn a_label_too_long_for_its_room_is_cut_at_the_mark() {
        assert_eq!(elided("draft.md", 200, stub), "draft.md", "it fits, whole");
        // Ten pixels a character, the mark included: five characters and the
        // mark are 60, so 65 keeps five.
        assert_eq!(elided("a-long-filename.md", 65, stub), "a-lon…");
        // Cut by character, so a Chinese name loses characters and not bytes.
        assert_eq!(elided("第一章的草稿", 45, stub), "第一章…");
        // A space before the mark reads as a gap rather than as a cut.
        assert_eq!(elided("Focus on this", 75, stub), "Focus…");
        // Not even the mark fits, and half a mark is worse than none.
        assert_eq!(elided("draft.md", 5, stub), "");
    }

    /// One label on this page is not karyll's to choose — a Bluetooth keyboard
    /// carries whatever name its maker gave it. Unbounded, a long one pushes the
    /// chips past the right margin, `chip_bounds` drops them, and the writer is
    /// left with a keyboard they cannot forget. The name gives way instead.
    #[test]
    fn a_label_nobody_chose_cannot_push_its_own_controls_off_the_page() {
        let items = vec![Item::Choice {
            label: "A Very Long Bluetooth Keyboard Name Indeed, 2024 Edition".into(),
            options: strings(&["Connect", "Forget"]),
            on: vec![false, false],
            inert: Vec::new(),
        }];
        let Item::Choice { label, options, .. } = &items[0] else {
            unreachable!()
        };
        for panel in [WIDE, NARROW] {
            let column = chip_column(&items, panel, stub);
            assert!(column <= panel / 2, "the column ran away with the label");
            let bounds = chip_bounds(column, panel, label, options, stub);
            assert_eq!(bounds.len(), 2, "{panel} px panel: Forget must still be on");
            let room = label_room(&items[0], column, panel, &mut stub);
            let drawn = elided(label, room, stub);
            assert!(
                ROW_INSET + stub(&drawn) < bounds[0].0,
                "{panel} px panel: {drawn:?} reaches its own Connect chip"
            );
        }
        // The 7″ panel is the one with no room for the whole name, and there it
        // is the name that gives way rather than the buttons.
        let column = chip_column(&items, NARROW, stub);
        let room = label_room(&items[0], column, NARROW, &mut stub);
        assert!(elided(label, room, stub).ends_with('…'));
    }

    #[test]
    fn chips_tile_left_to_right_and_stay_inside_the_margin() {
        let options = strings(&["Ember", "Bookerly", "Caecilia"]);
        let bounds = chip_bounds(400, WIDTH, "Type", &options, stub);
        assert_eq!(bounds.len(), 3);
        assert_eq!(bounds[0].0, 400);
        for (i, (x, w)) in bounds.iter().enumerate() {
            assert_eq!(*w, stub(&options[i]) + CHIP_PAD * 2, "chip {i} is its text");
            assert!(x + w <= WIDTH - MARGIN_X, "chip {i} runs past the margin");
        }
        for pair in bounds.windows(2) {
            assert_eq!(
                pair[0].0 + pair[0].1 + CHIP_GAP,
                pair[1].0,
                "chips are separated by exactly one gap"
            );
        }
        // Only when nothing else is left to give: a row that does not fit from
        // the column starts from its own label first, and only drops the tail
        // when even that is not enough room.
        let bounds = chip_bounds(400, 400, "Type", &options, stub);
        assert!(bounds.len() < options.len());
    }

    /// **The narrow-panel rule.** Three chips that do not fit from the shared
    /// column do fit from the row's own label, so the row steps out of the
    /// table rather than dropping a setting off the page.
    #[test]
    fn a_row_that_cannot_afford_the_column_keeps_its_own() {
        let options = strings(&["Ember", "Bookerly", "Caecilia"]);
        let bounds = chip_bounds(400, 600, "Type", &options, stub);
        assert_eq!(bounds.len(), 3, "every face is still reachable");
        assert!(bounds[0].0 < 400, "and they moved left to manage it");
        assert!(
            bounds[0].0 >= ROW_INSET + stub("Type"),
            "without running under the label"
        );

        // The row the column was measured from has nothing to gain by it, and
        // must not shove its own chips backwards over its own label.
        let long = "A Very Long Bluetooth Keyboard Name Indeed";
        assert_eq!(chip_bounds(400, 600, long, &options, stub)[0].0, 400);
    }

    /// The bug class this file has had three times: the invert lands on one
    /// control and the tap runs another. Drawing and hit-testing are the same
    /// two functions here, and this is the test that says so.
    #[test]
    fn a_tap_reported_on_a_chip_is_inside_the_chip_that_gets_drawn() {
        let l = layout();
        let items = settings();
        let column = chip_column(&items, WIDTH, stub);
        let Item::Choice { label, options, .. } = &items[1] else {
            panic!("item 1 is the languages row")
        };
        let bounds = chip_bounds(column, WIDTH, label, options, stub);
        let y = l.rows_top + l.row_h; // anywhere on item 1
        for option in 0..options.len() {
            let rect = chip_rect(l, 1, &bounds, option);
            let middle = rect.x + rect.width / 2;
            assert_eq!(
                hit(&items, l, WIDTH, middle, y, stub),
                Some(Hit::Option(1, option)),
                "the middle of chip {option} does not resolve to it"
            );
            assert!(rect.y >= l.rows_top + l.row_h, "chip {option} left its row");
            assert!(rect.y + rect.height <= l.rows_top + l.row_h * 2);
        }
    }

    /// The same bug class on the colour row, which has no text to be measured
    /// from and so lays itself out by its own arithmetic.
    #[test]
    fn a_tap_reported_on_a_swatch_is_inside_the_swatch_that_gets_drawn() {
        let l = layout();
        // The narrow panel, because it is the one where six of anything is a
        // question: the colour row only ever appears on a 1272 px Colorsoft.
        const COLORSOFT: u16 = 1272;
        let items = vec![Item::Swatches {
            label: "Highlight".into(),
            inks: (0..6).map(crate::window::ink::swatch).collect(),
            on: (0..6).map(|at| at == 3).collect(),
        }];
        let column = chip_column(&items, COLORSOFT, stub);
        let bounds = swatch_bounds(column, COLORSOFT, 6);
        assert_eq!(bounds.len(), 6, "a colour was left off the page");
        let y = l.rows_top;
        for option in 0..6 {
            let rect = chip_rect(l, 0, &bounds, option);
            let middle = rect.x + rect.width / 2;
            assert_eq!(
                hit(&items, l, COLORSOFT, middle, y, stub),
                Some(Hit::Option(0, option)),
                "the middle of swatch {option} does not resolve to it"
            );
            assert!(
                rect.x + rect.width <= COLORSOFT - MARGIN_X,
                "swatch {option} runs past the margin"
            );
        }
    }

    #[test]
    fn headings_labels_and_the_space_past_the_chips_are_not_controls() {
        let l = layout();
        let items = settings();
        let on_row = |i: u16| l.rows_top + i * l.row_h + 10;
        assert_eq!(
            hit(&items, l, WIDTH, WIDTH / 2, on_row(0), stub),
            None,
            "a heading names what follows and does nothing"
        );
        assert_eq!(
            hit(&items, l, WIDTH, ROW_INSET + 5, on_row(1), stub),
            None,
            "the label is not a control"
        );
        assert_eq!(
            hit(&items, l, WIDTH, WIDTH - MARGIN_X - 1, on_row(1), stub),
            None,
            "and neither is the space after the last chip"
        );
        assert_eq!(
            hit(&items, l, WIDTH, ROW_INSET, on_row(2), stub),
            Some(Hit::Row(2)),
            "a plain row is tappable across its width"
        );
    }

    /// An inert chip is a word in grey, and the tap has to stop at [`hit`].
    /// Nothing downstream can tell it apart: the action list a Config row
    /// carries is index-parallel to the options, so an inert option that
    /// reported a hit would fire whatever sits at its index.
    #[test]
    fn a_chip_marked_inert_is_a_state_and_not_a_target() {
        let l = layout();
        let items = vec![Item::Choice {
            label: "Keyboard".into(),
            options: strings(&["Connect", "Forget"]),
            on: vec![false, false],
            inert: vec![true, false],
        }];
        let middle = l.rows_top + 10;
        let bounds = chip_bounds(
            chip_column(&items, WIDTH, stub),
            WIDTH,
            "Keyboard",
            &["Connect".to_string(), "Forget".to_string()],
            stub,
        );
        let centre = |o: usize| bounds[o].0 + bounds[o].1 / 2;
        assert_eq!(
            hit(&items, l, WIDTH, centre(0), middle, stub),
            None,
            "the inert chip swallows the tap"
        );
        assert_eq!(
            hit(&items, l, WIDTH, centre(1), middle, stub),
            Some(Hit::Option(0, 1)),
            "its live neighbour still answers, at its own index"
        );
    }

    /// Short is fine and empty is the common case, the way it is for `on`.
    #[test]
    fn an_option_past_the_end_of_inert_is_live() {
        let l = layout();
        let items = vec![Item::Choice {
            label: "Size".into(),
            options: strings(&["38", "44"]),
            on: vec![true, false],
            inert: Vec::new(),
        }];
        let bounds = chip_bounds(
            chip_column(&items, WIDTH, stub),
            WIDTH,
            "Size",
            &["38".to_string(), "44".to_string()],
            stub,
        );
        assert_eq!(
            hit(
                &items,
                l,
                WIDTH,
                bounds[1].0 + bounds[1].1 / 2,
                l.rows_top + 10,
                stub
            ),
            Some(Hit::Option(0, 1))
        );
    }

    #[test]
    fn the_strip_sits_at_the_bottom_edge() {
        let l = layout();
        let rect = l.strip_rect(WIDTH);
        assert_eq!(rect.y + rect.height, HEIGHT);
        assert_eq!(rect.width, WIDTH);
    }

    /// A caret 34 tall, at `y`, for the candidate box tests.
    fn caret_at(y: u16) -> Rect {
        Rect {
            x: 300,
            y,
            width: 6,
            height: 34,
        }
    }

    /// The foot of the surface the box tests describe.
    const BOX_BOTTOM: u16 = 2360;

    #[test]
    fn candidates_are_numbered_and_the_tenth_is_zero() {
        let items = vec!["你".to_string(), "尼".to_string()];
        assert_eq!(Overlay::Candidates(&items).labels(), ["1 你", "2 尼"]);

        let ten: Vec<String> = (0..10).map(|i| i.to_string()).collect();
        let labels = Overlay::Candidates(&ten).labels();
        assert_eq!(labels[8], "9 8");
        assert_eq!(labels[9], "0 9", "the tenth is picked with 0");
    }

    #[test]
    fn a_notice_is_one_cell_and_carries_no_number() {
        // It is not a choice, so numbering it would invite a tap that does
        // nothing.
        assert_eq!(Overlay::Notice("日本語").labels(), ["日本語"]);
        assert!(Overlay::None.labels().is_empty());
    }

    #[test]
    fn there_is_no_candidate_box_when_there_is_nothing_to_choose() {
        let px = TEXT_PX;
        assert_eq!(
            overlay_rect(1860, &mut Stub, caret_at(100), px, BOX_BOTTOM, &[]),
            None
        );
    }

    #[test]
    fn the_candidate_box_sits_under_the_caret() {
        let px = TEXT_PX;
        let list = vec!["你好".to_string(), "尼豪".to_string()];
        let rect = overlay_rect(1860, &mut Stub, caret_at(100), px, BOX_BOTTOM, &list).unwrap();
        assert!(rect.y > 100 + 34, "below the caret, not over it: {rect:?}");
        assert!(rect.x <= 300, "starting at the caret or left of it");
    }

    #[test]
    fn the_label_sits_in_the_middle_of_its_box_both_ways() {
        // The label leans to the upper left when the box adds its padding once
        // per label and once more to the total — half a pad at the left and one
        // and a half at the right — and when its height is a leaded row with
        // the text at the top rather than half-leaded.

        let labels = vec!["简体".to_string()];
        let rect =
            overlay_rect(1860, &mut Stub, caret_at(100), TEXT_PX, BOX_BOTTOM, &labels).unwrap();
        let cells = overlay_cells(&mut Stub, rect, TEXT_PX, &labels);

        let (x, w) = cells[0];
        assert_eq!(x, rect.x, "the single cell is the box");
        assert_eq!(w, rect.width);
        let px = TEXT_PX * CANDIDATE_SCALE;
        let (pad_x, pad_y) = padding(px);
        let text = label_width(&mut Stub, "简体", px);
        assert_eq!(
            rect.width,
            text + pad_x * 2,
            "the same padding either side of the text"
        );

        let box_h = glyph_box(&mut Stub, "简体", px);
        assert_eq!(
            rect.height,
            box_h + pad_y * 2,
            "and the same above and below it"
        );
        // Which is what makes the drawn top symmetric.
        let top = rect.y as i32 + (rect.height as i32 - box_h as i32) / 2;
        assert_eq!(top - rect.y as i32, pad_y as i32);
    }

    #[test]
    fn a_short_latin_label_is_centred_in_the_box_a_han_one_would_need() {
        // `US` and `简体` have different glyph boxes. Whichever it is has to
        // end up in the middle, not standing on the box's floor.

        for label in ["US", "简体", "日本語"] {
            let labels = vec![label.to_string()];
            let rect =
                overlay_rect(1860, &mut Stub, caret_at(100), TEXT_PX, BOX_BOTTOM, &labels).unwrap();
            let px = TEXT_PX * CANDIDATE_SCALE;
            let box_h = glyph_box(&mut Stub, label, px);
            let above = (rect.height as i32 - box_h as i32) / 2;
            let below = rect.height as i32 - box_h as i32 - above;
            assert!(
                (above - below).abs() <= 1,
                "{label}: {above} above against {below} below"
            );
        }
    }

    #[test]
    fn a_box_with_no_room_below_goes_above_the_caret_instead() {
        // Composing on the last line of a page must not put the choices off
        // the bottom of it, where they cannot be read or tapped.
        let px = TEXT_PX;
        let list = vec!["你好".to_string()];
        let low = caret_at(BOX_BOTTOM - 40);
        let rect = overlay_rect(1860, &mut Stub, low, px, BOX_BOTTOM, &list).unwrap();
        assert!(
            rect.y + rect.height <= low.y,
            "above the caret: {rect:?} against a caret at {}",
            low.y
        );
    }

    #[test]
    fn a_box_anchored_to_the_find_bar_goes_above_it() {
        // The find bar is the strip, so there is never room below it. This is
        // the same fallback as composing on the last line of a page, and the
        // reason the box takes an anchor rather than reading the caret: while
        // finding, the caret is at the last match and the typing is down here.
        let px = TEXT_PX;
        let list = vec!["你好".to_string(), "尼豪".to_string()];
        let field = Rect {
            x: 0,
            y: BOX_BOTTOM + 3,
            width: 372,
            height: 117,
        };
        let rect = overlay_rect(1860, &mut Stub, field, px, BOX_BOTTOM, &list).unwrap();
        assert!(
            rect.y + rect.height <= field.y,
            "clear of the bar it belongs to: {rect:?}"
        );
        assert!(
            rect.y + rect.height <= BOX_BOTTOM,
            "and on the page rather than over the strip"
        );
    }

    /// The two panel widths karyll has to fit: the one it was written on, and
    /// the smaller one it is not allowed to break on. The narrow one is the
    /// wider of the two small panels, so the tighter one is a few pixels less.
    const WIDE: u16 = 1860;
    const NARROW: u16 = 1272;

    /// **Every setting has to be on the page**, at every panel width. A chip
    /// past the right margin is dropped, which for a Bluetooth keyboard's own
    /// name is the least bad answer and for karyll's own settings is a control
    /// the writer cannot reach at all. The type sizes are the row that comes
    /// closest: seven chips, and the widest label on the page pushes them as
    /// far right as the column is allowed to go.
    #[test]
    fn no_setting_falls_off_a_narrow_panel() {
        let sizes: Vec<String> = crate::render::SIZES
            .iter()
            .map(|px| format!("{px:.0}"))
            .collect();
        let languages: Vec<String> = crate::Language::ALL
            .iter()
            .map(|l| l.label().to_string())
            .collect();
        // A label long enough to push the column against its cap, which is what
        // a paired keyboard's name does.
        let items = vec![Item::Choice {
            label: "A Long Bluetooth Keyboard Name".into(),
            options: sizes.clone(),
            on: vec![],
            inert: Vec::new(),
        }];
        for panel in [WIDE, NARROW] {
            let measure = |s: &str| label_width(&mut Proportional, s, TEXT_PX);
            let column = chip_column(&items, panel, measure);
            for (label, options) in [("Size", &sizes), ("Languages", &languages)] {
                let bounds = chip_bounds(column, panel, label, options, measure);
                assert_eq!(
                    bounds.len(),
                    options.len(),
                    "{panel} px panel: {} of {:?} fit, from a column at {column}",
                    bounds.len(),
                    options
                );
            }
        }
    }

    /// Candidates are drawn at the size being written in, not at [`TEXT_PX`].
    const BODY: f32 = crate::render::DEFAULT_SIZE;

    /// `count` candidates of `chars` Han characters each — the one thing about
    /// them the box's width depends on.
    fn candidates(count: usize, chars: usize) -> Vec<String> {
        vec!["候".repeat(chars); count]
    }

    /// **Ten is what the number row can pick, not what the panel can show.**
    /// Ten three-character candidates are 1520 px of box — inside a 10.2″
    /// panel and past a 7″ one — so the smaller panel pages them and the
    /// larger one does not.
    #[test]
    fn a_bar_too_wide_for_the_panel_is_paged_rather_than_cut_off() {
        let list = candidates(10, 3);
        assert_eq!(
            candidate_pages(&mut Proportional, WIDE, BODY, &list, crate::ime::WANTED),
            vec![0]
        );
        assert_eq!(
            candidate_pages(&mut Proportional, NARROW, BODY, &list, crate::ime::WANTED),
            vec![0, 8],
            "eight fit, and the other two are a page rather than off the edge"
        );
    }

    /// The number row is the other limit, and it binds first when the
    /// candidates are short: eleven of them are two pages on any panel.
    #[test]
    fn a_page_is_never_longer_than_the_number_row() {
        let list = candidates(11, 1);
        assert_eq!(
            candidate_pages(&mut Proportional, WIDE, BODY, &list, crate::ime::WANTED),
            vec![0, crate::ime::WANTED]
        );
    }

    /// **The invariant the paging exists for**: everything on a page is inside
    /// the box that page is drawn in. It was not — [`overlay_rect`] clamped the
    /// box to the panel while [`overlay_cells`] went on laying candidates out
    /// past its edge, so the last ones were drawn off the screen and the number
    /// row went on picking them.
    #[test]
    fn every_candidate_on_a_page_is_inside_its_box() {
        for panel in [WIDE, NARROW] {
            for length in 1..=5 {
                let list = candidates(10, length);
                let pages =
                    candidate_pages(&mut Proportional, panel, BODY, &list, crate::ime::WANTED);
                for (p, &from) in pages.iter().enumerate() {
                    let to = pages.get(p + 1).copied().unwrap_or(list.len());
                    let labels = Overlay::Candidates(&list[from..to]).labels();
                    let rect = overlay_rect(
                        panel,
                        &mut Proportional,
                        caret_at(100),
                        BODY,
                        BOX_BOTTOM,
                        &labels,
                    )
                    .unwrap();
                    let cells = overlay_cells(&mut Proportional, rect, BODY, &labels);
                    let (x, w) = *cells.last().expect("a page is never empty");
                    assert!(
                        x + w <= rect.x + rect.width && x + w <= panel,
                        "{length}-character candidates, page {p} of {}, on a {panel} px panel: \
                         the bar ends at {} and the box at {}",
                        pages.len(),
                        x + w,
                        rect.x + rect.width
                    );
                }
            }
        }
    }

    /// Nothing to show is no pages, and a candidate too wide for the whole
    /// panel still gets one — refusing to show it at all would lose it.
    #[test]
    fn there_is_a_page_for_anything_the_engine_offers() {
        assert!(
            candidate_pages(&mut Proportional, NARROW, BODY, &[], crate::ime::WANTED).is_empty()
        );
        let huge = vec!["候".repeat(60)];
        assert_eq!(
            candidate_pages(&mut Proportional, NARROW, BODY, &huge, crate::ime::WANTED),
            vec![0]
        );
    }

    #[test]
    fn a_box_wider_than_the_room_to_its_right_is_pulled_left() {
        let px = TEXT_PX;
        let list: Vec<String> = (0..9).map(|i| format!("候補{i}")).collect();
        let far_right = Rect {
            x: 1800,
            ..caret_at(100)
        };
        let rect = overlay_rect(1860, &mut Stub, far_right, px, BOX_BOTTOM, &list).unwrap();
        assert!(
            rect.x + rect.width <= 1860,
            "the last candidate is not cut off: {rect:?}"
        );
    }
}
