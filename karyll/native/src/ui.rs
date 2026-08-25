//! On-screen UI. Geometry is derived from the face in use, a full-width strip
//! carries the actions, and hit-testing is a pure function over a measured
//! [`Layout`]. Coordinates arrive in window space.

use anyhow::Result;
use karyll_core::script::{Role, chrome_role_for, script_of};

use crate::font::{Fonts, Metrics};
use crate::window::{BLACK, QUIET, Rect, WHITE, Window};

/// Bottom action strip: a generous button row.
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
    /// `text_lh` and `title_lh` are the line heights of the faces drawn.
    pub fn compute(text_lh: u16, title_lh: u16, height: u16) -> Self {
        let lh = text_lh.max(1);
        let title_lh = title_lh.max(1);
        let title_top = title_lh / 2;
        let status_top = title_top + title_lh;
        Layout {
            line_height: lh,
            title_top,
            status_top,
            // A line of air between the status and the first row.
            rows_top: status_top + lh * 2,
            // Tap targets hold a 96 px floor at every font size.
            row_h: (lh * 2).max(96),
            strip_top: height.saturating_sub(STRIP_H),
        }
    }

    /// How many rows fit between the top of the list and the strip. A panel
    /// does not scroll, and a longer list is paged by the caller.
    pub fn capacity(&self) -> usize {
        (self.strip_top.saturating_sub(self.rows_top) / self.row_h.max(1)) as usize
    }

    /// Which row a tap at `y` fell on, or `None` above the list. The strip
    /// spans the full bottom width and belongs to the caller.
    pub fn row_at(&self, y: u16, rows: usize) -> Option<usize> {
        if y >= self.strip_top || y < self.rows_top {
            return None;
        }
        let row = ((y - self.rows_top) / self.row_h) as usize;
        (row < rows).then_some(row)
    }

    /// The list region, for repainting a selection change on its own.
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

/// One line of a panel. A `Choice` shows every option side by side, each its
/// own tap target.
pub enum Item {
    /// A section heading, with a rule under it. Not tappable.
    Heading(String),
    /// A tappable line: a name on the left, a detail column, and whether it is
    /// the one in use. The detail sits in the column the chips use.
    Row {
        label: String,
        detail: String,
        /// Drawn bold: the current one.
        on: bool,
        /// One chip of its own, pinned to the right margin, carrying the
        /// destructive action a row width away from the name that opens it.
        action: Option<String>,
    },
    /// A setting: a label, and the values it can take, drawn side by side.
    /// Each value is its own tap target.
    Choice {
        label: String,
        options: Vec<String>,
        /// One per option. A language row carries several on, a type row one,
        /// a navigating control none.
        on: Vec<bool>,
        /// Options that report a state. Drawn in [`QUIET`] and not tappable.
        /// An option past the end of this is live, as one past the end of `on`
        /// is off.
        inert: Vec<bool>,
    },
    /// A colour setting: the same row, its values drawn as filled circles.
    Swatches {
        label: String,
        /// One [`crate::window::ink`] index per swatch, in the order drawn.
        inks: Vec<u8>,
        on: Vec<bool>,
    },
}

/// Which line of a panel the keyboard is on, and which of that line's chips.
/// `row` indexes the items handed to the paint. `chip` is zero on a row with
/// no chips.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Focus {
    pub row: usize,
    pub chip: usize,
}

/// The chips of `item` a press can take, in the order they are drawn. A row's
/// own action chip is reached by its named key alone.
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
    /// Buttons along the bottom, left to right. Owned strings: the find bar's
    /// field and its count carry what was typed.
    pub strip: &'a [String],
    /// What the IME is offering for a word composed into this panel. Empty
    /// while nothing is being composed.
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
        // Last, over everything, hung off the status line. The empty body
        // below it is where the box lands.
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

/// The gap between a chip and the ring marking it, and the ring's thickness.
/// The ring sits outside the chip, which is filled when on and outlined when
/// available.
const RING_GAP: u16 = 5;
const RING: u16 = 3;

/// Blank space either side of a chip's text.
const CHIP_PAD: u16 = 24;
/// Between one chip and the next.
const CHIP_GAP: u16 = 20;

/// Where the second column starts on every row — chips, swatches and details
/// alike. Taken from the widest label, pulled back until the widest second
/// column fits, and held between `width / 3` and `width / 2`.
pub fn chip_column(items: &[Item], width: u16, mut measure_text: impl FnMut(&str) -> u16) -> u16 {
    let widest = items
        .iter()
        .filter_map(|item| match item {
            Item::Choice { label, .. } | Item::Row { label, .. } | Item::Swatches { label, .. } => {
                Some(measure_text(label))
            }
            // A heading starts at the margin and owns its whole line.
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

/// The furthest right this row's second column may start and finish inside the
/// right margin. [`u16::MAX`] for a row with no second column.
fn second_column_room(item: &Item, width: u16, measure_text: &mut impl FnMut(&str) -> u16) -> u16 {
    let right = width.saturating_sub(MARGIN_X);
    match item {
        Item::Heading(_) => u16::MAX,
        Item::Row { detail, action, .. } => {
            if detail.is_empty() {
                return u16::MAX;
            }
            // The action chip holds the right margin; the detail's room ends
            // a gap short of it.
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

/// How wide a row of cells is once tiled, gaps included. [`chip_column`],
/// [`chip_bounds`] and [`swatch_bounds`] all measure through this.
fn run_width(widths: impl IntoIterator<Item = u16>) -> u16 {
    let mut total = 0u16;
    let mut cells = 0u16;
    for w in widths {
        total = total.saturating_add(w);
        cells = cells.saturating_add(1);
    }
    total.saturating_add(CHIP_GAP.saturating_mul(cells.saturating_sub(1)))
}

/// How much of its line a row's label may take, ending a gap short of its own
/// value. Drawing and press feedback both measure through this.
pub fn label_room(
    item: &Item,
    column: u16,
    width: u16,
    measure_text: &mut impl FnMut(&str) -> u16,
) -> u16 {
    let right = width.saturating_sub(MARGIN_X);
    let gap = |start: u16| start.saturating_sub(ROW_INSET + CHIP_GAP);
    match item {
        // A heading starts at the margin and owns its whole line.
        Item::Heading(_) => right.saturating_sub(MARGIN_X),
        Item::Row { detail, action, .. } => {
            if !detail.is_empty() {
                return gap(column);
            }
            // Nothing in the second column: the line runs to the action chip.
            match action {
                Some(text) => right
                    .saturating_sub(action_width(text, measure_text))
                    .saturating_sub(CHIP_GAP),
                None => right,
            }
            .saturating_sub(ROW_INSET)
        }
        // Its own chips, which a row too narrow for the column keeps just
        // past its label.
        Item::Choice { label, options, .. } => {
            gap(chip_bounds(column, width, label, options, measure_text)
                .first()
                .map_or(column, |(x, _)| *x))
        }
        Item::Swatches { .. } => gap(column),
    }
}

/// `text`, cut to `room` with an ellipsis, or whole when it fits. The cut runs
/// by character, and a label in Chinese loses characters.
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
    // A space before the mark is trimmed.
    while kept.ends_with(' ') {
        kept.pop();
    }
    kept.push_str(mark);
    kept
}

/// Where each chip of one choice row sits, in window x. Drawing, hit-testing
/// and press feedback all measure through this. A chip past the right margin
/// is dropped; a row too narrow for [`chip_column`] starts past its own label.
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
    // Its own label only where that sits further left than the column.
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

/// A swatch's slot, wide enough for a thumb.
const SWATCH_W: u16 = 72;

/// Where a row of swatches sits, from the column the chips start at. Every
/// swatch is [`SWATCH_W`] wide.
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

/// Where a row's own action chip sits: the right margin, at the height and
/// vertical placing of any other chip. Measured from the right edge.
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

/// What a tap is on: `None` for a heading, a label, and the space past the
/// last chip.
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
        // The action chip is asked ahead of the row it sits on.
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

/// Draw the list. Separate from the panel: a selection change repaints just
/// these.
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
    // `capacity` is the number the caller pages by, and the one place the
    // list's end is decided.
    for i in 0..items.len().min(layout.capacity()) {
        let chip = focus.filter(|f| f.row == i).map(|f| f.chip);
        draw_item(window, fonts, layout, items, i, column, chip);
    }
}

/// Redraw one line of the list with the focus mark it carries, leaving the
/// rest of the page. Full width, which covers a ring drawn outside the last
/// chip.
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
    // The band a row's chips are centred in, shared with its label.
    let band = Rect {
        x: MARGIN_X,
        y: top,
        width: width.saturating_sub(MARGIN_X),
        height: layout.row_h,
    };
    let middle = |fonts: &mut Fonts, label: &str| centred_top(fonts, band, label, TEXT_PX);
    if focus.is_some() {
        // In the air [`ROW_INSET`] leaves. The label holds its position.
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
        // The text sits at the foot of its row and the rule under it, leaving
        // the empty half above as the gap between sections. Rows are a
        // uniform height: `row_at` divides by one number.
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
        // No rule of its own. The detail column separates one line from the
        // next.
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
            let y = middle(fonts, &label);
            draw_line(window, fonts, &label, ROW_INSET, y, TEXT_PX, *on, BLACK);
            if !detail.is_empty() {
                let y = middle(fonts, detail);
                draw_line(window, fonts, detail, column, y, TEXT_PX, false, BLACK);
            }
            if let Some(text) = action {
                let rect = action_rect(layout, i, width, text, |s| {
                    measure(fonts, s, TEXT_PX) as u16
                });
                // Never filled at rest. A fill marks the current value.
                draw_chip(window, fonts, rect, text, ChipState::default());
            }
        }
        // No rule: the chips carry their own bounds.
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
            let y = middle(fonts, &label);
            draw_line(window, fonts, &label, ROW_INSET, y, TEXT_PX, false, BLACK);
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
            let y = middle(fonts, &label);
            draw_line(window, fonts, &label, ROW_INSET, y, TEXT_PX, false, BLACK);
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

/// One swatch: a filled circle carrying the value, with a hole punched in the
/// chosen one. The mark survives a panel drawing these in grey.
fn draw_swatch(window: &mut Window, rect: Rect, ink: u8, on: bool) {
    let radius = rect.width.min(rect.height) / 2;
    let cx = rect.x + rect.width / 2;
    let cy = rect.y + rect.height / 2;
    disc(window, cx, cy, radius, ink);
    if on {
        disc(window, cx, cy, radius / 3, WHITE);
    }
}

/// A filled circle, scanline by scanline, shared with the Han emphasis mark.
/// Coverage is one bit and the edge is hard.
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

/// What a chip is, and what is happening to it. Every flag defaults off.
#[derive(Debug, Clone, Copy, Default)]
struct ChipState {
    /// What the setting is currently on.
    on: bool,
    /// Held under a finger.
    pressed: bool,
    /// Reporting a state, with no press behind it.
    inert: bool,
    /// Where the keyboard is.
    focused: bool,
}

/// One chip: filled when the setting is on it, outlined when it is available.
/// The renderer cuts coverage to one bit, and an inverted block survives that.
fn draw_chip(window: &mut Window, fonts: &mut Fonts, rect: Rect, label: &str, state: ChipState) {
    if state.focused {
        draw_ring(window, rect);
    }
    // A press inverts whatever the chip was, on or off alike.
    let filled = state.on != state.pressed;
    let (ground, ink) = if filled {
        (BLACK, WHITE)
    } else if state.inert {
        // Border and word both recede on a chip with no press behind it.
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
    let top = centred_top(fonts, rect, label, TEXT_PX);
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
        // A press marks the swatch the way a chosen one is marked.
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
/// A panel carries no status line; its title and status row name the screen.
pub fn paint_strip(window: &mut Window, fonts: &mut Fonts, layout: Layout, cells: &[String]) {
    let cells: Vec<String> = cells.iter().map(|label| format!("[ {label} ]")).collect();
    paint_cells(window, fonts, layout, &cells, &[], "");
}

/// The least blank a cell may keep either side of its text. Six cells at
/// [`CELL_PAD`] spend 312 px on air, and a narrow panel gives that up ahead
/// of a control.
const CELL_PAD_MIN: u16 = 8;

/// Blank space either side of a cell's text.
const CELL_PAD: u16 = 26;

/// Where each cell starts and how wide it is, in window x. Every cell is its
/// own text's width, packed from the left. `stretch` names the cells sharing
/// the remainder; empty leaves it to the status line.
pub fn cell_bounds(
    width: u16,
    cells: &[String],
    stretch: &[usize],
    mut measure_text: impl FnMut(&str) -> u16,
) -> Vec<(u16, u16)> {
    if cells.is_empty() {
        return Vec::new();
    }
    // The fixed cells first. An elastic cell's width is what they leave, and
    // its text is trimmed to that.
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
    // A cell wider than the whole strip is drawn clipped.
    if out.is_empty() {
        out.push((0, width));
    }
    out
}

/// One cell's width: its text, and the blank either side of it.
fn fitted(label: &str, pad: u16, measure_text: &mut impl FnMut(&str) -> u16) -> u16 {
    measure_text(label).saturating_add(pad * 2)
}

/// The blank this strip can afford either side of each cell: [`CELL_PAD`] where
/// the words fit with it, down to [`CELL_PAD_MIN`] where they do not, one value
/// across the whole strip. A stretch cell claims [`FIELD_MIN`] of the slack.
fn cell_pad(width: u16, text: u16, cells: usize, elastic: usize) -> u16 {
    let wanted = text.saturating_add(FIELD_MIN * elastic as u16);
    let each = width.saturating_sub(wanted) / (2 * cells.max(1)) as u16;
    each.clamp(CELL_PAD_MIN, CELL_PAD)
}

/// The least width a stretch cell is given: eight Latin characters at
/// [`TEXT_PX`].
pub const FIELD_MIN: u16 = (TEXT_PX * 8.0 / 2.0) as u16;

/// What one elastic cell gets of the room the fixed ones leave. Zero with
/// nothing elastic, leaving the remainder to the status line.
fn share(width: u16, fixed: u16, elastic: usize) -> u16 {
    if elastic == 0 {
        return 0;
    }
    width.saturating_sub(fixed) / elastic as u16
}

/// How much text each stretch cell has room for, given `others`. A stretch
/// cell's own label is not among them; its text is trimmed to what it is
/// given. The same arithmetic [`cell_bounds`] lays out with.
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

/// Which cell a tap at `x` fell on, or `None` for space no cell occupies. The
/// strip runs the full width and its cells may leave a tail.
pub fn cell_at(bounds: &[(u16, u16)], x: u16) -> Option<usize> {
    bounds
        .iter()
        .position(|(cx, w)| x >= *cx && x < cx.saturating_add(*w))
}

/// Draw cells along the bottom, with the text given. The action strip and the
/// find bar are both this. The rule runs the full width, no rules sit between
/// cells, and `status` is right-aligned and quiet in the same band.
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

/// Where a line of strip text sits. The buttons and the status line share it.
fn cell_text_top(layout: Layout) -> i32 {
    layout.strip_top as i32 + (STRIP_H as i32 - TEXT_PX as i32) / 2 - 4
}

/// The status line, in the room the buttons leave: right-aligned against the
/// far margin and drawn quiet. Dropped where the buttons have taken the width.
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

/// The rectangle of one strip cell, from the bounds [`cell_bounds`] laid out.
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

/// Redraw one strip cell, inverted while held. The one acknowledgement a tap
/// gets while the finger is down.
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
    // An empty cell is nothing to press. The find bar's count is blank until
    // something has been typed into it.
    if cells.get(index).is_none_or(|text| text.is_empty()) {
        return rect;
    }
    let (ground, ink) = if pressed {
        (BLACK, WHITE)
    } else {
        (WHITE, BLACK)
    };
    window.fill(rect, ground);
    // The label exactly as [`paint_cells`] draws it, brackets included. Press
    // feedback differs from the resting state by the inversion alone.
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
        // Drawn at rest while the row around it is inverted.
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
    // The baseline sits low enough for every face this label uses. A Han
    // label stands taller than the Latin face's ascent.
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
/// room for it. `anchor` is the caret, the find field or the status line, as a
/// rectangle and a type size; `bottom` is the last row it may occupy.
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

    // Each cell is its label plus the same padding either side, and the box is
    // exactly the cells. The padding is counted once.
    let cells: Vec<u16> = labels
        .iter()
        .map(|label| label_width(fonts, label, px) + pad_x * 2)
        .collect();
    let width = cells.iter().sum::<u16>().min(surface_width);
    // The tallest label's own glyph box. Leading is the space between lines of
    // prose, and the box holds one line.
    let text_box = labels
        .iter()
        .map(|label| glyph_box(fonts, label, px))
        .fold(0, u16::max);
    let height = text_box + pad_y * 2;

    // Below the anchor where it fits, above it where it does not. The find bar
    // sits on the strip, with nothing below.
    let below = anchor.y as i32 + anchor.height as i32 + gap as i32;
    let y = if below + height as i32 <= bottom as i32 {
        below
    } else {
        anchor.y as i32 - gap as i32 - height as i32
    };
    if y < 0 {
        return None;
    }
    // Pulled left by whatever it overhangs, keeping the last candidate whole.
    let x = anchor.x.min(surface_width.saturating_sub(width));
    Some(Rect {
        x,
        y: y as u16,
        width,
        height,
    })
}

/// Space inside the box, wider horizontally than vertically for one line of
/// text.
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

/// The top edge that centres `label`'s ink in `rect` under [`draw_line`],
/// which places the baseline an ascent below the top it is handed.
fn centred_top(fonts: &mut impl Metrics, rect: Rect, label: &str, px: f32) -> i32 {
    let roles = label_roles(label);
    let (top, bottom) = fonts.ink_box(px, &roles);
    let baseline = rect.y as f32 + (rect.height as f32 - (bottom - top)) / 2.0 - top;
    (baseline - fonts.ascent(px, &roles)).round() as i32
}

fn label_roles(label: &str) -> Vec<Role> {
    label
        .chars()
        .map(|c| chrome_role_for(false, script_of(c)))
        .collect()
}

/// How wide a label draws: the sum [`measure`] does, over `Metrics`.
pub fn label_width(fonts: &mut impl Metrics, label: &str, px: f32) -> u16 {
    let width: f32 = label
        .chars()
        .map(|c| fonts.advance(chrome_role_for(false, script_of(c)), px, c))
        .sum();
    width.round() as u16
}

/// Where each label sits inside the box, in absolute window x. Drawing and the
/// tap test both measure through this.
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

/// Where each page of candidates starts, given what the panel can hold. A page
/// is as many as fit, at most `most`. Empty for no candidates; a candidate
/// wider than the panel takes a page to itself.
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
        // Measured with the number in front that gets drawn. Every digit is
        // one width, and the tenth's `0` stands for all of them.
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
    // A filled panel with a rule around it. One bit of coverage carries no
    // shadow and no tint, and the border is what lifts it off the page.
    window.fill(rect, WHITE);
    frame_rect(window, rect, BORDER);

    let cells = overlay_cells(fonts, rect, body_px, labels);
    for (label, (x, _)) in labels.iter().zip(&cells) {
        // Each label centred on its own ink, whichever face draws it.
        let top = centred_top(fonts, rect, label, px);
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

/// Candidates are set smaller than the prose.
pub const CANDIDATE_SCALE: f32 = 0.72;

/// Thickness of the candidate box's border.
const BORDER: u16 = 2;
/// The small box that appears next to the caret. One slot: switching language
/// abandons any composition.
pub enum Overlay<'a> {
    None,
    /// Numbered choices from the IME.
    Candidates(&'a [String]),
    /// A single unnumbered label naming the language the keyboard became. The
    /// strip is hidden while writing.
    Notice(&'a str),
}

impl Overlay<'_> {
    /// The cells to draw, numbered or not. The numbering lands here, and the
    /// widths measured for the box are the widths of these strings.
    pub fn labels(&self) -> Vec<String> {
        match self {
            Self::None => Vec::new(),
            // The tenth is picked with 0.
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
        // Spacing comes from the title face.
        let l = layout();
        assert!(l.status_top >= l.title_top + 58, "status clears the title");
        assert!(l.rows_top >= l.status_top + 44, "rows clear the status");
        assert!(l.rows_top < l.strip_top);
    }

    #[test]
    fn geometry_follows_the_faces_in_use() {
        // A larger face pushes everything down.
        let small = Layout::compute(30, 40, HEIGHT);
        let large = Layout::compute(60, 80, HEIGHT);
        assert!(large.title_top > small.title_top);
        assert!(large.status_top > small.status_top);
        assert!(large.rows_top > small.rows_top);
        assert_eq!(large.row_h, 120, "twice the line height, past the floor");
    }

    #[test]
    fn a_small_face_still_gets_a_reachable_row() {
        // The 96 px floor holds under a small font.
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

    /// A stub metric: ten pixels a character.
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

    /// A row that opens something.
    fn opener() -> Item {
        Item::Row {
            label: "draft.md".into(),
            detail: "300 words".into(),
            on: false,
            action: Some("Delete".into()),
        }
    }

    /// The chips the arrows walk are the ones a finger can press. A row's own
    /// action chip is reached by its named key.
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

    /// Buttons take the width their own text needs, packed from the left.
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

    /// The tail past the last cell belongs to the status line, and a tap on it
    /// resolves to nothing.
    #[test]
    fn a_tap_past_the_last_button_hits_nothing() {
        let cells = strings(&["[ Exit ]", "[ Config ]"]);
        let bounds = cell_bounds(WIDTH, &cells, &[], stub);
        assert_eq!(cell_at(&bounds, 5), Some(0));
        assert_eq!(cell_at(&bounds, WIDTH - 1), None);
    }

    /// The find bar's field takes the slack, and every button holds its place
    /// as the field is typed into.
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

    /// `stretch_room` and `cell_bounds` do one subtraction: the text is
    /// trimmed against the width it is drawn into.
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
        // The width is the slack, not the text.
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

    /// A field long enough to fill the strip is drawn clipped.
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

    /// The panel does not scroll, and `capacity` is what the caller pages by.
    #[test]
    fn what_fits_is_what_the_caller_pages_by() {
        let l = layout();
        let fits = l.capacity();
        assert!(fits > 0);
        // The last row that fits resolves; the first that does not is nothing.
        // `paint_items` draws to the same boundary.
        assert_eq!(
            l.row_at(l.rows_top + (fits as u16 - 1) * l.row_h, fits),
            Some(fits - 1)
        );
        assert!(l.rows_top + fits as u16 * l.row_h + l.row_h > l.strip_top);
    }

    #[test]
    fn the_rows_region_stops_short_of_the_strip() {
        // The list stops above the strip.
        let l = layout();
        let rect = l.rows_rect(WIDTH);
        assert_eq!(rect.y, l.rows_top);
        assert_eq!(rect.y + rect.height, l.strip_top);
    }

    #[test]
    fn a_pressed_cell_covers_its_own_slot_and_no_other() {
        // The invert lands exactly on the cell under the finger.
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

    /// The chip that removes a row is pinned to the far margin, and [`hit`]
    /// asks it ahead of the row it sits on.
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
        // The two sit at opposite ends of the row.
        assert!(
            chip.x > WIDTH / 2,
            "the chip is at the far margin, not beside the name: {chip:?}"
        );
        assert_eq!(chip.x + chip.width, WIDTH - MARGIN_X);
    }

    /// The chip holds one edge whatever the row is called.
    #[test]
    fn action_chips_line_up_however_long_the_names_are() {
        let l = layout();
        let short = action_rect(l, 0, WIDTH, "Delete", stub);
        let same = action_rect(l, 3, WIDTH, "Delete", stub);
        assert_eq!(short.x, same.x);
        // An armed one grows leftward.
        let armed = action_rect(l, 0, WIDTH, "Delete?", stub);
        assert_eq!(armed.x + armed.width, short.x + short.width);
        assert!(armed.x < short.x);
    }

    /// On a row with no action chip, every tap resolves to the row, including
    /// one at the far margin.
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

    /// Chips and details line up down the page in one column, shared by a list
    /// of files and a page of settings.
    #[test]
    fn every_line_starts_its_second_column_in_the_same_place() {
        let items = settings();
        let column = chip_column(&items, WIDTH, stub);
        assert!(column > ROW_INSET + stub("Languages"), "clears the widest");
        assert!(column > ROW_INSET + stub("Latin"));
        assert!(column > ROW_INSET + stub("draft.md"), "rows count too");

        // A heading starts at the margin and moves the column nothing.
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

    /// One list lays out to a different column per panel width: a narrow panel
    /// pulls it back until the second column finishes inside the right margin.
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

    /// A settings row carries a run of chips where a file row carries a
    /// detail, and both bind the column. Seven type sizes is the widest run.
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

    /// A label is cut at its own value.
    #[test]
    fn a_label_too_long_for_its_room_is_cut_at_the_mark() {
        assert_eq!(elided("draft.md", 200, stub), "draft.md", "it fits, whole");
        // Ten pixels a character, the mark included: five characters and the
        // mark are 60, and 65 keeps five.
        assert_eq!(elided("a-long-filename.md", 65, stub), "a-lon…");
        // Cut by character: a Chinese name loses whole characters.
        assert_eq!(elided("第一章的草稿", 45, stub), "第一章…");
        // A space before the mark is trimmed.
        assert_eq!(elided("Focus on this", 75, stub), "Focus…");
        // Below the mark's own width, nothing is drawn.
        assert_eq!(elided("draft.md", 5, stub), "");
    }

    /// A Bluetooth keyboard's name comes from the keyboard. `elided` cuts a
    /// long one, and its chips stay inside the right margin.
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
        // On `NARROW` the name gives way and the buttons hold.
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

    /// Three chips that miss the shared column fit from the row's own label,
    /// and the row keeps every setting.
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

    /// Drawing and hit-testing measure through the same bounds: the invert
    /// lands on the control the tap runs.
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

    /// The colour row, with no text to measure, lays out by its own
    /// arithmetic; a tap resolves inside the swatch drawn.
    #[test]
    fn a_tap_reported_on_a_swatch_is_inside_the_swatch_that_gets_drawn() {
        let l = layout();
        // The colour row appears on a 1272 px panel, where six cells is the
        // tight case.
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

    /// An inert chip is a word in grey, and the tap stops at [`hit`]. A Config
    /// row's action list is index-parallel to its options.
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

    /// `inert` may be short or empty, as `on` may.
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
        // A notice carries no choice and no number.
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
        // The label sits centred: the box counts its padding once, and its
        // height is the glyph box with the text half-leaded.

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
        let box_top = rect.y as i32 + (rect.height as i32 - box_h as i32) / 2;
        assert_eq!(box_top - rect.y as i32, pad_y as i32);
    }

    /// **Every script sits square in its box.** `EN`, `简` and `한` come out of
    /// three faces whose reported extents differ by a fifth of an em;
    /// [`centred_top`] measures the ink they draw.
    #[test]
    fn every_script_sits_in_the_middle_of_its_box() {
        for label in ["EN", "简", "日本語", "한글", "1 你好"] {
            let labels = vec![label.to_string()];
            let rect =
                overlay_rect(1860, &mut Stub, caret_at(100), TEXT_PX, BOX_BOTTOM, &labels).unwrap();
            let px = TEXT_PX * CANDIDATE_SCALE;
            let roles = label_roles(label);
            let (ink_top, ink_bottom) = Stub.ink_box(px, &roles);
            let baseline = centred_top(&mut Stub, rect, label, px) as f32 + Stub.ascent(px, &roles);
            let above = baseline + ink_top - rect.y as f32;
            let below = (rect.y + rect.height) as f32 - (baseline + ink_bottom);
            assert!(
                (above - below).abs() <= 1.0,
                "{label}: {above:.1} above against {below:.1} below"
            );
        }
    }

    /// A chip is the same rule against a rectangle of its own.
    #[test]
    fn every_script_sits_in_the_middle_of_its_chip() {
        let rect = Rect {
            x: 0,
            y: 400,
            width: 200,
            height: 72,
        };
        for label in ["EN", "简", "한", "Forget"] {
            let roles = label_roles(label);
            let (ink_top, ink_bottom) = Stub.ink_box(TEXT_PX, &roles);
            let baseline =
                centred_top(&mut Stub, rect, label, TEXT_PX) as f32 + Stub.ascent(TEXT_PX, &roles);
            let above = baseline + ink_top - rect.y as f32;
            let below = (rect.y + rect.height) as f32 - (baseline + ink_bottom);
            assert!(
                (above - below).abs() <= 1.0,
                "{label}: {above:.1} above against {below:.1} below"
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
        // The find bar is the strip, with no room below it. While finding, the
        // caret is at the last match and the typing is in the field.
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

    /// The widest panel, and the wider of the two narrow ones — the tightest
    /// is a few pixels less.
    const WIDE: u16 = 1860;
    const NARROW: u16 = 1272;

    /// **Every setting is on the page**, at every panel width. A chip past the
    /// right margin is dropped. The type sizes are the closest row: seven
    /// chips, under the widest label on the page.
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

    /// Candidates are drawn at the size being written in.
    const BODY: f32 = crate::render::DEFAULT_SIZE;

    /// `count` candidates of `chars` Han characters each — the one thing about
    /// them the box's width depends on.
    fn candidates(count: usize, chars: usize) -> Vec<String> {
        vec!["候".repeat(chars); count]
    }

    /// Ten is what the number row picks, not what the panel shows. Ten
    /// three-character candidates are 1520 px of box: inside a 10.2″ panel and
    /// past a 7″ one.
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

    /// Everything on a page is inside the box that page is drawn in.
    /// [`overlay_rect`] clamps the box to the panel, [`overlay_cells`] lays the
    /// candidates out, and the two have to agree.
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

    /// Nothing to show is no pages; a candidate too wide for the panel takes
    /// a page of its own.
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
