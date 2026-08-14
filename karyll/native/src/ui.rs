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
