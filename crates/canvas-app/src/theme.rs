//! The look: color tokens (the old style.css :root variables) for light and dark, the one-second
//! cross-fade between them, and the fonts (Inter at the weights the design uses, KaTeX's fonts for
//! math, a monospace, and system fallbacks for characters Inter lacks).

use std::cell::Cell;
use std::sync::Arc;
use std::time::Instant;

use egui::{Color32, FontData, FontDefinitions, FontFamily, FontId};

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Tokens {
    pub bg: Color32,
    pub panel: Color32,
    pub panel_solid: Color32,
    pub sidebar: Color32,
    pub text: Color32,
    pub muted: Color32,
    pub faint: Color32,
    pub line: Color32,
    pub hover: Color32,
    pub vignette: Color32,
    pub accent: Color32,
    pub accent_soft: Color32,
    pub ok: Color32,
    pub ok_soft: Color32,
    pub warn: Color32,
    pub warn_soft: Color32,
    pub bad: Color32,
    pub bad_soft: Color32,
    /// Anki's "new" blue (#2f6db5, lighter in dark mode).
    pub blue: Color32,
    /// How much of the base color is washed over the paper (light .78, dark 0).
    pub paper_wash: f32,
    /// 0: multiply the paper in (light); 1: soft-light it (dark).
    pub paper_soft: f32,
    /// Leaves on the vines are dimmed in dark mode.
    pub leaf_alpha: f32,
    /// 0 light, 1 dark (mid-fade in between).
    pub dark: f32,
}

const fn rgb(hex: u32) -> Color32 {
    Color32::from_rgb((hex >> 16) as u8, (hex >> 8) as u8, hex as u8)
}
fn rgba(r: u8, g: u8, b: u8, a: f32) -> Color32 {
    Color32::from_rgba_unmultiplied(r, g, b, (a * 255.0).round() as u8)
}

pub fn light() -> Tokens {
    Tokens {
        bg: rgb(0xfbf7ec),
        panel: rgba(252, 250, 244, 0.62),
        panel_solid: rgb(0xfaf8f2),
        sidebar: rgb(0xf3eddf),
        text: rgb(0x1d1d1b),
        muted: rgb(0x6b6a66),
        faint: rgb(0x9a9994),
        line: rgb(0xddd7c9),
        hover: rgba(110, 90, 50, 0.07),
        vignette: rgba(120, 95, 50, 0.12),
        accent: rgb(0xb5462f),
        accent_soft: rgb(0xf6e4df),
        ok: rgb(0x2f7d4f),
        ok_soft: rgb(0xe1f1e7),
        warn: rgb(0xa86a12),
        warn_soft: rgb(0xf7ecd9),
        bad: rgb(0xb3261e),
        bad_soft: rgb(0xf8e0de),
        blue: rgb(0x2f6db5),
        paper_wash: 0.78,
        paper_soft: 0.0,
        leaf_alpha: 1.0,
        dark: 0.0,
    }
}

pub fn dark() -> Tokens {
    Tokens {
        bg: rgb(0x1d1b17),
        panel: rgba(44, 41, 35, 0.55),
        panel_solid: rgb(0x26241f),
        sidebar: rgb(0x181612),
        text: rgb(0xecebe7),
        muted: rgb(0xa3a29c),
        faint: rgb(0x75746f),
        line: rgb(0x36332c),
        hover: rgba(255, 240, 210, 0.06),
        vignette: rgba(0, 0, 0, 0.35),
        accent: rgb(0xe8795f),
        accent_soft: rgb(0x3a2621),
        ok: rgb(0x6cc38e),
        ok_soft: rgb(0x1e3327),
        warn: rgb(0xe0a54a),
        warn_soft: rgb(0x3a2e1b),
        bad: rgb(0xf08a80),
        bad_soft: rgb(0x3d2220),
        blue: rgb(0x7aa7e0),
        paper_wash: 0.0,
        paper_soft: 1.0,
        leaf_alpha: 0.75,
        dark: 1.0,
    }
}

pub fn lerp_color(a: Color32, b: Color32, t: f32) -> Color32 {
    let l = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * t).round() as u8;
    Color32::from_rgba_premultiplied(l(a.r(), b.r()), l(a.g(), b.g()), l(a.b(), b.b()), l(a.a(), b.a()))
}

impl Tokens {
    pub fn lerp(&self, o: &Tokens, t: f32) -> Tokens {
        let c = |a: Color32, b: Color32| lerp_color(a, b, t);
        let f = |a: f32, b: f32| a + (b - a) * t;
        Tokens {
            bg: c(self.bg, o.bg),
            panel: c(self.panel, o.panel),
            panel_solid: c(self.panel_solid, o.panel_solid),
            sidebar: c(self.sidebar, o.sidebar),
            text: c(self.text, o.text),
            muted: c(self.muted, o.muted),
            faint: c(self.faint, o.faint),
            line: c(self.line, o.line),
            hover: c(self.hover, o.hover),
            vignette: c(self.vignette, o.vignette),
            accent: c(self.accent, o.accent),
            accent_soft: c(self.accent_soft, o.accent_soft),
            ok: c(self.ok, o.ok),
            ok_soft: c(self.ok_soft, o.ok_soft),
            warn: c(self.warn, o.warn),
            warn_soft: c(self.warn_soft, o.warn_soft),
            bad: c(self.bad, o.bad),
            bad_soft: c(self.bad_soft, o.bad_soft),
            blue: c(self.blue, o.blue),
            paper_wash: f(self.paper_wash, o.paper_wash),
            paper_soft: f(self.paper_soft, o.paper_soft),
            leaf_alpha: f(self.leaf_alpha, o.leaf_alpha),
            dark: f(self.dark, o.dark),
        }
    }
    pub fn is_dark(&self) -> bool {
        self.dark > 0.5
    }
}

thread_local! {
    static CUR: Cell<Tokens> = Cell::new(light());
}

/// The tokens this frame is drawn with.
pub fn t() -> Tokens {
    CUR.with(|c| c.get())
}

pub fn set(tokens: Tokens) {
    CUR.with(|c| c.set(tokens));
}

/// color-mix(in srgb, a p%, b) for opaque-ish colors.
pub fn mix(a: Color32, b: Color32, a_share: f32) -> Color32 {
    lerp_color(b, a, a_share)
}

/// A color at some opacity (premultiplied).
pub fn alpha(c: Color32, a: f32) -> Color32 {
    c.gamma_multiply(a)
}

/// Theme preference: "" follows the system, or "light" / "dark"; switching fades over a second.
pub struct Theme {
    pub pref: String,
    from: Tokens,
    to: Tokens,
    start: Option<Instant>,
    pub system_dark: bool,
}

pub const FADE_SECONDS: f32 = 1.0;

fn ease_in_out(t: f32) -> f32 {
    // CSS ease-in-out: cubic-bezier(.42, 0, .58, 1)
    crate::anim::cubic_bezier(0.42, 0.0, 0.58, 1.0, t)
}

impl Theme {
    pub fn new(pref: &str, system_dark: bool) -> Theme {
        let mut th = Theme { pref: pref.to_string(), from: light(), to: light(), start: None, system_dark };
        let target = th.target();
        th.from = target;
        th.to = target;
        th
    }

    pub fn wants_dark(&self) -> bool {
        match self.pref.as_str() {
            "dark" => true,
            "light" => false,
            _ => self.system_dark,
        }
    }

    fn target(&self) -> Tokens {
        if self.wants_dark() { dark() } else { light() }
    }

    fn retarget(&mut self, fade: bool) {
        let target = self.target();
        if target == self.to {
            return;
        }
        if fade {
            self.from = self.current();
            self.start = Some(Instant::now());
        } else {
            self.from = target;
            self.start = None;
        }
        self.to = target;
    }

    pub fn set_pref(&mut self, pref: &str, fade: bool) {
        self.pref = pref.to_string();
        self.retarget(fade);
    }

    pub fn set_system_dark(&mut self, dark: bool) {
        if self.system_dark != dark {
            self.system_dark = dark;
            self.retarget(true);
        }
    }

    pub fn current(&self) -> Tokens {
        match self.start {
            Some(s) => {
                let p = (s.elapsed().as_secs_f32() / FADE_SECONDS).min(1.0);
                self.from.lerp(&self.to, ease_in_out(p))
            }
            None => self.to,
        }
    }

    pub fn fading(&self) -> bool {
        self.start.map(|s| s.elapsed().as_secs_f32() < FADE_SECONDS).unwrap_or(false)
    }
}

// --- fonts -----------------------------------------------------------------------------------------
macro_rules! font {
    ($f:literal) => {
        include_bytes!(concat!("../../../assets/fonts/", $f))
    };
}

pub const WEIGHTS: [u16; 6] = [400, 500, 550, 600, 650, 700];

/// The family for Inter at a weight (400–700), upright or italic.
pub fn family(weight: u16, italic: bool) -> FontFamily {
    if italic {
        return FontFamily::Name(if weight >= 550 { "ii600".into() } else { "ii400".into() });
    }
    let w = WEIGHTS.iter().copied().min_by_key(|w| (*w as i32 - weight as i32).abs()).unwrap_or(400);
    FontFamily::Name(format!("i{w}").into())
}

pub fn font(size: f32, weight: u16) -> FontId {
    FontId::new(size, family(weight, false))
}

pub fn italic(size: f32, weight: u16) -> FontId {
    FontId::new(size, family(weight, true))
}

pub fn mono(size: f32) -> FontId {
    FontId::new(size, FontFamily::Monospace)
}

/// KaTeX's fonts, by the names the math layout uses.
pub const KATEX: &[(&str, &[u8])] = &[
    ("KaTeX_Main-Regular", font!("katex/KaTeX_Main-Regular.ttf")),
    ("KaTeX_Main-Bold", font!("katex/KaTeX_Main-Bold.ttf")),
    ("KaTeX_Main-Italic", font!("katex/KaTeX_Main-Italic.ttf")),
    ("KaTeX_Main-BoldItalic", font!("katex/KaTeX_Main-BoldItalic.ttf")),
    ("KaTeX_Math-Italic", font!("katex/KaTeX_Math-Italic.ttf")),
    ("KaTeX_Math-BoldItalic", font!("katex/KaTeX_Math-BoldItalic.ttf")),
    ("KaTeX_AMS-Regular", font!("katex/KaTeX_AMS-Regular.ttf")),
    ("KaTeX_Caligraphic-Regular", font!("katex/KaTeX_Caligraphic-Regular.ttf")),
    ("KaTeX_Fraktur-Regular", font!("katex/KaTeX_Fraktur-Regular.ttf")),
    ("KaTeX_SansSerif-Regular", font!("katex/KaTeX_SansSerif-Regular.ttf")),
    ("KaTeX_Script-Regular", font!("katex/KaTeX_Script-Regular.ttf")),
    ("KaTeX_Typewriter-Regular", font!("katex/KaTeX_Typewriter-Regular.ttf")),
    ("KaTeX_Size1-Regular", font!("katex/KaTeX_Size1-Regular.ttf")),
    ("KaTeX_Size2-Regular", font!("katex/KaTeX_Size2-Regular.ttf")),
    ("KaTeX_Size3-Regular", font!("katex/KaTeX_Size3-Regular.ttf")),
    ("KaTeX_Size4-Regular", font!("katex/KaTeX_Size4-Regular.ttf")),
];

pub const INTER_400: &[u8] = font!("Inter-400.ttf");
pub const INTER_600: &[u8] = font!("Inter-600.ttf");

/// A system font for characters Inter doesn't have (symbols, other scripts).
fn system_fallbacks() -> Vec<(String, Vec<u8>)> {
    let candidates: &[&str] = if cfg!(windows) {
        &["C:\\Windows\\Fonts\\segoeui.ttf", "C:\\Windows\\Fonts\\seguisym.ttf", "C:\\Windows\\Fonts\\msyh.ttc", "C:\\Windows\\Fonts\\YuGothR.ttc"]
    } else {
        &[
            "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf",
            "/usr/share/fonts/TTF/DejaVuSans.ttf",
            "/usr/share/fonts/dejavu/DejaVuSans.ttf",
            "/usr/share/fonts/dejavu-sans-fonts/DejaVuSans.ttf",
            "/usr/share/fonts/truetype/noto/NotoSansSymbols2-Regular.ttf",
            "/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc",
            "/usr/share/fonts/noto-cjk/NotoSansCJK-Regular.ttc",
            "/usr/share/fonts/google-noto-cjk/NotoSansCJK-Regular.ttc",
        ]
    };
    candidates.iter().filter_map(|p| std::fs::read(p).ok().map(|b| (p.to_string(), b))).collect()
}

pub fn fonts() -> FontDefinitions {
    let mut defs = FontDefinitions::default(); // egui's Hack (monospace) and emoji fonts
    let inter: [(&str, &[u8]); 8] = [
        ("i400", font!("Inter-400.ttf")),
        ("i500", font!("Inter-500.ttf")),
        ("i550", font!("Inter-550.ttf")),
        ("i600", font!("Inter-600.ttf")),
        ("i650", font!("Inter-650.ttf")),
        ("i700", font!("Inter-700.ttf")),
        ("ii400", font!("Inter-Italic-400.ttf")),
        ("ii600", font!("Inter-Italic-600.ttf")),
    ];
    let emoji: Vec<String> = defs.families.get(&FontFamily::Proportional).cloned().unwrap_or_default().into_iter().filter(|n| n.contains("moji")).collect();
    let fallbacks = system_fallbacks();
    for (name, _) in &fallbacks {
        let data = FontData::from_owned(fallbacks.iter().find(|(n, _)| n == name).unwrap().1.clone());
        defs.font_data.insert(name.clone(), Arc::new(data));
    }
    let tail: Vec<String> = fallbacks.iter().map(|(n, _)| n.clone()).chain(emoji.iter().cloned()).collect();
    for (name, bytes) in inter {
        defs.font_data.insert(name.into(), Arc::new(FontData::from_static(bytes)));
        let mut chain = vec![name.to_string()];
        chain.extend(tail.iter().cloned());
        defs.families.insert(FontFamily::Name(name.into()), chain);
    }
    // The default proportional family is Inter 400 too (egui's own widgets use it).
    let mut prop = vec!["i400".to_string()];
    prop.extend(tail.iter().cloned());
    defs.families.insert(FontFamily::Proportional, prop);
    if let Some(mono) = defs.families.get_mut(&FontFamily::Monospace) {
        mono.extend(fallbacks.iter().map(|(n, _)| n.clone()));
    }
    for (name, bytes) in KATEX {
        defs.font_data.insert((*name).into(), Arc::new(FontData::from_static(bytes)));
        // Math glyphs missing from one KaTeX font fall back to Main, then Inter.
        defs.families.insert(FontFamily::Name((*name).into()), vec![(*name).to_string(), "KaTeX_Main-Regular".into(), "KaTeX_AMS-Regular".into(), "i400".into()]);
    }
    defs
}
