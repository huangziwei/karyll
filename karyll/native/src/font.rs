//! Loading faces, and measuring with them. Nothing is compiled in: the
//! firmware's faces are read from `/usr/java/lib/fonts`, the three writing
//! faces from `fonts/` beside this extension. A face is read on first use.

use std::collections::HashMap;
use std::io::SeekFrom;
use std::path::Path;

use ab_glyph::{Font as _, FontVec, PxScale, ScaleFont as _};
use anyhow::{Result, anyhow};
use karyll_core::script::{Region, Role};

/// The measurements layout needs, separated from the faces that supply them.
/// The goal column, page overlap and where a caret lands are all arithmetic
/// over this, and [`Stub`] supplies it off the device.
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

/// A row that only ever holds Latin: chrome, panel titles, any text karyll
/// wrote itself.
pub const LATIN_ROW: &[Role] = &[Role::Chrome];

/// A set of roles chosen between as one: the Latin four, one convention's Han
/// pair, or Hangul's. Each Han convention chooses its own faces; Latin chooses
/// once for every language written in it, and Hangul takes no region.
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

    /// What the settings panel calls this row, under its Type heading. Each
    /// names itself in its own script, with no 字体/書体 after it.
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
    /// itself in its own script.
    pub name: &'static str,
    /// One path per role in [`Group::roles`] order.
    faces: &'static [&'static str],
}

/// The faces karyll's own text is drawn in, in [`CHROME_ROLES`] order. Amazon
/// Ember, and no setting reaches it: the geometry [`crate::ui`] lays out
/// against it holds for the process.
const CHROME_FACES: [&str; 2] = [
    "/usr/java/lib/fonts/Amazon-Ember-Regular.ttf",
    "/usr/java/lib/fonts/Amazon-Ember-Bold.ttf",
];

/// The Latin families a *document* can be set in, default first. One design at
/// a shared 0.6 em base in three widths: Mono holds one width, Duo widens six
/// letters by half, Quattro allows four. Each is a true four-face family.
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

/// Simplified Chinese, default first. A body face and its bold: emphasis is a
/// 着重号 against each character and leaves the face alone.
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

/// Traditional Chinese, default first. 楷體 and 圓體 live in
/// `/var/local/font`, a font pack, and are the entries most often absent.
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

/// Korean, default first. All four are in `/usr/java/lib/fonts` on every
/// Kindle karyll runs on. Their sfnt tag is `OTTO`, and each holds 13,727
/// glyphs of Hangul, Latin and CJK punctuation, with no Hanja and no kana.
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

/// Which family each group is set to, as an index into [`families`]. The
/// default is the head of every list.
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

    /// Read a stored selection: one `group name` pair per line. A group or a
    /// name this build does not know is skipped and leaves that group on its
    /// default.
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

/// The family a group is set to. An index past the end falls back to the
/// default, the same answer [`Choices::parse`] gives a name it does not know.
fn family(group: Group, chosen: usize) -> &'static Family {
    let list = families(group);
    list.get(chosen).unwrap_or(&list[0])
}

/// The families of `group` that are installed, as indices. Every face is
/// checked, the italic and the bold with the body: a Latin role has one face
/// and then [`FALLBACK`].
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

/// Where each role is drawn from, given what each group is set to. A chrome
/// role answers [`CHROME_FACES`] whatever the choices are.
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
/// `code2000` is a pan-Unicode catch-all; `MTChineseSurrogates` carries the
/// rare Han outside the common blocks.
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

/// Where `(role, region)` sits in `slots`. Latin first, then the Han roles
/// with all three conventions of each side by side, then Hangul, then the
/// chrome and [`FALLBACK`]. Every pair keeps its slot for the process.
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

/// The slots that may draw `role` under `region`, best first. A Latin role has
/// one face; a Han role has the selected convention first and the other two
/// behind it; a Hangul role has one face and is never asked for Hanja.
fn chain_of(role: Role, region: Region) -> Vec<usize> {
    if !role.is_han() {
        return vec![slot_of(role, region)];
    }
    std::iter::once(region)
        .chain(REGIONS.iter().copied().filter(|r| *r != region))
        .map(|r| slot_of(role, r))
        .collect()
}

/// One slot per role, in the order [`slot_of`] files them. [`FALLBACK`] is not
/// here — [`Fonts::resolve`] reaches it from `FALLBACK_AT` whatever role was
/// asked for. The one statement of the order the advance cache is keyed on.
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
    /// Missing, or failed to parse. Skipped from here on, at one failed attempt
    /// per session.
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

    /// How tall this face is, without loading it. `hhea` and `head` are two
    /// small tables at a known offset: three seeks and twenty bytes.
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

/// The [`PxScale`] that draws a face with an em `px` pixels tall. ab_glyph
/// scales by `PxScale / hhea_height`, and `hhea_height` cancels it.
fn em_scale(px: f32, units_per_em: f32, hhea_height: f32) -> PxScale {
    PxScale::from(px * hhea_height / units_per_em)
}

/// [`em_scale`] for a loaded face. A face reporting no `unitsPerEm` takes `px`
/// as ab_glyph reads it.
fn scale_of(font: &FontVec, px: f32) -> PxScale {
    let height = font.height_unscaled();
    em_scale(px, font.units_per_em().unwrap_or(height), height)
}

/// `hhea` and `head` out of a font file, in ems: the 12-byte offset table, the
/// directory, and twenty bytes. A face's ascent on screen is
/// `Vertical::ascent * px`.
fn vertical_of(path: &str) -> Option<Vertical> {
    vertical_in(&mut std::fs::File::open(path).ok()?)
}

/// The parse itself, over anything seekable: the tests build a font byte by
/// byte.
fn vertical_in(file: &mut (impl std::io::Read + std::io::Seek)) -> Option<Vertical> {
    let mut header = [0u8; 12];
    file.read_exact(&mut header).ok()?;
    // A font collection has a different header and a nonsense table count.
    // Nothing on this device is one.
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
    /// One [`Fonts::centring`] per face and probe, in ems.
    centres: HashMap<(u8, char), f32>,
    /// Which convention the Han slots are currently loaded for.
    region: Region,
    /// Which family each group is set to. Held beside the slots it points at,
    /// which keeps the faces loaded and the names the panel shows in step.
    choices: Choices,
}

impl Fonts {
    /// Prepare the chain. Nothing is read yet. Fails only where no face is
    /// present at all; a single missing face falls through the chain.
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
            centres: HashMap::new(),
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

    /// Draw a group in another family from here on. A fresh [`Slot`] releases
    /// the `FontVec`, drops the cached widths keyed on its index, and leaves
    /// the metrics unread. Nothing repaints; the caller lays the page out.
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
            self.centres.retain(|(face, _), _| *face as usize != slot);
        }
    }

    /// Prefer another regional convention for Han from here on. The other
    /// conventions stay in the chain behind it, and each `(role, region)` pair
    /// owns its slot for the life of the process.
    pub fn set_region(&mut self, region: Region) {
        self.region = region;
    }

    /// Which convention the Han slots are loaded for.
    pub fn region(&self) -> Region {
        self.region
    }

    /// Faces this device actually has, in chain order. Logged at startup: a
    /// firmware that moved a face shows up as text in the wrong style.
    pub fn present(&self) -> impl Iterator<Item = &'static str> + '_ {
        self.slots
            .iter()
            .map(|s| s.path)
            .filter(|p| Path::new(p).is_file())
    }

    /// The slot that draws `ch` for `role`: the role's own face where it has
    /// the glyph, then down the fallback chain. `None` when nothing has it.
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

    /// How far the pen moves after drawing `ch` at `px`. A character no face
    /// has measures zero.
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

    /// The box a row occupies at `px`, as `(ascent, height)`, taken across
    /// every face `roles` can reach: the largest ascent, the lowest descent.
    /// [`Slot::vertical`] reads two tables, not a face.
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
            // missing the file. Enough to lay out with.
            return (px, px * 1.2);
        }
        (ascent, ascent - descent + gap)
    }

    /// Rasterise `ch` and hand each covered pixel to `emit` as
    /// `(dx, dy, coverage)`, offset from the pen on the baseline. `dy` is
    /// negative above it, and coverage is passed through unthresholded.
    fn centring(&mut self, face: usize, role: Role) -> f32 {
        if !(role.is_han() || role.is_hangul()) {
            return 0.0;
        }
        let probe = probe(role);
        let key = (face as u8, probe);
        if let Some(hit) = self.centres.get(&key) {
            return *hit;
        }
        // `px_bounds` is whole pixels; `CENTRING_PX` makes that a rounding
        // error in an answer every size scales from.
        let centre = self.slots[face].get().and_then(|font| {
            let scale = scale_of(font, CENTRING_PX);
            let ink = font
                .outline_glyph(font.glyph_id(probe).with_scale(scale))?
                .px_bounds();
            Some((ink.min.y + ink.max.y) / 2.0 / CENTRING_PX)
        });
        let drop = centre.map_or(0.0, |centre| -CJK_CENTRE - centre);
        self.centres.insert(key, drop);
        drop
    }

    /// The box `ch` covers against the baseline, without rasterising it.
    fn ink_of(&mut self, role: Role, px: f32, ch: char) -> Option<ab_glyph::Rect> {
        let face = self.resolve(role, ch)?;
        let drop = self.centring(face, role) * px;
        let font = self.slots[face].get()?;
        let glyph = font.glyph_id(ch).with_scale(scale_of(font, px));
        let mut ink = font.outline_glyph(glyph)?.px_bounds();
        ink.min.y += drop;
        ink.max.y += drop;
        Some(ink)
    }

    pub fn draw(
        &mut self,
        role: Role,
        px: f32,
        ch: char,
        mut emit: impl FnMut(i32, i32, f32),
    ) -> Option<ab_glyph::Rect> {
        let face = self.resolve(role, ch)?;
        let drop = (self.centring(face, role) * px).round() as i32;
        let font = self.slots[face].get()?;
        let glyph = font.glyph_id(ch).with_scale(scale_of(font, px));
        let outlined = font.outline_glyph(glyph)?;
        let bounds = outlined.px_bounds();
        let (ox, oy) = (bounds.min.x as i32, bounds.min.y as i32 + drop);
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

/// A Latin cap, in ems: Amazon Ember and the iA faces draw `H` to 0.711.
const CAP: f32 = 0.711;

/// Where CJK ink is centred above the baseline, in ems. Half a [`CAP`].
const CJK_CENTRE: f32 = CAP / 2.0;

/// The em [`Fonts::centring`] measures at. Large, and never drawn.
const CENTRING_PX: f32 = 512.0;

/// A CJK glyph against the baseline: 中 and 한 both reach 0.842 above it, and
/// the deeper of the two drops 0.132 below.
#[cfg(test)]
const CJK_INK: (f32, f32) = (0.842, 0.132);

/// Metrics with no font behind them: every character ten units wide, so a test
/// can say exactly where it expects a caret or a box edge to land. Both the
/// page and the panels measure text, and both reach this one.
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

/// Metrics in the proportions the real faces have: a CJK glyph one em wide, a
/// Latin one about half. Amazon Ember sets at about 0.37 em and the iA faces
/// on a 0.6 em base, so half an em is a stress figure for the panels.
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
                // role is three. The advance cache is keyed on this index.
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

    /// Chrome draws from its own slots, and no setting reaches them. A chrome
    /// role sharing the body's slot restyles the app on every document face.
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

    /// The CJK faces centre 中 and 한 between 0.348 and 0.413 of an em above
    /// the baseline; [`Fonts::centring`] brings every one onto [`CJK_CENTRE`].
    /// The centres are read from the faces, one per entry in [`families`].
    #[test]
    fn every_cjk_family_puts_its_ink_at_one_height() {
        let centres = [
            ("SC 黑体", -0.359),
            ("SC 宋体", -0.402),
            ("TC 黑體", -0.413),
            ("TC 楷體", -0.359),
            ("TC 圓體", -0.402),
            ("JA ゴシック", -0.348),
            ("JA 明朝", -0.348),
            ("JA 筑紫明朝", -0.380),
            ("KO 고딕", -0.380),
            ("KO 명조", -0.391),
        ];
        for px in crate::render::SIZES {
            for (name, centre) in centres {
                let drop = -CJK_CENTRE - centre;
                let landed = (centre + drop) * px;
                assert!(
                    (landed - -CJK_CENTRE * px).abs() < 0.5,
                    "{name} lands at {landed} against {} at a size of {px}",
                    -CJK_CENTRE * px
                );
            }
        }
        // The spread is worth more than two pixels at the default size.
        let raw: Vec<f32> = centres.iter().map(|(_, c)| *c).collect();
        let spread = raw.iter().cloned().fold(f32::MIN, f32::max)
            - raw.iter().cloned().fold(f32::MAX, f32::min);
        assert!(
            spread * crate::render::DEFAULT_SIZE > 2.0,
            "{spread} em is not worth correcting"
        );
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

        // 2048 units per em is as common as 1000. `head` carries the number,
        // which is what opens that table.
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
    /// STHeiti and TBGothic are not the same height, and the row takes the
    /// tallest whichever convention is selected.
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

    /// A CJK family is one design at two weights. Emphasis is a mark against
    /// the character, and an entry reaching outside its own design for the bold
    /// sets a bold word in a family [`Choices`] never named.
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
    /// faces: Han unification gives them the same code points, and the failure
    /// is the wrong glyph. No entry on a list may reach another's faces.
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
    /// covers.
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

    /// Names are the key a choice is stored under, and one group holds each
    /// once. Across groups they repeat: 黑体 and 黑體 in two conventions.
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
        // By name. An index is a position in a list that gets edited.
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

    /// Every face a document can be set in ships under the extension. The
    /// chrome does not: a page falls through to [`FALLBACK`] and the panels
    /// stay in Ember while the storage is away being read over USB.
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
        // No entry may straddle the two. A family half on the firmware and half
        // in the extension passes `available` with /mnt/us unmounted, with half
        // its faces gone.
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
        // back to, and emphasis comes out of the pan-Unicode fallback.
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
            centres: HashMap::new(),
            choices,
        }
    }

    /// The advance cache is keyed on the slot index, and a family change is the
    /// one thing that puts a different face in a slot. Left behind, the cached
    /// widths lays Mono out to Duo's metrics.
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

    /// Every role of the group moves, not the body alone. Emphasis moves to
    /// the sans, which is what the pairing turns into under a serif body.
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
            centres: HashMap::new(),
            choices: Choices::default(),
        };
        // No faces at all, but the invisible check comes first and short
        // circuits before any lookup.
        assert_eq!(fonts.advance_px(Role::Body, 32.0, '\u{200B}'), 0.0);
    }
}
