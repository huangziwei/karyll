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
use karyll_core::markdown::{Block, Style};
use karyll_core::script::{Role, role_for, script_of};

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
    },
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
        paint_items(window, fonts, layout, self.items);
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
    let w = measure_text(label).saturating_add(CHIP_PAD * 2);
    let x = width.saturating_sub(MARGIN_X).saturating_sub(w);
    chip_slot(layout, item, x, w)
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
        Item::Choice { options, .. } => {
            let column = chip_column(items, width, &mut measure_text);
            let bounds = chip_bounds(column, width, options, &mut measure_text);
            cell_at(&bounds, x).map(|option| Hit::Option(index, option))
        }
    }
}

/// Draw the list. Separated so a selection change can repaint just these.
pub fn paint_items(window: &mut Window, fonts: &mut Fonts, layout: Layout, items: &[Item]) {
    let width = window.width();
    window.fill(layout.rows_rect(width), WHITE);
    let column = chip_column(items, width, |s| measure(fonts, s, TEXT_PX) as u16);
    // `capacity` rather than a break inside the loop, because that is the
    // number the caller pages by. Two ways of saying where the list stops is
    // two ways for them to disagree, and this one would hide a document.
    for (i, item) in items.iter().take(layout.capacity()).enumerate() {
        let top = layout.rows_top + i as u16 * layout.row_h;
        let middle = top as i32 + (layout.row_h as i32 - TEXT_PX as i32) / 2;
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
                draw_line(window, fonts, label, ROW_INSET, middle, TEXT_PX, *on, BLACK);
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
                    draw_chip(window, fonts, rect, text, false, false);
                }
            }
            // No rule: the chips are visibly bounded already, and a line under
            // every setting buries the structure of the page.
            Item::Choice { label, options, on } => {
                draw_line(
                    window, fonts, label, ROW_INSET, middle, TEXT_PX, false, BLACK,
                );
                let bounds = chip_bounds(column, width, options, |s| {
                    measure(fonts, s, TEXT_PX) as u16
                });
                for (o, _) in bounds.iter().enumerate() {
                    let rect = chip_rect(layout, i, &bounds, o);
                    draw_chip(
                        window,
                        fonts,
                        rect,
                        &options[o],
                        on.get(o).copied().unwrap_or(false),
                        false,
                    );
                }
            }
        }
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
    let fixed: u16 = cells
        .iter()
        .enumerate()
        .filter(|(i, _)| !stretch.contains(i))
        .map(|(_, label)| fitted(label, &mut measure_text))
        .sum();
    let each = share(width, fixed, stretch.len());

    let mut out: Vec<(u16, u16)> = Vec::new();
    let mut x = 0u16;
    for (i, label) in cells.iter().enumerate() {
        let w = if stretch.contains(&i) {
            each
        } else {
            fitted(label, &mut measure_text)
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
fn fitted(label: &str, measure_text: &mut impl FnMut(&str) -> u16) -> u16 {
    measure_text(label).saturating_add(CELL_PAD * 2)
}

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
    let fixed: u16 = others
        .iter()
        .map(|label| fitted(label, &mut measure_text))
        .sum();
    share(width, fixed, elastic).saturating_sub(CELL_PAD * 2)
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
    if let Some(Item::Row {
        label,
        detail,
        on,
        action,
    }) = items.get(index)
    {
        draw_line(window, fonts, label, ROW_INSET, baseline, TEXT_PX, *on, ink);
        if !detail.is_empty() {
            let column = chip_column(items, width, |s| measure(fonts, s, TEXT_PX) as u16);
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
            draw_chip(window, fonts, chip, text, false, false);
        }
    }
    rect
}

pub fn measure(fonts: &mut Fonts, text: &str, px: f32) -> f32 {
    text.chars()
        .map(|c| fonts.advance(role_for(Block::Paragraph, Style::Text, script_of(c)), px, c))
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
    let block = if bold {
        Block::Heading(2)
    } else {
        Block::Paragraph
    };
    // The baseline sits low enough for the faces this label uses. A Han label —
    // the language button, a candidate, a Chinese filename — is taller than the
    // Latin face's ascent, and that ascent would draw it above its own row.
    let roles: Vec<Role> = text
        .chars()
        .map(|ch| role_for(block, Style::Text, script_of(ch)))
        .collect();
    let baseline = y as f32 + fonts.ascent(px, &roles);
    for ch in text.chars() {
        let role = role_for(block, Style::Text, script_of(ch));
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
        .map(|c| role_for(Block::Paragraph, Style::Text, script_of(c)))
        .collect()
}

/// How wide a label draws. The same sum `ui::measure` does, but against
/// `Metrics` rather than the concrete faces, so the geometry above can be
/// checked on the host.
fn label_width(fonts: &mut impl Metrics, label: &str, px: f32) -> u16 {
    let width: f32 = label
        .chars()
        .map(|c| fonts.advance(role_for(Block::Paragraph, Style::Text, script_of(c)), px, c))
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
    use crate::font::Stub;

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
