//! Loading faces, and measuring with them.
//!
//! Nothing is compiled into the binary. The firmware's own faces are read from
//! `/usr/java/lib/fonts`, and the three writing faces karyll ships are read from
//! `fonts/` beside it in the extension. The policy for *which* face a run wants
//! lives in `karyll_core::script` and is testable without any of them present;
//! this module is the part that needs the files.
//!
//! **The app and the page are set separately.** [`CHROME_FACES`] draws karyll's
//! own text and cannot be changed; [`LATIN_FAMILIES`] and the Han lists are what
//! a document can be set in. One face doing both jobs made the panel look like a
//! draft and moved the panel's own geometry every time a writer tried a face on.
//!
//! **Why these faces.** The renderer thresholds glyph coverage to one bit,
//! because the partial waveform is two-level and an antialiased grey edge comes
//! out muddy. That rules the body face more than taste does: 宋体 sets thin
//! horizontal strokes that thin out further under a one-bit cut, where 黑体
//! holds an even stroke weight and survives it. So the Han body defaults to
//! **STHeitiMedium** and Latin to **iA Writer Duo**, pairing a sans with a sans.
//!
//! **The defaults are the head of a short list, not the only option.** The
//! stroke-thinning argument is a prediction rather than a measurement, and it is
//! also a matter of taste once it stops being a matter of legibility, so
//! [`families`] offers two or three per writing system and the settings panel
//! picks between them. It is a curated list and deliberately not a browser over
//! the faces the device carries: most of them are handwriting styles or scripts
//! karyll does not typeset, and a list nobody can read through is not a choice.
//!
//! **Faces are read on first use, never at startup.** A face costs its file
//! size in resident bytes, and the Han faces are ~10 MB each against ~514 MB
//! shared with the framework. An English-only session never pays for them.

use std::collections::HashMap;
use std::io::SeekFrom;
use std::path::Path;

use ab_glyph::{Font as _, FontVec, PxScale, ScaleFont as _};
use anyhow::{Result, anyhow};
use karyll_core::script::{Region, Role};

/// The measurements layout needs, separated from the faces that supply them.
///
/// Layout arithmetic is worth testing — the goal column, page overlap, where a
/// caret lands — and none of it can run against real faces on a development
/// machine, because they live on the device. A stub implements this instead.
pub trait Metrics {
    /// How far the pen moves after drawing `ch` at `px`.
    fn advance(&mut self, role: Role, px: f32, ch: char) -> f32;
    /// Baseline-to-baseline distance for a row that may hold any of `roles`.
    fn line_height(&mut self, px: f32, roles: &[Role]) -> f32;
    /// Top of the row to its baseline, for a row that may hold any of `roles`.
    fn ascent(&mut self, px: f32, roles: &[Role]) -> f32;
    /// Top and bottom of the ink `roles` draw, from the baseline, positive
    /// downwards. What a label is centred on; see [`probe`].
    fn ink_box(&mut self, px: f32, roles: &[Role]) -> (f32, f32);
}

/// The character whose ink stands for a role's face: a cap with no descender
/// for Latin, and for Han and Hangul a character that fills its em.
fn probe(role: Role) -> char {
    if role.is_hangul() {
        '한'
    } else if role.is_han() {
        '中'
    } else {
        'H'
    }
}

/// A row that only ever holds Latin: chrome, panel titles, anything whose text
/// karyll wrote itself rather than read out of a document.
pub const LATIN_ROW: &[Role] = &[Role::Chrome];

/// A set of roles that are chosen between as one: the Latin four, one
/// convention's Han pair, or Hangul's.
///
/// **The Han faces depend on the region**, because Han unification gives the
/// three conventions one code point and three correct glyphs. Drawing
/// Traditional or Japanese from the Simplified faces is not a missing glyph —
/// it is the wrong glyph, silently. So each convention chooses separately, and
/// Latin chooses once for every language written in it: English and German are
/// drawn by the same faces, and offering each of them its own setting would be
/// two controls over one thing.
///
/// **Hangul takes no region.** No code point it holds is drawn two ways, so a
/// Korean family is a plain choice of face, as a Latin one is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Group {
    Latin,
    Han(Region),
    Hangul,
}

/// Every group, in the order the settings panel lists them.
pub const GROUPS: [Group; 5] = [
    Group::Latin,
    Group::Han(Region::Simplified),
    Group::Han(Region::Traditional),
    Group::Han(Region::Japanese),
    Group::Hangul,
];

impl Group {
    /// The group `role` is chosen as part of.
    fn of(role: Role, region: Region) -> Group {
        if role.is_hangul() {
            Group::Hangul
        } else if role.is_han() {
            Group::Han(region)
        } else {
            Group::Latin
        }
    }

    /// The roles this group supplies faces for, in the order [`Family::faces`]
    /// lists them.
    fn roles(self) -> &'static [Role] {
        match self {
            Group::Latin => &LATIN_ROLES,
            Group::Han(_) => &HAN_ROLES,
            Group::Hangul => &HANGUL_ROLES,
        }
    }

    /// The convention this group draws. Latin and Hangul have none and answer
    /// the default, which is the region their slots are filed under.
    fn region(self) -> Region {
        match self {
            Group::Latin | Group::Hangul => Region::default(),
            Group::Han(region) => region,
        }
    }

    /// What the settings panel calls this row, under its Type heading.
    ///
    /// Each names itself in its own script, as the language button does. No
    /// 字体/書体 after it: the section heading says a font row is a font row,
    /// and the labels are the shorter for it.
    pub fn label(self) -> &'static str {
        match self {
            Group::Latin => "Latin",
            Group::Han(Region::Simplified) => "简体",
            Group::Han(Region::Traditional) => "繁體",
            Group::Han(Region::Japanese) => "日本語",
            Group::Hangul => "한글",
        }
    }

    /// What this group is stored under in `var/fonts`.
    fn token(self) -> &'static str {
        match self {
            Group::Latin => "latin",
            Group::Han(Region::Simplified) => "sc",
            Group::Han(Region::Traditional) => "tc",
            Group::Han(Region::Japanese) => "ja",
            Group::Hangul => "ko",
        }
    }
}

/// One entry in a group's curated list: what to call it, and the face that
/// draws each of the group's roles.
pub struct Family {
    /// Shown in the settings panel, and the key it is stored under. Each names
    /// itself the way its readers would.
    pub name: &'static str,
    /// One path per role in [`Group::roles`] order.
    faces: &'static [&'static str],
}

/// The faces karyll's own text is drawn in, in [`CHROME_ROLES`] order.
///
/// **Amazon Ember, and it is not on offer.** It is the Kindle's interface face,
/// so a panel set in it reads as part of the device rather than as a document —
/// which is what chrome is for. It also never changes, so the geometry laid out
/// against it in [`crate::ui`] holds for the life of the process; see
/// [`karyll_core::script::chrome_role_for`] for why that matters.
const CHROME_FACES: [&str; 2] = [
    "/usr/java/lib/fonts/Amazon-Ember-Regular.ttf",
    "/usr/java/lib/fonts/Amazon-Ember-Bold.ttf",
];

/// The Latin families a *document* can be set in, default first.
///
/// **The three faces karyll ships, and nothing off the firmware.** The device's
/// own faces draw the device — a page set in one looks like the reader it was
/// taken from, and the app's chrome is already in Ember. These three were drawn
/// to be written in, and shipping them is what makes one editor read the same
/// on every Kindle.
///
/// **They are bundled because no Kindle carries a monospace text face.** Every
/// device this was built against ships the same firmware faces, and the only
/// one named for the word is a symbol font — so a fenced block or a table has
/// nothing to line its columns up in. The three are cut from one design at a
/// shared 0.6 em base and differ in how many widths they allow: Mono holds one,
/// so it is the only one whose columns align; **Duo** widens six letters by
/// half and is the writing face iA itself defaults to, which is why it is the
/// default here; Quattro allows four widths and sets some 9% narrower than Duo,
/// buying back the line length a fixed pitch costs.
///
/// Every entry is a true four-face family, so emphasis and strong are real
/// italics and bolds rather than synthetic slants — which is what [`available`]
/// declines an incomplete family to protect.
const LATIN_FAMILIES: &[Family] = &[
    Family {
        name: "Duo",
        faces: &[
            "/mnt/us/extensions/karyll/fonts/iAWriterDuoS-Regular.ttf",
            "/mnt/us/extensions/karyll/fonts/iAWriterDuoS-Italic.ttf",
            "/mnt/us/extensions/karyll/fonts/iAWriterDuoS-Bold.ttf",
            "/mnt/us/extensions/karyll/fonts/iAWriterDuoS-BoldItalic.ttf",
        ],
    },
    Family {
        name: "Mono",
        faces: &[
            "/mnt/us/extensions/karyll/fonts/iAWriterMonoS-Regular.ttf",
            "/mnt/us/extensions/karyll/fonts/iAWriterMonoS-Italic.ttf",
            "/mnt/us/extensions/karyll/fonts/iAWriterMonoS-Bold.ttf",
            "/mnt/us/extensions/karyll/fonts/iAWriterMonoS-BoldItalic.ttf",
        ],
    },
    Family {
        name: "Quattro",
        faces: &[
            "/mnt/us/extensions/karyll/fonts/iAWriterQuattroS-Regular.ttf",
            "/mnt/us/extensions/karyll/fonts/iAWriterQuattroS-Italic.ttf",
            "/mnt/us/extensions/karyll/fonts/iAWriterQuattroS-Bold.ttf",
            "/mnt/us/extensions/karyll/fonts/iAWriterQuattroS-BoldItalic.ttf",
        ],
    },
];

/// Simplified Chinese, default first.
///
/// A body face and its bold, and nothing between them: emphasis is a 着重号
/// against each character and leaves the face alone, so a family is one design
/// at two weights. The device carries no Simplified 楷体 or 圆体, so these two
/// are all there is.
const SIMPLIFIED_FAMILIES: &[Family] = &[
    Family {
        name: "黑体",
        faces: &[
            "/usr/java/lib/fonts/STHeitiMedium.ttf",
            "/usr/java/lib/fonts/STHeitiBold.ttf",
        ],
    },
    Family {
        name: "宋体",
        faces: &[
            "/usr/java/lib/fonts/STSongMedium.ttf",
            "/usr/java/lib/fonts/STSongBold.ttf",
        ],
    },
];

/// Traditional Chinese, default first.
///
/// 楷體 and 圓體 live in `/var/local/font`, a font pack rather than the system
/// directory, so they are the entries most likely to be absent — which is what
/// the existence check exists for.
const TRADITIONAL_FAMILIES: &[Family] = &[
    Family {
        name: "黑體",
        faces: &[
            "/usr/java/lib/fonts/STHeitiTC.ttf",
            "/usr/java/lib/fonts/STHeitiTCBold.ttf",
        ],
    },
    Family {
        name: "楷體",
        faces: &[
            "/var/local/font/mnt/zh-Hant_font/fonts/STKaitiTC.ttf",
            "/var/local/font/mnt/zh-Hant_font/fonts/STKaitiTCBold.ttf",
        ],
    },
    Family {
        name: "圓體",
        faces: &[
            "/var/local/font/mnt/zh-Hant_font/fonts/STYuanTC.ttf",
            "/var/local/font/mnt/zh-Hant_font/fonts/STYuanTCBold.ttf",
        ],
    },
];

/// Japanese, default first. 筑紫明朝 comes from the same kind of font pack as
/// the Traditional pair.
const JAPANESE_FAMILIES: &[Family] = &[
    Family {
        name: "ゴシック",
        faces: &[
            "/usr/java/lib/fonts/TBGothicMed_213.ttf",
            "/usr/java/lib/fonts/TBGothicBold_213.ttf",
        ],
    },
    Family {
        name: "明朝",
        faces: &[
            "/usr/java/lib/fonts/TBMinchoMedium_213.ttf",
            "/usr/java/lib/fonts/TBMinchoBold_213.ttf",
        ],
    },
    Family {
        name: "筑紫明朝",
        faces: &[
            "/var/local/font/mnt/ja_font/fonts/TsukuMinPr5-Medium.ttf",
            "/var/local/font/mnt/ja_font/fonts/TsukuMinPr5-Bold.ttf",
        ],
    },
];

/// Korean, default first.
///
/// **All four are in `/usr/java/lib/fonts`**, the base firmware, on every
/// Kindle karyll runs on: a Korean document draws with nothing installed.
///
/// **Their sfnt tag is `OTTO`** — CFF outlines, and the `.otf` extension that
/// goes with them. `ab_glyph` reads the table. Each holds 13,727 glyphs of
/// Hangul, Latin and the CJK punctuation, with no Hanja and no kana, which is
/// what puts them at a tenth of a Han face's size.
///
/// **고딕 opens**, on the argument at the head of this file: a one-bit cut
/// thins a thin horizontal stroke out of existence, and 명조 is a Song-like
/// face that has them where 고딕 holds an even weight. The bold is a 900
/// against a 400 body, which is what the firmware carries.
const HANGUL_FAMILIES: &[Family] = &[
    Family {
        name: "고딕",
        faces: &[
            "/usr/java/lib/fonts/NotoSansKR-Regular.otf",
            "/usr/java/lib/fonts/NotoSansKR-Black.otf",
        ],
    },
    Family {
        name: "명조",
        faces: &[
            "/usr/java/lib/fonts/NotoSerifKR-Medium.otf",
            "/usr/java/lib/fonts/NotoSerifKR-Black.otf",
        ],
    },
];

/// What a group can be set to, best first. Never empty.
pub fn families(group: Group) -> &'static [Family] {
    match group {
        Group::Latin => LATIN_FAMILIES,
        Group::Han(Region::Simplified) => SIMPLIFIED_FAMILIES,
        Group::Han(Region::Traditional) => TRADITIONAL_FAMILIES,
        Group::Han(Region::Japanese) => JAPANESE_FAMILIES,
        Group::Hangul => HANGUL_FAMILIES,
    }
}

/// Which family each group is set to, as an index into [`families`].
///
/// The default is the head of every list, so a writer who never opens the panel
/// gets exactly the faces this file argues for.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Choices {
    picked: [usize; GROUPS.len()],
}

impl Choices {
    pub fn get(&self, group: Group) -> usize {
        self.picked[group_at(group)]
    }

    pub fn set(&mut self, group: Group, family: usize) {
        self.picked[group_at(group)] = family;
    }

    /// Read a stored selection: one `group name` pair per line.
    ///
    /// **Stored by name, not by index**, because an index is a position in a
    /// list that will be edited — inserting a family would silently move every
    /// writer onto a different face. A group or a name this build does not know
    /// is skipped and leaves that group on its default, so an older file, a
    /// hand-typed one, or one written by a build that carried a face this
    /// device does not, all degrade to the default rather than to nothing.
    pub fn parse(text: &str) -> Choices {
        let mut choices = Choices::default();
        for line in text.lines() {
            let mut parts = line.split_whitespace();
            let (Some(token), Some(name)) = (parts.next(), parts.next()) else {
                continue;
            };
            let Some(group) = GROUPS.into_iter().find(|g| g.token() == token) else {
                continue;
            };
            if let Some(at) = families(group).iter().position(|f| f.name == name) {
                choices.set(group, at);
            }
        }
        choices
    }

    pub fn render(&self) -> String {
        GROUPS
            .into_iter()
            .map(|group| {
                format!(
                    "{} {}\n",
                    group.token(),
                    family(group, self.get(group)).name
                )
            })
            .collect()
    }
}

/// Where `group` sits in [`GROUPS`], which is where its choice sits in
/// [`Choices`].
fn group_at(group: Group) -> usize {
    GROUPS
        .iter()
        .position(|g| *g == group)
        .expect("every group is listed")
}

/// The family a group is set to.
///
/// An index past the end falls back to the default rather than to the last
/// entry, which is the same answer [`Choices::parse`] gives a name it does not
/// know: when the stored choice cannot be honoured, the head of the list is
/// what this module argues for, and the tail is nothing in particular.
fn family(group: Group, chosen: usize) -> &'static Family {
    let list = families(group);
    list.get(chosen).unwrap_or(&list[0])
}

/// The families of `group` that are actually installed, as indices.
///
/// **Every face is checked, not just the body.** A family missing its italic
/// would not fall back to another italic — a Latin role has one face and then
/// the pan-Unicode fallback, so emphasis would come out in code2000. Declining
/// the whole entry is the honest answer, and every family listed here ships
/// complete — the Han faces on the firmware this was built against, the Latin
/// three in the extension.
pub fn available(group: Group) -> Vec<usize> {
    available_by(group, |path| Path::new(path).is_file())
}

/// The check itself, over any notion of "present", so it is tested without the
/// device's faces.
fn available_by(group: Group, exists: impl Fn(&str) -> bool) -> Vec<usize> {
    families(group)
        .iter()
        .enumerate()
        .filter(|(_, family)| family.faces.iter().all(|path| exists(path)))
        .map(|(at, _)| at)
        .collect()
}

/// Where each role is drawn from, given what each group is set to.
///
/// A chrome role answers [`CHROME_FACES`] whatever the choices are: it is not
/// part of any group, and nothing in the settings panel can re-point it.
fn path_for(role: Role, region: Region, choices: Choices) -> &'static str {
    if let Some(at) = chrome_at(role) {
        return CHROME_FACES[at];
    }
    let group = Group::of(role, region);
    family(group, choices.get(group)).faces[index_in(group, role)]
}

/// Where `role` sits among [`CHROME_ROLES`], or `None` if it draws a document.
fn chrome_at(role: Role) -> Option<usize> {
    CHROME_ROLES.iter().position(|r| *r == role)
}

/// Where `role` sits among the roles `group` covers.
fn index_in(group: Group, role: Role) -> usize {
    group
        .roles()
        .iter()
        .position(|r| *r == role)
        .expect("a group lists every role it covers")
}

/// Tried in order when the role's own face has no glyph for a character.
///
/// `code2000` is a pan-Unicode catch-all; `MTChineseSurrogates` carries the
/// rare Han outside the common blocks. Last resort before drawing nothing.
const FALLBACK: &[&str] = &[
    "/usr/java/lib/fonts/code2000.ttf",
    "/usr/java/lib/fonts/MTChineseSurrogates.ttf",
];

/// The Latin roles, which are the same face whichever convention is selected.
const LATIN_ROLES: [Role; 4] = [
    Role::Body,
    Role::BodyItalic,
    Role::BodyBold,
    Role::BodyBoldItalic,
];

/// The Han roles, each of which has a face **per region**.
const HAN_ROLES: [Role; 2] = [Role::Han, Role::HanBold];

/// The Hangul roles. One face each, at [`HANGUL_AT`]: no unification cuts them
/// three ways, and [`HANGUL_FAMILIES`] is the whole repertoire.
const HANGUL_ROLES: [Role; 2] = [Role::Hangul, Role::HangulBold];

/// The chrome roles, in [`CHROME_FACES`] order. One face each and no regional
/// cuts: karyll's own Latin is the same wherever it is read.
const CHROME_ROLES: [Role; 2] = [Role::Chrome, Role::ChromeBold];

const REGIONS: [Region; 3] = [Region::Simplified, Region::Traditional, Region::Japanese];

/// Where `(role, region)` sits in `slots`.
///
/// Latin first, then the Han roles with all three conventions of each side by
/// side, then Hangul, then the chrome and [`FALLBACK`]. Every pair has its own
/// slot and keeps it for the life of the process, which is what lets the
/// advance cache be keyed on the index rather than on a path.
///
/// A slot's *contents* do change, when the writer picks another family — and
/// that is precisely why [`Fonts::set_family`] evicts the cached advances of
/// the slots it re-points. A width belongs to the face that measured it.
fn slot_of(role: Role, region: Region) -> usize {
    if let Some(at) = chrome_at(role) {
        return CHROME_AT + at;
    }
    let group = Group::of(role, region);
    let at = index_in(group, role);
    match group {
        Group::Latin => at,
        Group::Han(region) => {
            let region = REGIONS
                .iter()
                .position(|r| *r == region)
                .expect("every region is listed");
            LATIN_ROLES.len() + at * REGIONS.len() + region
        }
        Group::Hangul => HANGUL_AT + at,
    }
}

/// Where the Hangul slots start.
const HANGUL_AT: usize = LATIN_ROLES.len() + HAN_ROLES.len() * REGIONS.len();

/// Where the chrome slots start.
const CHROME_AT: usize = HANGUL_AT + HANGUL_ROLES.len();

/// Where [`FALLBACK`] starts.
const FALLBACK_AT: usize = CHROME_AT + CHROME_ROLES.len();

/// The slots that may draw `role` under `region`, best first.
///
/// A Latin role has one face. **A Han role has the selected convention first and
/// the other two behind it**, because the three faces do not cover the same
/// characters and a document is not written in one convention. The Japanese face
/// is built to a JIS repertoire and has no 说 or 这; the TC faces have no
/// Simplified forms. Without the other two in the chain, selecting a language
/// does not re-cut the Han already on the page — it deletes whatever that face
/// has never heard of.
///
/// **A Hangul role has one face.** The Korean faces carry no Hanja and are
/// never asked for one: 한자 in Korean prose is
/// [`karyll_core::script::Script::Han`], and the Han chain draws that run.
///
/// The right glyph if the preferred face has it; the character in another
/// convention's shape if only another does. A wrong shape is a real cost and is
/// far cheaper than a character that does not appear.
fn chain_of(role: Role, region: Region) -> Vec<usize> {
    if !role.is_han() {
        return vec![slot_of(role, region)];
    }
    std::iter::once(region)
        .chain(REGIONS.iter().copied().filter(|r| *r != region))
        .map(|r| slot_of(role, r))
        .collect()
}

/// One slot per role, in the order [`slot_of`] files them.
///
/// [`FALLBACK`] is not here: it is not a role, and [`Fonts::resolve`] reaches
/// it from `FALLBACK_AT` whatever role was asked for. The one statement of the
/// order, so a slot index means the same face to every caller — the advance
/// cache is keyed on it.
fn role_slots(choices: Choices) -> impl Iterator<Item = Slot> {
    let region = Region::default();
    LATIN_ROLES
        .iter()
        .map(move |r| Slot::pending(path_for(*r, region, choices)))
        .chain(HAN_ROLES.iter().flat_map(move |r| {
            REGIONS
                .iter()
                .map(move |g| Slot::pending(path_for(*r, *g, choices)))
        }))
        .chain(
            HANGUL_ROLES
                .iter()
                .chain(CHROME_ROLES.iter())
                .map(move |r| Slot::pending(path_for(*r, region, choices))),
        )
}

enum State {
    /// On disk, not read yet.
    Pending,
    Loaded(Box<FontVec>),
    /// Missing, or failed to parse. Skipped from here on, so a bad candidate
    /// costs one failed attempt per session rather than one per character.
    Absent,
}

/// A face's vertical metrics, in ems, so they scale to any `px`.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Vertical {
    ascent: f32,
    /// Negative, as the font file reports it.
    descent: f32,
    gap: f32,
}

enum VerticalState {
    Unread,
    Missing,
    Known(Vertical),
}

struct Slot {
    path: &'static str,
    state: State,
    vertical: VerticalState,
}

impl Slot {
    fn pending(path: &'static str) -> Self {
        Self {
            path,
            state: State::Pending,
            vertical: VerticalState::Unread,
        }
    }

    /// How tall this face is, **without loading it**.
    ///
    /// A row has to be measured against every face that could draw in it, and
    /// there are three conventions of each Han role at 5–15 MB apiece. Loading
    /// them all to ask their height would cost more memory than drawing with
    /// them ever does. `hhea` and `head` are two small tables at a known offset,
    /// so this is three seeks and twenty bytes.
    fn vertical(&mut self) -> Option<Vertical> {
        if matches!(self.vertical, VerticalState::Unread) {
            self.vertical = match vertical_of(self.path) {
                Some(v) => VerticalState::Known(v),
                None => VerticalState::Missing,
            };
        }
        match self.vertical {
            VerticalState::Known(v) => Some(v),
            _ => None,
        }
    }

    /// Read the face if it has not been read, and hand it back.
    fn get(&mut self) -> Option<&FontVec> {
        if matches!(self.state, State::Pending) {
            self.state = match std::fs::read(self.path)
                .ok()
                .and_then(|bytes| FontVec::try_from_vec(bytes).ok())
            {
                Some(font) => State::Loaded(Box::new(font)),
                None => State::Absent,
            };
        }
        match &self.state {
            State::Loaded(font) => Some(font),
            _ => None,
        }
    }
}

/// The [`PxScale`] that draws a face with an em `px` pixels tall.
///
/// ab_glyph scales a face by `PxScale / hhea_height`, so `px` alone sets the em
/// of a 1000/1480 face to 0.68 of a 1000/1000 face's. `hhea_height` cancels it.
fn em_scale(px: f32, units_per_em: f32, hhea_height: f32) -> PxScale {
    PxScale::from(px * hhea_height / units_per_em)
}

/// [`em_scale`] for a loaded face. A face reporting no `unitsPerEm` takes `px`
/// as ab_glyph reads it.
fn scale_of(font: &FontVec, px: f32) -> PxScale {
    let height = font.height_unscaled();
    em_scale(px, font.units_per_em().unwrap_or(height), height)
}

/// `hhea` and `head` out of a font file, in ems.
///
/// Both are small tables at offsets the table directory names, so this reads the
/// 12-byte offset table, the directory, and twenty bytes — never the outlines,
/// which are all but a rounding error of the file.
///
/// The numbers are fractions of the em, so a face's ascent on screen is
/// `Vertical::ascent * px`.
fn vertical_of(path: &str) -> Option<Vertical> {
    vertical_in(&mut std::fs::File::open(path).ok()?)
}

/// The parse itself, over anything seekable, so it is tested against a font
/// built byte by byte rather than against files this machine does not have.
fn vertical_in(file: &mut (impl std::io::Read + std::io::Seek)) -> Option<Vertical> {
    let mut header = [0u8; 12];
    file.read_exact(&mut header).ok()?;
    // A font collection has a different header and would give a nonsense table
    // count. Nothing on this device is one; declining is better than guessing.
    if !matches!(&header[0..4], [0x00, 0x01, 0x00, 0x00] | b"true" | b"OTTO") {
        return None;
    }
    let tables = u16::from_be_bytes([header[4], header[5]]) as usize;
    let mut directory = vec![0u8; tables.checked_mul(16)?];
    file.read_exact(&mut directory).ok()?;
    let offset_of = |tag: &[u8; 4]| -> Option<u64> {
        directory
            .as_chunks::<16>()
            .0
            .iter()
            .find(|entry| &entry[0..4] == tag)
            .map(|entry| u32::from_be_bytes([entry[8], entry[9], entry[10], entry[11]]) as u64)
    };

    // head: unitsPerEm is the 2 bytes at +18, past version, revision, checksum
    // adjustment, magic and flags.
    let units = u16::from_be_bytes(read_at::<2, _>(file, offset_of(b"head")? + 18)?) as f32;
    if units <= 0.0 {
        return None;
    }
    // hhea: ascender, descender and lineGap are three i16 at +4, past version.
    let m = read_at::<6, _>(file, offset_of(b"hhea")? + 4)?;
    let em = |at: usize| i16::from_be_bytes([m[at], m[at + 1]]) as f32 / units;
    Some(Vertical {
        ascent: em(0),
        descent: em(2),
        gap: em(4),
    })
}

fn read_at<const N: usize, S: std::io::Read + std::io::Seek>(
    file: &mut S,
    at: u64,
) -> Option<[u8; N]> {
    file.seek(SeekFrom::Start(at)).ok()?;
    let mut buf = [0u8; N];
    file.read_exact(&mut buf).ok()?;
    Some(buf)
}

/// Cache key for a measured advance. Sizes come from a small fixed set — body
/// and the heading steps — so quantising to whole pixels loses nothing and
/// keeps the key hashable.
#[derive(Hash, PartialEq, Eq, Clone, Copy)]
struct Measured {
    face: u8,
    px: u16,
    ch: char,
}

pub struct Fonts {
    /// Role faces first, in [`LATIN_ROLES`], [`HAN_ROLES`] then
    /// [`HANGUL_ROLES`] order, then [`FALLBACK`].
    slots: Vec<Slot>,
    /// Advances are re-measured on every wrap, and wrapping runs on a 1 GHz
    /// ARM, so this is worth its memory.
    advances: HashMap<Measured, f32>,
    /// Which convention the Han slots are currently loaded for.
    region: Region,
    /// Which family each group is set to. Held here rather than beside the
    /// editor's other settings so that the faces in the slots and the names the
    /// panel shows cannot disagree.
    choices: Choices,
}

impl Fonts {
    /// Prepare the chain. Nothing is read yet.
    ///
    /// Fails only when no face is present at all, which means the firmware is
    /// not what this was built against. A single missing face is not a reason
    /// to refuse to start: the chain falls through and the text still draws.
    pub fn load(choices: Choices) -> Result<Self> {
        let region = Region::default();
        let slots: Vec<Slot> = role_slots(choices)
            .chain(FALLBACK.iter().map(|p| Slot::pending(p)))
            .collect();
        if !slots.iter().any(|s| Path::new(s.path).is_file()) {
            let tried: Vec<&str> = slots.iter().map(|s| s.path).collect();
            return Err(anyhow!("no usable font among {tried:?}"));
        }
        Ok(Self {
            slots,
            advances: HashMap::new(),
            region,
            choices,
        })
    }

    pub fn choices(&self) -> Choices {
        self.choices
    }

    /// The family `group` is currently set to.
    pub fn family(&self, group: Group) -> &'static Family {
        family(group, self.choices.get(group))
    }

    /// Draw a group in another family from here on.
    ///
    /// **The old face is dropped and its cached widths go with it.** A slot
    /// keeps its index for the life of the process and the advance cache is
    /// keyed on that index, so leaving the entries behind would measure the new
    /// face with the old one's widths — text laid out to a metric nothing on
    /// screen has. Assigning a fresh [`Slot`] also releases the `FontVec`, which
    /// for a Han face is some 10 MB, and leaves the vertical metrics unread so
    /// the row is re-measured against what will actually draw in it.
    ///
    /// Nothing repaints here. The caller has to lay the page out again, because
    /// two families are not the same height and every row moves.
    pub fn set_family(&mut self, group: Group, chosen: usize) {
        self.choices.set(group, chosen);
        for &role in group.roles() {
            let slot = slot_of(role, group.region());
            let path = path_for(role, group.region(), self.choices);
            if self.slots[slot].path == path {
                continue;
            }
            self.slots[slot] = Slot::pending(path);
            self.advances.retain(|key, _| key.face as usize != slot);
        }
    }

    /// Prefer another regional convention for Han from here on.
    ///
    /// **This changes which face is tried first, and nothing else.** The other
    /// conventions stay in the chain behind it, because a document is not
    /// written in one of them: a draft that mixes 简体, 繁體 and 日本語 needs
    /// all three repertoires available at once, and the three faces do not
    /// cover the same characters. The Japanese face is built to a JIS
    /// repertoire and has no 说 or 这; STHeitiTC has no Simplified forms.
    /// Swapping the faces on a language switch therefore did not re-cut those
    /// characters — it dropped them, and they stayed dropped after switching
    /// back to English, because a Latin language leaves the convention alone.
    ///
    /// Each `(role, region)` pair owns its slot for the life of the process, so
    /// the advance cache stays valid: a cached width belongs to the face that
    /// measured it rather than to whatever is loaded in that position now.
    pub fn set_region(&mut self, region: Region) {
        self.region = region;
    }

    /// Which convention the Han slots are loaded for.
    pub fn region(&self) -> Region {
        self.region
    }

    /// Faces this device actually has, in chain order. Worth logging at
    /// startup: a firmware that moved a face otherwise shows up only as text
    /// that draws in the wrong style.
    pub fn present(&self) -> impl Iterator<Item = &'static str> + '_ {
        self.slots
            .iter()
            .map(|s| s.path)
            .filter(|p| Path::new(p).is_file())
    }

    /// The slot that draws `ch` for `role`: the role's own face if it has the
    /// glyph, otherwise down the fallback chain. `None` when nothing has it.
    fn resolve(&mut self, role: Role, ch: char) -> Option<usize> {
        let n = self.slots.len();
        let mut order = chain_of(role, self.region)
            .into_iter()
            .chain(FALLBACK_AT..n);
        order.find(|&i| {
            i < n
                && self.slots[i]
                    .get()
                    .is_some_and(|font| font.glyph_id(ch).0 != 0)
        })
    }

    /// How far the pen moves after drawing `ch` at `px`.
    ///
    /// A character no face has measures zero rather than reserving space for a
    /// box that will not be drawn.
    fn advance_px(&mut self, role: Role, px: f32, ch: char) -> f32 {
        if karyll_core::script::is_invisible(ch) {
            return 0.0;
        }
        let Some(face) = self.resolve(role, ch) else {
            return 0.0;
        };
        let key = Measured {
            face: face as u8,
            px: px as u16,
            ch,
        };
        if let Some(hit) = self.advances.get(&key) {
            return *hit;
        }
        let advance = self.slots[face]
            .get()
            .map(|font| {
                font.as_scaled(scale_of(font, px))
                    .h_advance(font.glyph_id(ch))
            })
            .unwrap_or(0.0);
        self.advances.insert(key, advance);
        advance
    }

    /// The box a row occupies at `px`, as `(ascent, height)`, from the
    /// **extremes across every face the row can hold**.
    ///
    /// **A row must contain everything drawn in it.** Amazon Ember's ascent is
    /// shorter than a Han glyph is tall, so a row measured from the Latin face
    /// alone puts CJK several pixels above its own top edge — outside the
    /// rectangle that repaints it, inside the one above it, and sitting high
    /// against the Latin on the same line. Ascent is therefore the largest of
    /// the faces', descent the lowest, and the height spans both: the two can
    /// come from different faces, and adding the extremes is what makes the box
    /// hold either.
    ///
    /// Rows stay uniform across the page, which is the point of asking about a
    /// set of roles rather than one line's. The row's own Latin role is always
    /// in that set, so a row holding only Han keeps the height it has.
    ///
    /// **That anchor is [`Role::Chrome`] for a panel and [`Role::Body`] for a
    /// page**, and mixing them would put the document's face back into the
    /// panel's arithmetic — a settings row that grew and shrank as the writer
    /// tried faces on, which is the coupling the chrome slots exist to cut.
    ///
    /// **The whole chain is measured, not the selected convention's face**, and
    /// the fallbacks with it: `resolve` may reach any of them, and a face that
    /// can be drawn has to be one the row was sized for. It also makes the row a
    /// property of the *document* rather than of the language button — measuring
    /// only the selected convention moved the line spacing every time the button
    /// was pressed, since STHeiti and TBGothic are not the same height.
    ///
    /// Nothing here loads a face. [`Slot::vertical`] reads two tables, so a row
    /// can be measured against all three conventions for a few dozen bytes
    /// rather than 30 MB.
    fn row_box(&mut self, px: f32, roles: &[Role]) -> (f32, f32) {
        let mut ascent = f32::MIN;
        let mut descent = f32::MAX;
        let mut gap: f32 = 0.0;
        // Not `self.region`: a Han role is measured against all three
        // conventions, so the row is the same height whichever is selected.
        let mut measure = |slot: &mut Slot| {
            if let Some(v) = slot.vertical() {
                ascent = ascent.max(v.ascent * px);
                descent = descent.min(v.descent * px);
                gap = gap.max(v.gap * px);
            }
        };
        let anchor = if roles.iter().any(|r| r.is_chrome()) {
            Role::Chrome
        } else {
            Role::Body
        };
        for &role in roles.iter().chain(std::iter::once(&anchor)) {
            if role.is_han() {
                for region in REGIONS {
                    measure(&mut self.slots[slot_of(role, region)]);
                }
            } else {
                measure(&mut self.slots[slot_of(role, Region::default())]);
            }
        }
        for i in FALLBACK_AT..self.slots.len() {
            measure(&mut self.slots[i]);
        }
        if ascent == f32::MIN {
            // No face readable at all — the development machine, or a device
            // missing the file. Enough to lay out with rather than a panic.
            return (px, px * 1.2);
        }
        (ascent, ascent - descent + gap)
    }

    /// Rasterise `ch` and hand each covered pixel to `emit` as
    /// `(dx, dy, coverage)`, **offset from the pen position on the baseline**.
    ///
    /// The offsets are signed, and that is the point: `dy` is negative for the
    /// part of a glyph above the baseline, which is most of it. ab_glyph reports
    /// coverage relative to the glyph's own bounding box, so the box origin has
    /// to be added back — without it every glyph is placed by the top of its own
    /// bitmap rather than by the baseline, and a word comes out with its short
    /// letters sitting lower than its tall ones.
    ///
    /// Coverage is passed through rather than thresholded here — where the cut
    /// falls is the renderer's decision, and syntax marks dither instead of
    /// cutting.
    /// The box `ch` covers against the baseline, without rasterising it.
    fn ink_of(&mut self, role: Role, px: f32, ch: char) -> Option<ab_glyph::Rect> {
        let face = self.resolve(role, ch)?;
        let font = self.slots[face].get()?;
        let glyph = font.glyph_id(ch).with_scale(scale_of(font, px));
        Some(font.outline_glyph(glyph)?.px_bounds())
    }

    pub fn draw(
        &mut self,
        role: Role,
        px: f32,
        ch: char,
        mut emit: impl FnMut(i32, i32, f32),
    ) -> Option<ab_glyph::Rect> {
        let face = self.resolve(role, ch)?;
        let font = self.slots[face].get()?;
        let glyph = font.glyph_id(ch).with_scale(scale_of(font, px));
        let outlined = font.outline_glyph(glyph)?;
        let bounds = outlined.px_bounds();
        let (ox, oy) = (bounds.min.x as i32, bounds.min.y as i32);
        outlined.draw(|gx, gy, coverage| emit(ox + gx as i32, oy + gy as i32, coverage));
        Some(bounds)
    }
}

impl Metrics for Fonts {
    fn advance(&mut self, role: Role, px: f32, ch: char) -> f32 {
        self.advance_px(role, px, ch)
    }

    fn line_height(&mut self, px: f32, roles: &[Role]) -> f32 {
        self.row_box(px, roles).1
    }

    fn ascent(&mut self, px: f32, roles: &[Role]) -> f32 {
        self.row_box(px, roles).0
    }

    fn ink_box(&mut self, px: f32, roles: &[Role]) -> (f32, f32) {
        let (mut top, mut bottom) = (f32::MAX, f32::MIN);
        for &role in roles {
            if let Some(ink) = self.ink_of(role, px, probe(role)) {
                top = top.min(ink.min.y);
                bottom = bottom.max(ink.max.y);
            }
        }
        if top > bottom {
            // No face readable: a Latin cap places the label.
            return (-px * CAP, 0.0);
        }
        (top, bottom)
    }
}

/// A Latin cap, as a share of the em. Amazon Ember and the iA faces both draw
/// `H` to 0.711.
const CAP: f32 = 0.711;

/// A CJK glyph against the baseline: 中 and 한 both reach 0.842 above it, and
/// the deeper of the two drops 0.132 below.
#[cfg(test)]
const CJK_INK: (f32, f32) = (0.842, 0.132);

/// Metrics with no font behind them: every character ten units wide, so a test
/// can say exactly where it expects a caret or a box edge to land.
///
/// Here rather than in one module's tests because both the page and the panels
/// measure text, and two copies of a stub is two stubs that can disagree about
/// what a Han row costs — which is the one thing it exists to model.
#[cfg(test)]
pub struct Stub;

#[cfg(test)]
impl Metrics for Stub {
    fn advance(&mut self, _role: Role, _px: f32, _ch: char) -> f32 {
        10.0
    }

    /// A CJK role makes the row a fifth taller, standing in for the real faces:
    /// those glyphs are taller than Amazon Ember's ascent, and every bug this
    /// stub exists to catch comes from a row that does not know it.
    fn line_height(&mut self, px: f32, roles: &[Role]) -> f32 {
        if roles.iter().any(is_cjk) {
            px * 1.2
        } else {
            px
        }
    }

    fn ascent(&mut self, px: f32, roles: &[Role]) -> f32 {
        if roles.iter().any(is_cjk) {
            px
        } else {
            px * 0.8
        }
    }

    /// The proportions the real faces draw to.
    fn ink_box(&mut self, px: f32, roles: &[Role]) -> (f32, f32) {
        if roles.iter().any(is_cjk) {
            (-px * CJK_INK.0, px * CJK_INK.1)
        } else {
            (-px * CAP, 0.0)
        }
    }
}

/// Metrics in the proportions the real faces have: a CJK glyph is one em wide
/// and a Latin one about half.
///
/// [`Stub`]'s flat ten units are easier to check arithmetic against, and they
/// say nothing at all about how wide CJK draws — a Han character and a comma
/// cost the same there. Anything asking whether text *fits* has to know the
/// difference, because the answer is where every fitting bug comes from: ten
/// four-character candidates want more than a 7″ panel is wide and less than a
/// 10.2″ one is, and a stub that measures them alike sees neither case.
///
/// **Half an em is a stress figure for the panels**, which are set in Amazon
/// Ember at about 0.37 em. The iA faces set on a 0.6 em base.
#[cfg(test)]
pub struct Proportional;

#[cfg(test)]
impl Metrics for Proportional {
    fn advance(&mut self, role: Role, px: f32, _ch: char) -> f32 {
        if is_cjk(&role) { px } else { px * 0.5 }
    }

    fn line_height(&mut self, px: f32, roles: &[Role]) -> f32 {
        Stub.line_height(px, roles)
    }

    fn ascent(&mut self, px: f32, roles: &[Role]) -> f32 {
        Stub.ascent(px, roles)
    }

    fn ink_box(&mut self, px: f32, roles: &[Role]) -> (f32, f32) {
        Stub.ink_box(px, roles)
    }
}

/// A role drawn from a CJK face: one em wide, and taller than a Latin ascent.
/// Han and Hangul take the same room.
#[cfg(test)]
fn is_cjk(role: &Role) -> bool {
    role.is_han() || role.is_hangul()
}

#[cfg(test)]
mod tests {
    use super::*;

    // These run on a development machine, where none of the device's faces
    // exist. They cover the parts that do not need the files.

    fn all_roles() -> impl Iterator<Item = Role> {
        LATIN_ROLES
            .into_iter()
            .chain(HAN_ROLES)
            .chain(HANGUL_ROLES)
            .chain(CHROME_ROLES)
    }

    /// Every family for every group, which is what most of these check.
    fn all_families() -> impl Iterator<Item = (Group, usize, &'static Family)> {
        GROUPS.into_iter().flat_map(|group| {
            families(group)
                .iter()
                .enumerate()
                .map(move |(at, family)| (group, at, family))
        })
    }

    #[test]
    fn every_role_and_region_maps_to_a_face_and_its_own_slot() {
        let mut taken = std::collections::HashSet::new();
        for role in all_roles() {
            for region in REGIONS {
                assert!(!path_for(role, region, Choices::default()).is_empty());
                let slot = slot_of(role, region);
                assert!(slot < FALLBACK_AT, "{role:?} is past the fallbacks");
                // A Latin role is one slot across all three conventions; a Han
                // role is three. The advance cache is keyed on this index, so a
                // collision would measure one face with another's widths.
                if role.is_han() {
                    assert!(taken.insert(slot), "{role:?}/{region:?} shares a slot");
                }
            }
        }
        let own: std::collections::HashSet<usize> = LATIN_ROLES
            .iter()
            .chain(&CHROME_ROLES)
            .map(|r| slot_of(*r, Region::Simplified))
            .collect();
        assert_eq!(own.len(), LATIN_ROLES.len() + CHROME_ROLES.len());
        assert!(own.iter().all(|i| !taken.contains(i)));
    }

    /// Chrome draws from its own slots, and nothing the writer can set reaches
    /// them. A chrome role sharing the body's slot would restyle the whole app
    /// the moment a document face was picked.
    #[test]
    fn chrome_is_pinned_and_no_setting_can_move_it() {
        for (at, role) in CHROME_ROLES.into_iter().enumerate() {
            assert!(role.is_chrome());
            assert!(!role.is_han());
            for region in REGIONS {
                // Whatever any group is set to, chrome answers Ember.
                let mut choices = Choices::default();
                for group in GROUPS {
                    choices.set(group, families(group).len() - 1);
                }
                assert_eq!(path_for(role, region, choices), CHROME_FACES[at]);
                assert!(CHROME_FACES[at].contains("Amazon-Ember"));
                // One slot across every convention, as a Latin role is.
                assert_eq!(slot_of(role, region), CHROME_AT + at);
            }
        }
        // The document's Latin body is a different face and a different slot,
        // which is the whole separation.
        assert_ne!(
            slot_of(Role::Chrome, Region::default()),
            slot_of(Role::Body, Region::default())
        );
        assert_ne!(
            path_for(Role::Chrome, Region::default(), Choices::default()),
            path_for(Role::Body, Region::default(), Choices::default())
        );
        // And no group lists a chrome role, so `set_family` never re-points one.
        for group in GROUPS {
            assert!(group.roles().iter().all(|r| !r.is_chrome()), "{group:?}");
        }
    }

    /// **One size is one em, in every face karyll draws with**, across the
    /// whole of [`crate::render::SIZES`]. The pairs are
    /// `(unitsPerEm, ascender − descender)`, read from the faces themselves.
    #[test]
    fn one_size_is_one_em_whatever_the_face_reports() {
        let faces = [
            ("STHeitiMedium", 1000.0, 1000.0),
            ("TBGothicMed_213", 256.0, 256.0),
            ("NotoSansKR-Regular", 1000.0, 1480.0),
            ("NotoSerifKR-Medium", 1000.0, 1437.0),
            ("iAWriterDuoS-Regular", 1000.0, 1300.0),
            ("Amazon-Ember-Regular", 1000.0, 1254.0),
            ("code2000", 2048.0, 2600.0),
        ];
        for px in crate::render::SIZES {
            for (name, units, height) in faces {
                // The factor ab_glyph applies to the face's own units.
                let factor = em_scale(px, units, height).y / height;
                assert!(
                    (factor * units - px).abs() < 1e-3,
                    "{name} draws an em of {} at a size of {px}",
                    factor * units
                );
            }
        }
    }

    /// A panel row is measured against Ember, a page row against the document's
    /// face. Sharing the anchor put the writer's face back into the panel's
    /// arithmetic, so a settings row changed height as faces were tried on.
    #[test]
    fn a_panel_row_is_not_measured_against_the_document_face() {
        assert_eq!(LATIN_ROW, &[Role::Chrome]);
        assert!(LATIN_ROW.iter().all(|r| r.is_chrome()));
    }

    /// A font file carrying nothing but a `head` and an `hhea`, which is all
    /// the row box reads.
    fn synthetic(units: u16, ascender: i16, descender: i16, gap: i16) -> Vec<u8> {
        let head_at = 12u32 + 32;
        let hhea_at = head_at + 54;
        let mut out = Vec::new();
        out.extend_from_slice(&0x0001_0000u32.to_be_bytes());
        out.extend_from_slice(&2u16.to_be_bytes()); // numTables
        out.extend_from_slice(&[0; 6]); // searchRange, entrySelector, rangeShift
        for (tag, at, len) in [(b"head", head_at, 54u32), (b"hhea", hhea_at, 36)] {
            out.extend_from_slice(tag);
            out.extend_from_slice(&[0; 4]); // checksum
            out.extend_from_slice(&at.to_be_bytes());
            out.extend_from_slice(&len.to_be_bytes());
        }
        let mut head = vec![0u8; 54];
        head[18..20].copy_from_slice(&units.to_be_bytes());
        out.extend_from_slice(&head);
        let mut hhea = vec![0u8; 36];
        hhea[4..6].copy_from_slice(&ascender.to_be_bytes());
        hhea[6..8].copy_from_slice(&descender.to_be_bytes());
        hhea[8..10].copy_from_slice(&gap.to_be_bytes());
        out.extend_from_slice(&hhea);
        out
    }

    #[test]
    fn vertical_metrics_come_out_of_the_tables_in_ems() {
        let font = synthetic(1000, 880, -120, 50);
        let got = vertical_in(&mut std::io::Cursor::new(font)).expect("a parseable font");
        assert_eq!(got.ascent, 0.88);
        assert_eq!(
            got.descent, -0.12,
            "descent stays negative, as the file has it"
        );
        assert_eq!(got.gap, 0.05);

        // 2048 units per em is as common as 1000, and reading it from `head`
        // rather than assuming is the whole reason that table is opened.
        let font = synthetic(2048, 1024, -512, 0);
        let got = vertical_in(&mut std::io::Cursor::new(font)).expect("a parseable font");
        assert_eq!(got.ascent, 0.5);
        assert_eq!(got.descent, -0.25);
    }

    #[test]
    fn something_that_is_not_a_font_measures_nothing() {
        assert_eq!(vertical_in(&mut std::io::Cursor::new(b"".to_vec())), None);
        assert_eq!(
            vertical_in(&mut std::io::Cursor::new(b"ttcf\0\0\0\0\0\0\0\0".to_vec())),
            None,
            "a collection has another header and would give a nonsense count"
        );
        let mut truncated = synthetic(1000, 880, -120, 0);
        truncated.truncate(40);
        assert_eq!(vertical_in(&mut std::io::Cursor::new(truncated)), None);
    }

    #[test]
    fn a_han_role_can_reach_every_convention_preferred_one_first() {
        for region in REGIONS {
            let chain = chain_of(Role::Han, region);
            assert_eq!(
                chain[0],
                slot_of(Role::Han, region),
                "{region:?} comes first"
            );
            let reachable: std::collections::HashSet<usize> = chain.into_iter().collect();
            for other in REGIONS {
                assert!(
                    reachable.contains(&slot_of(Role::Han, other)),
                    "{region:?} cannot reach {other:?}, so its characters vanish"
                );
            }
        }
    }

    #[test]
    fn a_latin_role_is_one_face_whatever_the_convention() {
        for region in REGIONS {
            assert_eq!(chain_of(Role::Body, region).len(), 1);
        }
    }

    /// The row is a property of the document, not of the language button.
    /// Measuring only the selected convention moved the line spacing every time
    /// the button was pressed, because STHeiti and TBGothic are not the same
    /// height — so 简体 and 繁體 set wider than 日本語, English and German.
    #[test]
    fn the_row_is_the_same_height_in_every_convention() {
        let Ok(mut fonts) = Fonts::load(Choices::default()) else {
            return; // No device faces here; the device run is the check.
        };
        let roles = [Role::Body, Role::Han];
        let mut boxes = Vec::new();
        for region in REGIONS {
            fonts.set_region(region);
            boxes.push(fonts.row_box(46.0, &roles));
        }
        assert!(
            boxes.windows(2).all(|w| w[0] == w[1]),
            "line spacing follows the button: {boxes:?}"
        );
    }

    /// In every Latin family, not only the default: a synthetic slant is not
    /// emphasis, and an entry that cannot supply four faces does not belong on
    /// the list.
    #[test]
    fn latin_roles_are_four_distinct_files() {
        for family in LATIN_FAMILIES {
            let paths: std::collections::HashSet<&str> = family.faces.iter().copied().collect();
            assert_eq!(
                paths.len(),
                4,
                "{}: emphasis must be a real face, not a synthetic slant",
                family.name
            );
        }
    }

    /// The Latin faces are the same whatever the Han convention: Amazon Ember
    /// does not have regional cuts and should not be re-read on a switch.
    #[test]
    fn only_the_han_roles_depend_on_the_region() {
        for role in all_roles() {
            let paths: std::collections::HashSet<&str> = REGIONS
                .iter()
                .map(|r| path_for(role, *r, Choices::default()))
                .collect();
            assert_eq!(paths.len(), if role.is_han() { 3 } else { 1 }, "{role:?}");
        }
    }

    /// **A CJK family is one design at two weights**, because emphasis is a
    /// mark against the character rather than a second face — an entry that
    /// reached outside its own design for the bold would set a bold word in a
    /// family the writer did not choose.
    #[test]
    fn a_cjk_family_is_one_design_bodied_and_bolded() {
        for (group, _, family) in all_families() {
            let [body, bold] = group.roles() else {
                assert!(
                    matches!(group, Group::Latin),
                    "{} is no pair",
                    group.label()
                );
                continue;
            };
            assert_ne!(
                family.faces[index_in(group, *body)],
                family.faces[index_in(group, *bold)],
                "{} sets bold in its own body face",
                family.name
            );
            assert_eq!(family.faces.len(), 2, "{} is not a pair", family.name);
        }
        // The sans by default in every writing system — 黑体 for Chinese,
        // ゴシック for Japanese, 고딕 for Korean.
        let default = Choices::default();
        let ko = Region::default();
        assert!(path_for(Role::Han, Region::Simplified, default).contains("STHeiti"));
        assert!(path_for(Role::HanBold, Region::Simplified, default).contains("STHeitiBold"));
        assert!(path_for(Role::Han, Region::Japanese, default).contains("TBGothic"));
        assert!(path_for(Role::HanBold, Region::Japanese, default).contains("TBGothicBold"));
        assert!(path_for(Role::Hangul, ko, default).contains("NotoSansKR-Regular"));
        assert!(path_for(Role::HangulBold, ko, default).contains("NotoSansKR-Black"));
    }

    /// Traditional Chinese and Japanese must not be drawn from the Simplified
    /// faces. Han unification gives them the same code points, so the failure
    /// is not a missing glyph — it is the wrong glyph, silently, which is the
    /// kind of bug that survives a screenshot.
    /// Not just the defaults: no convention may reach another's faces through
    /// *any* entry on its list, or picking 楷體 would be a way to have
    /// Traditional set in Simplified glyphs.
    #[test]
    fn each_convention_gets_its_own_han_faces() {
        let faces = |group| -> std::collections::HashSet<&str> {
            families(group)
                .iter()
                .flat_map(|f| f.faces.iter().copied())
                .collect()
        };
        for one in REGIONS {
            for other in REGIONS.iter().filter(|r| **r != one) {
                let shared: Vec<&str> = faces(Group::Han(one))
                    .intersection(&faces(Group::Han(*other)))
                    .copied()
                    .collect();
                assert!(
                    shared.is_empty(),
                    "{one:?} draws {other:?}'s faces: {shared:?}"
                );
            }
        }
        assert!(path_for(Role::Han, Region::Traditional, Choices::default()).contains("TC"));
    }

    /// Every group has something to offer, which [`family`] indexes into
    /// without checking, and every entry supplies exactly the roles its group
    /// covers — a short list would draw one role from another's face.
    #[test]
    fn every_group_has_families_and_every_family_is_complete() {
        for group in GROUPS {
            assert!(!families(group).is_empty(), "{group:?}");
        }
        for (group, _, family) in all_families() {
            assert_eq!(
                family.faces.len(),
                group.roles().len(),
                "{} has the wrong number of faces",
                family.name
            );
        }
    }

    /// Names are the key a choice is stored under, so two entries sharing one
    /// in a group would make the setting ambiguous. Across groups is fine and
    /// happens: 黑体 and 黑體 are the same family in two conventions.
    #[test]
    fn family_names_are_unique_within_a_group() {
        for group in GROUPS {
            let names: std::collections::HashSet<&str> =
                families(group).iter().map(|f| f.name).collect();
            assert_eq!(names.len(), families(group).len(), "{group:?}");
        }
    }

    /// The head of each list is what the app shipped with, so a writer who
    /// never opens the panel is exactly where they were.
    #[test]
    fn the_default_choice_is_the_face_this_module_argues_for() {
        let default = Choices::default();
        for group in GROUPS {
            assert_eq!(default.get(group), 0, "{group:?}");
        }
        assert_eq!(
            path_for(Role::Body, Region::Simplified, default),
            "/mnt/us/extensions/karyll/fonts/iAWriterDuoS-Regular.ttf",
            "a page opens in the face iA Writer itself defaults to"
        );
        assert_eq!(
            path_for(Role::Chrome, Region::Simplified, default),
            "/usr/java/lib/fonts/Amazon-Ember-Regular.ttf",
            "the app stays in the Kindle's own face"
        );
        assert_eq!(
            path_for(Role::Han, Region::Simplified, default),
            "/usr/java/lib/fonts/STHeitiMedium.ttf"
        );
        assert_eq!(
            path_for(Role::Han, Region::Traditional, default),
            "/usr/java/lib/fonts/STHeitiTC.ttf"
        );
        assert_eq!(
            path_for(Role::Han, Region::Japanese, default),
            "/usr/java/lib/fonts/TBGothicMed_213.ttf"
        );
    }

    #[test]
    fn a_choice_survives_the_round_trip_through_the_file() {
        let mut choices = Choices::default();
        choices.set(Group::Latin, 1);
        choices.set(Group::Han(Region::Japanese), 2);
        assert_eq!(Choices::parse(&choices.render()), choices);
        // By name, so inserting a family at the head of a list would not move a
        // stored choice onto the wrong face.
        assert!(choices.render().contains("latin Mono"));
        assert!(choices.render().contains("ja 筑紫明朝"));
    }

    #[test]
    fn a_stored_line_this_build_does_not_know_leaves_the_default() {
        let stored = "latin Quattro\nkr 바탕\nja Helvetica\nsc\n\ntc 楷體 and more\n";
        let got = Choices::parse(stored);
        assert_eq!(got.get(Group::Latin), 2, "a line it knows still applies");
        assert_eq!(got.get(Group::Han(Region::Japanese)), 0, "unknown name");
        assert_eq!(got.get(Group::Han(Region::Simplified)), 0, "no name at all");
        assert_eq!(
            got.get(Group::Han(Region::Traditional)),
            1,
            "trailing words are ignored rather than failing the line"
        );
    }

    /// A family removed by a later build leaves a stored index past the end.
    /// The head of the list is the answer; a panic is not.
    #[test]
    fn a_choice_past_the_end_of_the_list_falls_back() {
        let mut choices = Choices::default();
        choices.set(Group::Latin, 99);
        assert_eq!(
            path_for(Role::Body, Region::Simplified, choices),
            path_for(Role::Body, Region::Simplified, Choices::default())
        );
    }

    /// Every face a document can be set in ships with karyll and lives under
    /// the extension. The chrome does not: the app has to be able to draw
    /// itself on a device whose storage is away being read over USB, where a
    /// page falls through to [`FALLBACK`] and the panels stay in Ember.
    #[test]
    fn the_bundled_families_are_the_ones_karyll_ships() {
        const BUNDLED: &str = "/mnt/us/extensions/karyll/fonts/";
        let shipped: Vec<&str> = LATIN_FAMILIES
            .iter()
            .filter(|f| f.faces.iter().all(|p| p.starts_with(BUNDLED)))
            .map(|f| f.name)
            .collect();
        assert_eq!(shipped, vec!["Duo", "Mono", "Quattro"]);
        // Chrome is never bundled: the app has to draw itself on a device whose
        // storage is away being read over USB.
        assert!(CHROME_FACES.iter().all(|p| !p.starts_with(BUNDLED)));
        // No entry may straddle the two: a family half on the firmware and half
        // in the extension would survive `available` with /mnt/us unmounted and
        // then fail to load the half that is gone.
        for family in LATIN_FAMILIES {
            let bundled = family.faces.iter().filter(|p| p.starts_with(BUNDLED));
            assert!(
                bundled.clone().count() == 0 || bundled.count() == family.faces.len(),
                "{} draws from both the firmware and the extension",
                family.name
            );
        }
        assert!(
            available_by(Group::Latin, |path| !path.starts_with(BUNDLED)).is_empty(),
            "the writing faces are the extension's, all of them"
        );
    }

    #[test]
    fn only_families_whose_every_face_is_installed_are_offered() {
        let all = available_by(Group::Latin, |_| true);
        assert_eq!(all, vec![0, 1, 2]);
        assert!(available_by(Group::Latin, |_| false).is_empty());
        // One missing italic is enough: a Latin role has no second face to fall
        // back to, so emphasis would come out of the pan-Unicode fallback.
        let no_italic = available_by(Group::Latin, |path| !path.contains("Italic"));
        assert!(!no_italic.contains(&0), "Duo offered without its italic");
    }

    #[test]
    fn loading_fails_cleanly_when_no_face_is_present() {
        // The development machine has no /usr/java/lib/fonts, which is exactly
        // the "wrong firmware" case.
        let err = match Fonts::load(Choices::default()) {
            Err(err) => err,
            Ok(_) => panic!("no device faces exist on a development machine"),
        };
        assert!(err.to_string().contains("no usable font"));
    }

    fn bare(choices: Choices) -> Fonts {
        Fonts {
            slots: role_slots(choices).collect(),
            region: Region::default(),
            advances: HashMap::new(),
            choices,
        }
    }

    /// The advance cache is keyed on the slot index, and a family change is the
    /// one thing that puts a different face in a slot. Left behind, the cached
    /// widths would lay Mono out to Duo's metrics.
    #[test]
    fn changing_a_family_drops_the_widths_measured_with_the_old_one() {
        let mut fonts = bare(Choices::default());
        let latin = slot_of(Role::Body, Region::Simplified);
        let han = slot_of(Role::Han, Region::Simplified);
        let chrome = slot_of(Role::Chrome, Region::Simplified);
        for face in [latin, han, chrome] {
            fonts.advances.insert(
                Measured {
                    face: face as u8,
                    px: 46,
                    ch: 'a',
                },
                12.0,
            );
        }

        fonts.set_family(Group::Latin, 1);

        assert_eq!(fonts.choices().get(Group::Latin), 1);
        assert_eq!(fonts.family(Group::Latin).name, "Mono");
        assert!(fonts.slots[latin].path.contains("iAWriterMonoS"));
        assert!(
            !fonts.advances.keys().any(|key| key.face as usize == latin),
            "Duo's widths outlived Duo"
        );
        assert!(
            fonts.advances.keys().any(|key| key.face as usize == han),
            "a group nobody touched lost its cache"
        );
        // Chrome is not in any group, so a document face change cannot evict it
        // — nor draw the panel in what was just picked for the page.
        assert!(
            fonts.advances.keys().any(|key| key.face as usize == chrome),
            "the panel was re-measured for a change it does not follow"
        );
        assert!(fonts.slots[chrome].path.contains("Amazon-Ember"));
    }

    /// Every role of the group moves, not only the body: picking 明朝 and
    /// getting ゴシック back the moment something is emboldened is a half
    /// applied setting. Emphasis moves *to* the sans, because that is what the
    /// pairing turns into when the body is the serif.
    #[test]
    fn changing_a_family_re_points_every_role_it_covers() {
        let mut fonts = bare(Choices::default());
        fonts.set_family(Group::Han(Region::Japanese), 1);
        for role in HAN_ROLES {
            let slot = slot_of(role, Region::Japanese);
            assert_ne!(
                fonts.slots[slot].path,
                path_for(role, Region::Japanese, Choices::default()),
                "{role:?} still draws from ゴシック's face"
            );
        }
        assert!(
            fonts.slots[slot_of(Role::Han, Region::Japanese)]
                .path
                .contains("TBMincho")
        );
        assert!(
            fonts.slots[slot_of(Role::HanBold, Region::Japanese)]
                .path
                .contains("TBMinchoBold")
        );
        // And the other conventions are untouched: they are chosen separately.
        assert!(
            fonts.slots[slot_of(Role::Han, Region::Simplified)]
                .path
                .contains("STHeiti")
        );
    }

    #[test]
    fn invisible_characters_measure_zero() {
        let mut fonts = Fonts {
            slots: Vec::new(),
            region: Region::Simplified,
            advances: HashMap::new(),
            choices: Choices::default(),
        };
        // No faces at all, but the invisible check comes first and short
        // circuits before any lookup.
        assert_eq!(fonts.advance_px(Role::Body, 32.0, '\u{200B}'), 0.0);
    }
}
