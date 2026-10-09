//! Colours, taken from the active Omarchy theme.
//!
//! Omarchy renders the template this reader installed — the configuration
//! directory's `omarchy/themed/omaread.toml.tpl`, wherever the environment
//! puts that directory — on every theme change and writes the result into the
//! current theme directory. The reader reads that file and falls back to
//! built-in colours when it is absent, so it works on a machine without
//! Omarchy just as well.
//!
//! The file is re-read when its timestamp changes. A theme change replaces the
//! whole directory by an atomic swap, so the path points at a new file; checking
//! the timestamp of the path catches that, where a watch on the file itself would
//! lose track of it.
//!
//! The file is only half the story: the terminal paints the page, so its own
//! colours are asked for directly (its default background and foreground, its
//! sixteen slots, its light/dark answer) and the two are merged. Omarchy sets
//! the terminal's colours from the same theme the file was rendered from, so
//! where both answer they agree — and where the terminal is not Omarchy's to
//! configure, its answers win over a desktop theme drawn for another screen.
//! A terminal that says nothing changes nothing: the file, or the built-ins.

use crate::paths;
use serde::Deserialize;
use std::path::PathBuf;
use std::time::{Duration, SystemTime};

/// A colour as 24-bit RGB.
pub type Rgb = (u8, u8, u8);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Theme {
    pub background: Rgb,
    pub foreground: Rgb,
    /// Headings and progress.
    pub accent: Rgb,
    /// Rules, image labels, the status line.
    pub muted: Rgb,
    pub code_background: Rgb,
    pub code_foreground: Rgb,
    pub quote: Rgb,
    /// The five mark colours, in the order the template writes them:
    /// yellow, green, blue, red, purple.
    pub marks: [Rgb; 5],
}

impl Default for Theme {
    fn default() -> Self {
        Self {
            background: (0x1a, 0x1b, 0x26),
            foreground: (0xc8, 0xd3, 0xf5),
            accent: (0xff, 0xc7, 0x77),
            muted: (0x7a, 0x88, 0xa8),
            code_background: (0x24, 0x28, 0x36),
            code_foreground: (0x86, 0xe1, 0xfc),
            quote: (0x9a, 0xa5, 0xce),
            marks: BUILT_IN_MARKS,
        }
    }
}

/// Mark colours that are known to tell each other apart. Used when a theme
/// offers no five distinguishable ones.
const BUILT_IN_MARKS: [Rgb; 5] = [
    (0xd9, 0xb8, 0x4c), // yellow
    (0x6f, 0xb8, 0x6f), // green
    (0x6a, 0x9f, 0xd8), // blue
    (0xe0, 0x7a, 0x6a), // red
    (0xb4, 0x8a, 0xd8), // purple
];

/// The file as written on disk. Every field is optional, so a partial template
/// still works.
#[derive(Debug, Deserialize, Default)]
struct File {
    #[serde(default)]
    colors: Colors,
}

#[derive(Debug, Deserialize, Default)]
struct Colors {
    background: Option<String>,
    foreground: Option<String>,
    accent: Option<String>,
    muted: Option<String>,
    code_background: Option<String>,
    code_foreground: Option<String>,
    quote: Option<String>,
    mark_yellow: Option<String>,
    mark_green: Option<String>,
    mark_blue: Option<String>,
    mark_red: Option<String>,
    mark_purple: Option<String>,
}

/// Watches the theme file and hands out the current colours.
pub struct Watcher {
    /// Where the rendered file may live, newest layout first — the same two
    /// `paths` offers. Kept rather than only the answer: the file is rendered
    /// from the template on the next theme change, so it can be missing when
    /// the watcher is made and appear later.
    candidates: Vec<PathBuf>,
    /// The candidate being watched, once one of them is there. A path already
    /// picked is kept: a theme switch swaps the directory underneath it.
    path: Option<PathBuf>,
    seen: Option<SystemTime>,
    /// The file's colours, or nothing when there is no file: nothing is not a
    /// broken state but the state of a machine without Omarchy, where the
    /// colours come from the terminal's palette instead.
    file: Option<Theme>,
    /// What the terminal answered the last time it was asked. Handed in at
    /// startup — where the picture questions share the same raw-mode window —
    /// and asked again after a theme switch, which re-colours the terminal too.
    terminal: TerminalColors,
}

impl Watcher {
    pub fn new(terminal: TerminalColors) -> Self {
        let mut watcher = Self {
            candidates: paths::omarchy_theme_files().unwrap_or_default(),
            path: None,
            seen: None,
            file: None,
            terminal,
        };
        watcher.resolve();
        watcher.reload();
        watcher
    }

    /// The colours to draw with now: the file's roles and the terminal's page,
    /// merged and corrected for contrast.
    pub fn theme(&self) -> Theme {
        from_terminal(&self.terminal, self.file)
    }

    /// Re-reads the file and asks the terminal again when it changed.
    ///
    /// A theme switch re-colours the terminal as well as the file — both come
    /// from the same Omarchy theme — so the startup answers are stale after
    /// one, and a reader that kept them would paint the new page in the old
    /// screen's colours. A terminal that answers the second time as poorly as
    /// the first keeps the file's colours, which is where it stood anyway.
    pub fn follow(&mut self) -> bool {
        if !self.refresh() {
            return false;
        }
        self.terminal = query();
        true
    }

    /// Picks the candidate to watch, while nothing is being watched yet.
    fn resolve(&mut self) {
        if self.path.is_none() {
            self.path = self.candidates.iter().find(|path| path.exists()).cloned();
        }
    }

    /// Re-reads the file if it changed. Returns true when the colours moved.
    fn refresh(&mut self) -> bool {
        // A file that was not there when the watcher was made appears on the
        // next theme change, and a watcher that froze its answer then would
        // leave the reader on its built-in colours for the whole session.
        // Two `exists` calls at most, and only while there is nothing to watch.
        self.resolve();
        let Some(path) = &self.path else {
            return false;
        };
        let stamp = std::fs::metadata(path).and_then(|m| m.modified()).ok();
        if stamp == self.seen {
            return false;
        }
        let before = self.file;
        self.reload();
        before != self.file
    }

    fn reload(&mut self) {
        let Some(path) = &self.path else { return };
        self.seen = std::fs::metadata(path).and_then(|m| m.modified()).ok();
        self.file = std::fs::read_to_string(path).ok().map(|text| parse(&text));
    }
}

/// The template Omarchy renders on every theme change. It is built into the
/// binary, so a downloaded reader sets itself up without a checkout.
const TEMPLATE: &str = include_str!("../themed/omaread.toml.tpl");

/// Puts the template in place, so the reader follows the theme from the first
/// start onwards. Reports whether it wrote something.
///
/// Nothing happens when the file is already there. It may be a link into a
/// checkout, or hold colours someone changed on purpose, and neither should be
/// overwritten by a program start.
pub fn install_template() -> bool {
    let Ok(target) = paths::omarchy_theme_template() else {
        return false;
    };
    if !write_template(&target) {
        return false;
    }
    // Without this the template stays unrendered until the next theme change,
    // which would leave the first session with the built-in colours.
    if let Some(theme) = current_theme_name() {
        apply_theme(&theme);
    }
    true
}

/// Writes the template unless something is there already.
fn write_template(target: &std::path::Path) -> bool {
    // symlink_metadata rather than exists: a link into a checkout counts as
    // present even where its target moved away.
    if target.symlink_metadata().is_ok() {
        return false;
    }
    let Some(themed) = target.parent() else {
        return false;
    };
    // Only where Omarchy lives. Elsewhere nothing would render the template and
    // the directory would be litter.
    if !themed.parent().is_some_and(|omarchy| omarchy.is_dir()) {
        return false;
    }
    std::fs::create_dir_all(themed).is_ok() && std::fs::write(target, TEMPLATE).is_ok()
}

/// The theme in use, from wherever this Omarchy release records it.
fn current_theme_name() -> Option<String> {
    theme_name_in(&paths::omarchy_theme_name_files().ok()?)
}

/// The first of these files that answers, trimmed: the newest layout wins,
/// and an absent file falls through to the older one.
fn theme_name_in(files: &[PathBuf]) -> Option<String> {
    files
        .iter()
        .find_map(|path| std::fs::read_to_string(path).ok())
        .map(|name| name.trim().to_string())
        .filter(|name| !name.is_empty())
}

/// Re-applies a theme, which renders every template including ours.
///
/// The wallpaper is left alone: re-applying it would flash the desktop for a
/// step that is about text colours. Output is discarded because the reader is
/// about to take over the screen.
fn apply_theme(name: &str) {
    let _ = std::process::Command::new("omarchy-theme-set")
        .arg(name)
        .env("OMARCHY_THEME_SKIP_BACKGROUND", "1")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status();
}

fn parse(text: &str) -> Theme {
    let file: File = match toml::from_str(text) {
        Ok(file) => file,
        // A malformed file must not leave the reader colourless.
        Err(_) => return Theme::default(),
    };
    let fallback = Theme::default();
    let colors = file.colors;
    let pick =
        |value: &Option<String>, default: Rgb| value.as_deref().and_then(hex).unwrap_or(default);

    let marks = [
        pick(&colors.mark_yellow, BUILT_IN_MARKS[0]),
        pick(&colors.mark_green, BUILT_IN_MARKS[1]),
        pick(&colors.mark_blue, BUILT_IN_MARKS[2]),
        pick(&colors.mark_red, BUILT_IN_MARKS[3]),
        pick(&colors.mark_purple, BUILT_IN_MARKS[4]),
    ];

    let mut theme = Theme {
        background: pick(&colors.background, fallback.background),
        foreground: pick(&colors.foreground, fallback.foreground),
        accent: pick(&colors.accent, fallback.accent),
        muted: pick(&colors.muted, fallback.muted),
        code_background: pick(&colors.code_background, fallback.code_background),
        code_foreground: pick(&colors.code_foreground, fallback.code_foreground),
        quote: pick(&colors.quote, fallback.quote),
        // Marks carry meaning, so they have to be told apart. Not every theme
        // offers five that are: some map red and purple onto one accent. They
        // are pulled apart in brightness below, so the five stay colours the
        // theme itself uses.
        marks,
    };
    separate_marks(&mut theme.marks, theme.background);
    theme
}

/// Pulls colliding marks apart in brightness alone.
///
/// A theme may map red and purple onto the same accent, and reaching for a
/// built-in set would hand the reader a palette its own theme never agreed to.
/// Moving a mark toward the end of the scale until it stands off the ones
/// before it keeps every mark a colour the theme already uses, and keeps the
/// five a coherent scale rather than a mixture.
fn separate_marks(marks: &mut [Rgb; 5], background: Rgb) {
    let towards = if luminance(background) <= 0.5 { WHITE } else { BLACK };
    for index in 1..marks.len() {
        let mut steps = 0;
        while marks[..index]
            .iter()
            .any(|earlier| distance(*earlier, marks[index]) < MIN_MARK_DISTANCE)
            && steps < 12
        {
            marks[index] = blend(marks[index], towards, 0.12);
            steps += 1;
        }
    }
}

/// Squared distance in RGB below which two colours read as the same mark.
const MIN_MARK_DISTANCE: u32 = 3000;

#[cfg(test)]
fn all_distinguishable(marks: &[Rgb; 5]) -> bool {
    for (i, a) in marks.iter().enumerate() {
        for b in marks.iter().skip(i + 1) {
            if distance(*a, *b) < MIN_MARK_DISTANCE {
                return false;
            }
        }
    }
    true
}

fn distance((ar, ag, ab): Rgb, (br, bg, bb): Rgb) -> u32 {
    let d = |x: u8, y: u8| (x as i32 - y as i32).pow(2) as u32;
    d(ar, br) + d(ag, bg) + d(ab, bb)
}

/// Parses `#rrggbb`, with or without the hash.
fn hex(text: &str) -> Option<Rgb> {
    let text = text.trim().trim_start_matches('#');
    if text.len() != 6 || !text.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    let byte = |at: usize| u8::from_str_radix(&text[at..at + 2], 16).ok();
    Some((byte(0)?, byte(2)?, byte(4)?))
}

/// The ends of the scale, not colours the reader paints: they answer whether a
/// background leaves room for readable text at all.
const WHITE: Rgb = (255, 255, 255);
const BLACK: Rgb = (0, 0, 0);

/// Relative luminance per WCAG 2.1.
fn luminance((r, g, b): Rgb) -> f32 {
    fn channel(value: u8) -> f32 {
        let v = value as f32 / 255.0;
        if v <= 0.03928 {
            v / 12.92
        } else {
            ((v + 0.055) / 1.055).powf(2.4)
        }
    }
    0.2126 * channel(r) + 0.7152 * channel(g) + 0.0722 * channel(b)
}

/// Contrast ratio between two colours, per WCAG 2.1.
pub fn contrast_ratio(a: Rgb, b: Rgb) -> f32 {
    let (l1, l2) = (luminance(a), luminance(b));
    let (lighter, darker) = if l1 >= l2 { (l1, l2) } else { (l2, l1) };
    (lighter + 0.05) / (darker + 0.05)
}

/// Picks the text colour that reads best on a coloured background.
///
/// A terminal has no transparency: a background colour replaces the text
/// instead of letting it through, so both colours are set here. That keeps the
/// text legible in any theme, whatever the terminal behind it looks like.
pub fn text_color_on(background: Rgb, theme: &Theme) -> Rgb {
    // Text on a coloured patch is drawn in the page's own two colours: the
    // theme's foreground or its background, whichever stands off the patch
    // better. Nothing is invented here — and when even the better one falls
    // short, it is lifted in brightness alone, never turned into another hue.
    let foreground = contrast_ratio(background, theme.foreground);
    let behind = contrast_ratio(background, theme.background);
    let picked = if behind >= foreground {
        theme.background
    } else {
        theme.foreground
    };
    reach_contrast(picked, background, BODY_CONTRAST)
}

// ---------------------------------------------------------------------------
// Asking the terminal.
// ---------------------------------------------------------------------------

/// What is asked of the terminal, in one write: its default background, its
/// default foreground, its sixteen palette slots, and whether it considers
/// itself light or dark. Together so that one wait covers them all.
pub const ASK: &str = concat!(
    "\x1b]11;?\x1b\\",
    "\x1b]10;?\x1b\\",
    "\x1b]4;0;?\x1b\\\x1b]4;1;?\x1b\\\x1b]4;2;?\x1b\\\x1b]4;3;?\x1b\\",
    "\x1b]4;4;?\x1b\\\x1b]4;5;?\x1b\\\x1b]4;6;?\x1b\\\x1b]4;7;?\x1b\\",
    "\x1b]4;8;?\x1b\\\x1b]4;9;?\x1b\\\x1b]4;10;?\x1b\\\x1b]4;11;?\x1b\\",
    "\x1b]4;12;?\x1b\\\x1b]4;13;?\x1b\\\x1b]4;14;?\x1b\\\x1b]4;15;?\x1b\\",
    "\x1b[?996n",
);

/// How long the answers are waited for.
///
/// A terminal that answers does so in milliseconds — the wait only covers the
/// round trip — and one that never answers must cost the start no more than
/// this, after which the reader draws with the colours it already had.
pub const TIMEOUT: Duration = Duration::from_millis(100);

/// How quiet the input must fall before the answers count as complete.
///
/// The replies arrive in one burst; waiting past that would be waiting out the
/// full timeout on a terminal that has already said everything it will.
pub const QUIET: Duration = Duration::from_millis(20);

/// Asks the terminal its colours and reads the answer.
///
/// The session asks once at startup, where the picture questions share the
/// raw-mode window; this is the asking that comes later — after a theme switch
/// has re-coloured the terminal — where raw mode is already on.
fn query() -> TerminalColors {
    TerminalColors::parse(&crate::tty::gather(ASK, TIMEOUT, QUIET))
}

/// Which way a terminal's colours run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Appearance {
    Dark,
    Light,
}

/// What the terminal said about its own colours: everything the reader can
/// learn by asking, and nothing at all when it does not answer.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct TerminalColors {
    pub background: Option<Rgb>,
    pub foreground: Option<Rgb>,
    /// All sixteen slots or none of them: a few slots of a palette say
    /// nothing about the sixteen colours the roles would be derived from.
    pub palette: Option<[Rgb; 16]>,
    /// The terminal's light/dark report, where it gives one (true: light).
    pub light: Option<bool>,
}

impl TerminalColors {
    /// Reads the answers out of what came back.
    ///
    /// The reply is a scrap heap: the answers that were asked for, whatever
    /// else the terminal volunteers, and any keystroke typed while the
    /// question was out. Only the shapes that were asked for are read; the
    /// rest passes by unread, and so does anything malformed — a terminal
    /// wording a colour differently must not stop the reader from drawing.
    pub fn parse(reply: &str) -> Self {
        let mut colors = Self::default();
        let mut slots = [None; 16];
        let mut at = 0;
        while let Some(found) = reply[at..].find("\x1b]") {
            let body = at + found + "\x1b]".len();
            let rest = &reply[body..];
            // Every operating system command ends in BEL or in ST. One that
            // ends in neither was cut off mid-flight, and what follows cannot
            // be trusted to hold whole answers.
            let (len, stop) = match (rest.find('\x07'), rest.find("\x1b\\")) {
                (Some(bel), Some(st)) if bel < st => (bel, 1),
                (Some(_), Some(st)) => (st, 2),
                (Some(bel), None) => (bel, 1),
                (None, Some(st)) => (st, 2),
                (None, None) => return Self::complete(colors, slots, reply),
            };
            let mut parts = rest[..len].splitn(3, ';');
            match parts.next().unwrap_or_default() {
                "10" => colors.foreground = parts.next().and_then(reported),
                "11" => colors.background = parts.next().and_then(reported),
                "4" => {
                    let slot = parts.next().and_then(|slot| slot.parse::<usize>().ok());
                    if let (Some(slot), Some(colour)) =
                        (slot, parts.next().and_then(reported))
                        && let Some(slot) = slots.get_mut(slot)
                    {
                        *slot = Some(colour);
                    }
                }
                _ => {}
            }
            at = body + len + stop;
        }
        Self::complete(colors, slots, reply)
    }

    /// Closes the parse with the parts that are counted rather than keyed:
    /// the palette as a whole, and the light/dark report
    /// (`CSI ? 997 ; 1 n` says dark, `; 2 n` says light).
    fn complete(mut colors: Self, slots: [Option<Rgb>; 16], reply: &str) -> Self {
        colors.palette = slots
            .iter()
            .all(Option::is_some)
            .then(|| std::array::from_fn(|index| slots[index].expect("checked above")));

        const REPORT: &str = "\x1b[?997;";
        let mut at = 0;
        while let Some(found) = reply[at..].find(REPORT) {
            let start = at + found + REPORT.len();
            let rest = &reply[start..];
            let digits = rest
                .find(|c: char| !c.is_ascii_digit())
                .unwrap_or(rest.len());
            let ends = rest.as_bytes().get(digits) == Some(&b'n');
            match &rest[..digits] {
                "1" if ends => colors.light = Some(false),
                "2" if ends => colors.light = Some(true),
                _ => {}
            }
            // Past the digits and their terminator when there is one, so the
            // next search cannot find this report again.
            at = start + digits + usize::from(digits < rest.len());
        }
        colors
    }

    /// Whether the terminal considers itself light or dark.
    ///
    /// From its own colours where it reported both: the direction of the text
    /// says which scheme the page belongs to — but only on a background that
    /// can carry text at all, since against a mid-grey neither black nor white
    /// reads and the direction says nothing. Otherwise from the terminal's
    /// light/dark report, then `COLORFGBG` — `fg;bg` as palette indices, where
    /// a background from the dark half of the palette means dark — and dark
    /// when nothing speaks.
    pub fn appearance(&self) -> Appearance {
        self.appearance_from(std::env::var("COLORFGBG").ok().as_deref())
    }

    fn appearance_from(&self, colorfgbg: Option<&str>) -> Appearance {
        if let (Some(background), Some(foreground)) = (self.background, self.foreground) {
            // The direction of the terminal's own text says which scheme its
            // page belongs to — but only where text can be read at all:
            // against a mid grey neither black nor white reaches body
            // contrast, and then the direction says nothing.
            let direction = if luminance(foreground) > luminance(background) {
                Appearance::Dark
            } else {
                Appearance::Light
            };
            let own_text = if direction == Appearance::Dark { WHITE } else { BLACK };
            if contrast_ratio(background, own_text) >= BODY_CONTRAST {
                return direction;
            }
        }
        if let Some(light) = self.light {
            return if light { Appearance::Light } else { Appearance::Dark };
        }
        colorfgbg
            .and_then(|value| value.rsplit(';').next())
            .and_then(|index| index.trim().parse::<u8>().ok())
            .filter(|index| *index <= 15)
            .map_or(Appearance::Dark, |index| {
                if index < 8 { Appearance::Dark } else { Appearance::Light }
            })
    }
}

/// One colour as a terminal spells it: `rgb:RRRR/GGGG/BBBB`, `rgb:RR/GG/BB`
/// or `#rrggbb`. Anything else is no answer.
fn reported(value: &str) -> Option<Rgb> {
    let value = value.trim();
    if value.starts_with('#') {
        return hex(value);
    }
    if !value.get(..4)?.eq_ignore_ascii_case("rgb:") {
        return None;
    }
    let mut parts = value[4..].split('/');
    let channels = (channel(parts.next()?)?, channel(parts.next()?)?, channel(parts.next()?)?);
    parts.next().is_none().then_some(channels)
}

/// One channel: two hex digits as written, or four scaled down to the byte
/// this reader draws with.
fn channel(text: &str) -> Option<u8> {
    if text.len() != 2 && text.len() != 4 {
        return None;
    }
    if !text.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return None;
    }
    let value = u32::from_str_radix(text, 16).ok()?;
    Some(if text.len() == 2 {
        value as u8
    } else {
        (value * 255 / 65535) as u8
    })
}

/// The contrast body text needs to be read as prose (WCAG AA).
const BODY_CONTRAST: f32 = 4.5;

/// The contrast a colour needs to be told apart from the page it sits on.
const ROLE_CONTRAST: f32 = 3.0;

/// The colours to draw with: an Omarchy theme's roles where there is one, the
/// terminal's own palette where there is not, and in both cases the page the
/// terminal reports — every role then corrected until it reaches the contrast
/// its job needs on that page.
///
/// A terminal that answers nothing gets exactly what the reader had before it
/// asked: the file's colours untouched, or the built-ins. Asking must not
/// change the answer a silent terminal was already giving.
pub fn from_terminal(terminal: &TerminalColors, omarchy: Option<Theme>) -> Theme {
    let mut theme = match (omarchy, terminal.palette) {
        (Some(theme), _) => theme,
        (None, Some(palette)) => from_palette(&palette, terminal),
        (None, None) => Theme::default(),
    };
    if let Some(background) = terminal.background {
        theme.background = background;
    }
    if let Some(foreground) = terminal.foreground {
        theme.foreground = foreground;
    }
    // Only when the terminal said something about colours. With nothing
    // reported the final background is the file's own, and the file's roles
    // were mixed against it — correcting them would repaint a choice that was
    // made deliberately for that very background.
    if terminal.background.is_some()
        || terminal.foreground.is_some()
        || terminal.palette.is_some()
    {
        correct(&mut theme);
    }
    theme
}

/// The palette slots the five marks are read from: yellow, green, blue, red,
/// purple — the order the template writes them, each with its bright twin
/// eight slots along.
const MARK_SLOTS: [usize; 5] = [3, 2, 4, 1, 5];

/// Roles from the sixteen slots of a terminal palette, for a reader with no
/// Omarchy file to take them from.
///
/// A palette's slots are the terminal's vocabulary by name and whatever the
/// theme likes by nature in fact, so each role takes the slot nearest its job
/// that reaches the contrast the role needs — and the page itself comes from
/// the terminal's answers where it gave them.
fn from_palette(palette: &[Rgb; 16], terminal: &TerminalColors) -> Theme {
    let background = terminal.background.unwrap_or_else(|| {
        if terminal.appearance() == Appearance::Light {
            palette[15]
        } else {
            palette[0]
        }
    });
    let foreground = terminal.foreground.unwrap_or_else(|| {
        if terminal.appearance() == Appearance::Light {
            palette[0]
        } else {
            palette[15]
        }
    });
    let readable = |colour: Rgb| {
        (contrast_ratio(colour, background) >= ROLE_CONTRAST).then_some(colour)
    };
    let accent = readable(palette[11])
        .or_else(|| readable(palette[3]))
        .unwrap_or(foreground);
    let muted = readable(palette[8]).unwrap_or_else(|| blend(background, foreground, 0.5));
    let code_foreground = readable(palette[14])
        .or_else(|| readable(palette[6]))
        .unwrap_or(foreground);
    let quote = readable(palette[13])
        .or_else(|| readable(palette[5]))
        .unwrap_or(foreground);
    let marks = MARK_SLOTS.map(|slot| {
        // The bright twin is for the page that shows it: bright colours carry
        // a dark page, the dull ones a light page, and contrast is the whole
        // of what distinguishes the two here.
        if contrast_ratio(palette[slot], background)
            >= contrast_ratio(palette[slot + 8], background)
        {
            palette[slot]
        } else {
            palette[slot + 8]
        }
    });
    let mut theme = Theme {
        background,
        foreground,
        accent,
        muted,
        // A touch of the text's light mixed into the page, the way the
        // Omarchy template makes a code block sit slightly apart from it.
        code_background: blend(background, foreground, 0.1),
        code_foreground,
        quote,
        // Marks carry meaning, so five that cannot be told apart are worse
        // than none: colliding slots are pulled apart in brightness, the one
        // dimension a palette does not lose when it moves.
        marks,
    };
    separate_marks(&mut theme.marks, theme.background);
    theme
}

/// Pushes every colour to the contrast its job needs, against the background
/// the page will actually be read on.
///
/// A palette's colours are picked for a desktop and a theme file's for the
/// screen behind it: neither owes the text sitting on *this* background a
/// readable ratio, and the terminal's own reported colours may be no better —
/// so the floor is applied to all of them alike, at the ratio each job needs.
fn correct(theme: &mut Theme) {
    let background = theme.background;
    theme.foreground = reach_contrast(theme.foreground, background, BODY_CONTRAST);
    for role in [
        &mut theme.accent,
        &mut theme.muted,
        &mut theme.quote,
        &mut theme.code_foreground,
    ] {
        *role = reach_contrast(*role, background, ROLE_CONTRAST);
    }
    for mark in &mut theme.marks {
        *mark = reach_contrast(*mark, background, ROLE_CONTRAST);
    }
}

/// Steps a colour toward white or black until it reaches `ratio` on
/// `background`.
///
/// Plain sRGB: the goal is a floor, not a target, so the first step that
/// clears it wins and a colour that already reads keeps its own hue untouched.
/// Blending keeps the character of the colour — a red stays a red, lifted —
/// where substituting a fixed grey would flatten every theme to one palette.
fn reach_contrast(colour: Rgb, background: Rgb, ratio: f32) -> Rgb {
    if contrast_ratio(colour, background) >= ratio {
        return colour;
    }
    const STEPS: u32 = 24;
    const WHITE: Rgb = (255, 255, 255);
    const BLACK: Rgb = (0, 0, 0);
    // Whichever end can get further from the background is the direction
    // with room to grow: light text for a dark page, dark text for a light
    // one. Pure white or black always clears 4.5:1 on any background, so
    // the loop always finds its floor within the steps.
    let target = if contrast_ratio(WHITE, background) >= contrast_ratio(BLACK, background) {
        WHITE
    } else {
        BLACK
    };
    for step in 1..=STEPS {
        let candidate = blend(colour, target, step as f32 / STEPS as f32);
        if contrast_ratio(candidate, background) >= ratio {
            return candidate;
        }
    }
    target
}

/// `t` of the way from `from` to `to`, per channel.
fn blend(from: Rgb, to: Rgb, t: f32) -> Rgb {
    let mix = |a: u8, b: u8| (a as f32 + (b as f32 - a as f32) * t).round() as u8;
    (mix(from.0, to.0), mix(from.1, to.1), mix(from.2, to.2))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A throwaway home directory, named after the test that uses it.
    fn scratch(name: &str) -> crate::testkit::Scratch {
        crate::testkit::Scratch::new(name)
    }

    #[test]
    fn the_template_is_written_only_where_omarchy_renders_it() {
        // Written beside the rest of Omarchy's files, and only where Omarchy
        // is installed: an existing template is the owner's to keep, and
        // nothing at all is created when Omarchy is not there.
        let home = scratch("install");
        std::fs::create_dir_all(home.join(".config/omarchy")).unwrap();
        let target = home.join(".config/omarchy/themed/omaread.toml.tpl");
        assert!(write_template(&target));
        assert_eq!(std::fs::read_to_string(&target).unwrap(), TEMPLATE);

        let home = scratch("keep");
        std::fs::create_dir_all(home.join(".config/omarchy/themed")).unwrap();
        let target = home.join(".config/omarchy/themed/omaread.toml.tpl");
        std::fs::write(&target, "mine").unwrap();
        assert!(!write_template(&target));
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "mine");

        let home = scratch("no-omarchy");
        std::fs::create_dir_all(home.join(".config")).unwrap();
        let target = home.join(".config/omarchy/themed/omaread.toml.tpl");
        assert!(!write_template(&target));
        assert!(!home.join(".config/omarchy").exists());
    }

    #[test]
    fn the_theme_name_comes_from_the_newest_layout_that_exists() {
        // Both layouts may exist while a machine sits between releases. The
        // state directory is the newer one, so its name is the theme in use;
        // the candidates arrive newest first from `paths`.
        let dir = scratch("name");
        let state = dir.join("state/omarchy/current/theme.name");
        let config = dir.join("config/omarchy/current/theme.name");
        std::fs::create_dir_all(state.parent().unwrap()).unwrap();
        std::fs::write(&state, "giants\n").unwrap();
        std::fs::create_dir_all(config.parent().unwrap()).unwrap();
        std::fs::write(&config, "matte-black\n").unwrap();
        assert_eq!(theme_name_in(&[state, config]).as_deref(), Some("giants"));

        // And the older layout on its own still answers.
        let dir = scratch("name-old");
        let config = dir.join("config/omarchy/current/theme.name");
        std::fs::create_dir_all(config.parent().unwrap()).unwrap();
        std::fs::write(&config, "matte-black\n").unwrap();
        let missing_state = dir.join("state/omarchy/current/theme.name");
        assert_eq!(
            theme_name_in(&[missing_state, config]).as_deref(),
            Some("matte-black")
        );
    }

    #[test]
    fn a_theme_file_that_appears_after_the_watcher_is_followed() {
        // Omarchy renders the template on the next theme change, so the file
        // may not exist when the watcher is made. Freezing the answer then
        // would leave the reader on the built-in colours for ever.
        let dir = scratch("late-theme");
        let path = dir.join("theme/omaread.toml");
        let mut watcher = Watcher {
            candidates: vec![path.clone()],
            path: None,
            seen: None,
            file: None,
            terminal: TerminalColors::default(),
        };
        assert!(!watcher.refresh(), "there is nothing to read yet");

        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "[colors]\nbackground = \"#0A1428\"\n").unwrap();
        assert!(
            watcher.refresh(),
            "the colours moved once the file appeared"
        );
        assert_eq!(watcher.theme().background, (0x0a, 0x14, 0x28));
        assert!(
            !watcher.refresh(),
            "reading the same file again changes nothing"
        );
    }

    #[test]
    fn every_colour_the_reader_reads_can_come_from_the_template() {
        // The template is the only source of the theme file, so a field the
        // reader parses but the template never writes would silently fall back.
        for field in [
            "background",
            "foreground",
            "accent",
            "muted",
            "code_background",
            "code_foreground",
            "quote",
            "mark_yellow",
            "mark_green",
            "mark_blue",
            "mark_red",
            "mark_purple",
        ] {
            assert!(
                TEMPLATE.contains(&format!("{field} = ")),
                "{field} is missing"
            );
        }

        // A rendered file reads back as the colours it names, hash or not.
        let theme = parse(
            r##"
            [colors]
            background = "#0A1428"
            foreground = "#F0F8FF"
            accent = "#FF40A3"
            muted = "#88919e"
            code_background = "#212b3e"
            code_foreground = "#f4cae8"
            quote = "#abb4be"
            mark_yellow = "#d9b84c"
            mark_green = "#6fb86f"
            mark_blue = "#6a9fd8"
            mark_red = "#e07a6a"
            mark_purple = "#b48ad8"
            "##,
        );
        assert_eq!(theme.background, (0x0a, 0x14, 0x28));
        assert_eq!(theme.accent, (0xff, 0x40, 0xa3));
        assert_eq!(theme.marks[0], (0xd9, 0xb8, 0x4c));
    }

    #[test]
    fn a_colour_is_read_only_as_six_hex_digits() {
        assert_eq!(hex("#a1b2c3"), Some((0xa1, 0xb2, 0xc3)));
        assert_eq!(hex("A1B2C3"), Some((0xa1, 0xb2, 0xc3)));
        assert_eq!(hex(" #a1b2c3 "), Some((0xa1, 0xb2, 0xc3)));
        assert_eq!(hex("#abc"), None);
        assert_eq!(hex("#gggggg"), None);
        assert_eq!(hex(""), None);
    }

    #[test]
    fn a_partial_or_broken_theme_leaves_the_defaults() {
        let theme = parse("[colors]\naccent = \"#ff0000\"\n");
        assert_eq!(theme.accent, (0xff, 0, 0));
        assert_eq!(theme.background, Theme::default().background);
        assert_eq!(parse("this is not toml {{{"), Theme::default());
        assert_eq!(parse(""), Theme::default());
    }

    #[test]
    fn marks_that_collide_are_pulled_apart_in_brightness() {
        // A theme where red and purple are the same accent, as some are.
        let theme = parse(
            r##"
            [colors]
            mark_yellow = "#5076B2"
            mark_green = "#00BFFF"
            mark_blue = "#F0F8FF"
            mark_red = "#FF40A3"
            mark_purple = "#FF40A3"
            "##,
        );
        // The four that were already distinct keep the theme's own colours;
        // the fifth is the same colour moved along the scale, so NORMAL and
        // VISION still follow the theme instead of a built-in palette.
        assert_eq!(
            &theme.marks[..4],
            &[
                hex("#5076B2").unwrap(),
                hex("#00BFFF").unwrap(),
                hex("#F0F8FF").unwrap(),
                hex("#FF40A3").unwrap(),
            ]
        );
        assert_ne!(theme.marks[4], hex("#FF40A3").unwrap());
        assert!(all_distinguishable(&theme.marks), "{:?}", theme.marks);
        assert!(all_distinguishable(&BUILT_IN_MARKS));
    }

    #[test]
    fn every_mark_carries_readable_text() {
        // WCAG AA asks for 4.5:1 on body text. A mark paints text against its
        // colour, so this has to hold for the text drawn on it — and the
        // contrast it uses is symmetric and bounded by white on white.
        for mark in BUILT_IN_MARKS {
            let fg = text_color_on(mark, &Theme::default());
            let ratio = contrast_ratio(mark, fg);
            assert!(ratio >= 4.5, "{mark:?} reaches only {ratio:.2}:1");
        }
        let white = (255, 255, 255);
        let black = (0, 0, 0);
        assert!((contrast_ratio(white, black) - 21.0).abs() < 0.01);
        assert_eq!(contrast_ratio(white, black), contrast_ratio(black, white));
        assert!((contrast_ratio(white, white) - 1.0).abs() < 0.01);
    }

    #[test]
    fn a_terminal_colour_is_read_in_all_three_forms() {
        // The spellings terminals use for one colour: sixteen-bit channels,
        // eight-bit channels, and the hash form.
        assert_eq!(reported("rgb:1212/1212/1212"), Some((0x12, 0x12, 0x12)));
        assert_eq!(reported("rgb:be/be/be"), Some((0xbe, 0xbe, 0xbe)));
        assert_eq!(reported("#0a1428"), Some((0x0a, 0x14, 0x28)));
        assert_eq!(reported("RGB:BEBE/BEBE/BEBE"), Some((0xbe, 0xbe, 0xbe)));

        // What is not a colour in one of those forms is no answer at all.
        assert_eq!(reported("tomato"), None);
        assert_eq!(reported("rgb:12/zz/12"), None);
        assert_eq!(reported("rgb:1/2/3"), None);
        assert_eq!(reported("rgb:1212/1212"), None);
        assert_eq!(reported("rgb:1212/1212/1212/1212"), None);
        assert_eq!(reported("#abc"), None);
    }

    #[test]
    fn the_answers_are_read_out_of_the_reply() {
        // A reply the way a terminal writes it: the two default colours (one
        // ended by ST, one by BEL), all sixteen slots, and the light report.
        let mut reply = String::from("\x1b]11;rgb:1212/1212/1212\x1b\\");
        reply.push_str("\x1b]10;rgb:bebe/bebe/bebe\x07");
        for slot in 0..16u8 {
            reply.push_str(&format!(
                "\x1b]4;{slot};rgb:{slot:02x}{slot:02x}/0000/1010\x1b\\"
            ));
        }
        reply.push_str("\x1b[?997;2n");

        let colors = TerminalColors::parse(&reply);
        assert_eq!(colors.background, Some((0x12, 0x12, 0x12)));
        assert_eq!(colors.foreground, Some((0xbe, 0xbe, 0xbe)));
        let palette = colors.palette.expect("all sixteen slots answered");
        for (slot, colour) in palette.iter().enumerate() {
            assert_eq!(*colour, (slot as u8, 0, 16), "slot {slot}");
        }
        assert_eq!(colors.light, Some(true), "997;2 is light");

        // The other half of the same report.
        assert_eq!(
            TerminalColors::parse("\x1b[?997;1n").light,
            Some(false),
            "997;1 is dark"
        );
    }

    #[test]
    fn an_answer_that_makes_no_sense_is_ignored() {
        // Replies, junk, and malformed shapes in one stream: only the shapes
        // that were asked for and spelled right are read.
        let colors = TerminalColors::parse(concat!(
            "somebody typed while the question was out\r\n",
            "\x1b]11;tomato\x07",
            "\x1b]10;rgb:12/zz/12\x07",
            "\x1b]4;99;rgb:0000/0000/0000\x07",
            "\x1b]4;3;rgb:1212/1212\x07",
            "\x1b[?997;9n",
            "\x1b]11;rgb:0102/0304/0506\x07",
        ));
        assert_eq!(colors.background, Some((1, 3, 5)), "the one right answer");
        assert_eq!(colors.foreground, None);
        assert_eq!(colors.palette, None, "no slot answered whole");
        assert_eq!(colors.light, None);

        // A reply cut off mid-sequence says nothing at all.
        assert_eq!(
            TerminalColors::parse("\x1b]11;rgb:1212/1212/1212"),
            TerminalColors::default()
        );
    }

    #[test]
    fn appearance_takes_the_colours_then_the_report_then_the_variable() {
        // The direction of the text: light on a dark page, dark on a light one.
        let dark = TerminalColors {
            background: Some((18, 18, 18)),
            foreground: Some((190, 190, 190)),
            ..TerminalColors::default()
        };
        assert_eq!(dark.appearance(), Appearance::Dark);
        let light = TerminalColors {
            background: Some((245, 245, 245)),
            foreground: Some((30, 30, 30)),
            ..TerminalColors::default()
        };
        assert_eq!(light.appearance(), Appearance::Light);

        // A mid-grey background lets neither black nor white text reach 4.5:1,
        // so the direction of the foreground says nothing — the terminal's own
        // report decides, even against what the colours suggest.
        let undecided = TerminalColors {
            background: Some((128, 128, 128)),
            foreground: Some((250, 250, 250)),
            light: Some(true),
            ..TerminalColors::default()
        };
        assert_eq!(undecided.appearance(), Appearance::Light);

        // No colours to reason from: the report, the environment, darkness.
        let reported_dark = TerminalColors {
            light: Some(false),
            ..TerminalColors::default()
        };
        assert_eq!(reported_dark.appearance(), Appearance::Dark);
        let reported_light = TerminalColors {
            light: Some(true),
            ..TerminalColors::default()
        };
        assert_eq!(reported_light.appearance(), Appearance::Light);

        let silent = TerminalColors::default();
        assert_eq!(silent.appearance_from(Some("15;0")), Appearance::Dark);
        assert_eq!(silent.appearance_from(Some("0;15")), Appearance::Light);
        assert_eq!(silent.appearance_from(Some("not a colour")), Appearance::Dark);
        assert_eq!(silent.appearance_from(None), Appearance::Dark);
    }

    #[test]
    fn contrast_is_lifted_only_when_the_page_hides_the_colour() {
        // A dark page swallows a colour that sits near it: the steps carry it
        // toward white until the floor is cleared.
        let background = (16, 16, 20);
        let hidden = (40, 40, 48);
        assert!(contrast_ratio(hidden, background) < BODY_CONTRAST);
        let lifted = reach_contrast(hidden, background, BODY_CONTRAST);
        assert!(
            contrast_ratio(lifted, background) >= BODY_CONTRAST,
            "{lifted:?} reaches only {:.2}:1",
            contrast_ratio(lifted, background)
        );
        assert!(lifted.0 >= hidden.0 && lifted.1 >= hidden.1, "lighter");

        // A colour that already reads keeps its own hue untouched.
        let readable = (200, 205, 215);
        assert!(contrast_ratio(readable, background) >= BODY_CONTRAST);
        assert_eq!(reach_contrast(readable, background, BODY_CONTRAST), readable);

        // A light page works the other way: the colour is carried down.
        let background = (255, 255, 255);
        let hidden = (240, 240, 240);
        let lifted = reach_contrast(hidden, background, BODY_CONTRAST);
        assert!(contrast_ratio(lifted, background) >= BODY_CONTRAST);
        assert!(lifted.0 <= hidden.0 && lifted.1 <= hidden.1, "darker");
        let readable = (30, 30, 40);
        assert_eq!(reach_contrast(readable, background, BODY_CONTRAST), readable);

        // A role's floor is lower than body text's, so a colour between the
        // two is enough for one and not for the other.
        let between = (120, 120, 130);
        assert_eq!(reach_contrast(between, (255, 255, 255), ROLE_CONTRAST), between);
        assert!(
            reach_contrast(between, (255, 255, 255), BODY_CONTRAST) != between
        );
    }

    /// A palette with room on a dark page for every role it has to serve.
    fn synthetic_palette() -> [Rgb; 16] {
        let mut palette = [(0x80, 0x80, 0x80); 16];
        palette[0] = (16, 16, 20);
        palette[15] = (235, 235, 235);
        palette[8] = (120, 120, 130);
        palette[6] = (85, 190, 210);
        palette[7] = (200, 200, 200);
        palette[3] = (200, 170, 60);
        palette[11] = (235, 205, 95);
        palette[2] = (90, 190, 110);
        palette[10] = (120, 225, 140);
        palette[4] = (90, 140, 225);
        palette[12] = (125, 175, 245);
        palette[1] = (220, 105, 95);
        palette[9] = (245, 140, 130);
        palette[5] = (185, 115, 225);
        palette[13] = (215, 150, 245);
        palette[14] = (110, 215, 235);
        palette
    }

    #[test]
    fn a_palette_becomes_a_readable_theme_where_no_file_is() {
        let palette = synthetic_palette();
        let terminal = TerminalColors {
            background: Some(palette[0]),
            foreground: Some(palette[15]),
            palette: Some(palette),
            light: None,
        };
        let theme = from_terminal(&terminal, None);

        // The page is the terminal's own, and every role clears the floor its
        // job needs against it.
        assert_eq!(theme.background, palette[0]);
        assert_eq!(theme.foreground, palette[15]);
        assert!(contrast_ratio(theme.foreground, theme.background) >= BODY_CONTRAST);
        for (name, role) in [
            ("accent", theme.accent),
            ("muted", theme.muted),
            ("quote", theme.quote),
            ("code foreground", theme.code_foreground),
        ] {
            assert!(
                contrast_ratio(role, theme.background) >= ROLE_CONTRAST,
                "{name} {role:?} reaches only {:.2}:1",
                contrast_ratio(role, theme.background)
            );
        }
        for mark in theme.marks {
            assert!(
                contrast_ratio(mark, theme.background) >= ROLE_CONTRAST,
                "mark {mark:?}"
            );
        }
        assert!(
            all_distinguishable(&theme.marks),
            "five marks must tell each other apart: {:?}",
            theme.marks
        );

        // The mapping: the bright yellow leads, bright black dims, and a code
        // block is a touch of the text mixed into the page.
        assert_eq!(theme.accent, palette[11]);
        assert_eq!(theme.muted, palette[8]);
        assert_eq!(
            theme.code_background,
            blend(theme.background, theme.foreground, 0.1)
        );
    }

    #[test]
    fn a_theme_file_is_corrected_against_the_background_the_terminal_reports() {
        let file = parse(
            r##"
            [colors]
            background = "#121212"
            foreground = "#bebebe"
            accent = "#e68e0d"
            muted = "#717171"
            code_background = "#232323"
            code_foreground = "#c8b292"
            quote = "#8a8a8a"
            mark_yellow = "#d9b84c"
            mark_green = "#6fb86f"
            mark_blue = "#6a9fd8"
            mark_red = "#e07a6a"
            mark_purple = "#b48ad8"
            "##,
        );
        let terminal = TerminalColors {
            background: Some((250, 250, 250)),
            foreground: Some((35, 35, 40)),
            ..TerminalColors::default()
        };
        let theme = from_terminal(&terminal, Some(file));

        // The page is the terminal's, not the file's…
        assert_eq!(theme.background, (250, 250, 250));
        assert_eq!(theme.foreground, (35, 35, 40));
        assert!(contrast_ratio(theme.foreground, theme.background) >= BODY_CONTRAST);

        // …so the file's roles are measured against *that* page: the warm
        // accent cannot live on white and was carried down, while the muted
        // and quoted greys already could and kept the file's hue.
        assert_ne!(theme.accent, file.accent);
        assert!(contrast_ratio(theme.accent, theme.background) >= ROLE_CONTRAST);
        assert_eq!(theme.muted, file.muted);
        assert_eq!(theme.quote, file.quote);
        for mark in theme.marks {
            assert!(
                contrast_ratio(mark, theme.background) >= ROLE_CONTRAST,
                "mark {mark:?}"
            );
        }
    }

    #[test]
    fn a_terminal_that_says_nothing_changes_nothing() {
        // The whole point of the fallback: no answer is not an invitation to
        // repaint. The file's colours, or the built-ins, stand as they did
        // before anything was asked.
        let file = parse("[colors]\nbackground = \"#0A1428\"\naccent = \"#FF40A3\"\n");
        assert_eq!(from_terminal(&TerminalColors::default(), Some(file)), file);
        assert_eq!(
            from_terminal(&TerminalColors::default(), None),
            Theme::default()
        );
    }
}

