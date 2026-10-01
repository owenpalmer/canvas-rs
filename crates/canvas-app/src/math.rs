//! Math, drawn like KaTeX: LaTeX parsed into a tree, laid out with TeX's rules (the atom
//! spacing table, fraction and script shifts, sized delimiters, big operators with limits) using
//! KaTeX's own fonts and their real glyph metrics. \ce{…} (mhchem) is translated to TeX first.
//! Anything it doesn't know is shown in red, as KaTeX does with throwOnError off.

use std::collections::HashMap;
use std::sync::Arc;

use egui::{Color32, FontFamily, FontId, Pos2, Stroke, Ui, pos2};
use once_cell::sync::Lazy;

// ---------- fonts and metrics ----------
static FACES: Lazy<HashMap<&'static str, ttf_parser::Face<'static>>> =
    Lazy::new(|| crate::theme::KATEX.iter().filter_map(|(n, b)| ttf_parser::Face::parse(b, 0).ok().map(|f| (*n, f))).collect());

#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash)]
pub enum Fnt {
    MathIt,
    Main,
    MainBold,
    MainIt,
    Ams,
    Cal,
    Frak,
    Sans,
    Tt,
    Script,
    Size1,
    Size2,
    Size3,
    Size4,
}

impl Fnt {
    fn name(self) -> &'static str {
        match self {
            Fnt::MathIt => "KaTeX_Math-Italic",
            Fnt::Main => "KaTeX_Main-Regular",
            Fnt::MainBold => "KaTeX_Main-Bold",
            Fnt::MainIt => "KaTeX_Main-Italic",
            Fnt::Ams => "KaTeX_AMS-Regular",
            Fnt::Cal => "KaTeX_Caligraphic-Regular",
            Fnt::Frak => "KaTeX_Fraktur-Regular",
            Fnt::Sans => "KaTeX_SansSerif-Regular",
            Fnt::Tt => "KaTeX_Typewriter-Regular",
            Fnt::Script => "KaTeX_Script-Regular",
            Fnt::Size1 => "KaTeX_Size1-Regular",
            Fnt::Size2 => "KaTeX_Size2-Regular",
            Fnt::Size3 => "KaTeX_Size3-Regular",
            Fnt::Size4 => "KaTeX_Size4-Regular",
        }
    }
}

/// A character's metrics in ems (advance, height, depth) and the font that has it.
fn metrics(font: &'static str, c: char) -> (f32, f32, f32, &'static str) {
    for f in [font, "KaTeX_Main-Regular", "KaTeX_AMS-Regular", "KaTeX_Size1-Regular", "KaTeX_Math-Italic"] {
        if let Some(face) = FACES.get(f) {
            if let Some(g) = face.glyph_index(c) {
                let upem = face.units_per_em() as f32;
                let adv = face.glyph_hor_advance(g).unwrap_or(0) as f32 / upem;
                let (h, d) = face.glyph_bounding_box(g).map(|b| (b.y_max as f32 / upem, -(b.y_min as f32) / upem)).unwrap_or((0.0, 0.0));
                return (adv, h.max(0.0), d.max(0.0), f);
            }
        }
    }
    // not in KaTeX's fonts: Inter draws it (egui's fallback)
    let wide = !c.is_ascii();
    (if wide { 0.8 } else { 0.55 }, 0.7, 0.05, font)
}

/// An italic glyph's overhang past its advance, in ems (KaTeX's italic correction).
fn italic(font: &'static str, c: char) -> f32 {
    if !font.contains("Italic") {
        return 0.0;
    }
    let Some(face) = FACES.get(font) else { return 0.0 };
    let Some(g) = face.glyph_index(c) else { return 0.0 };
    let upem = face.units_per_em() as f32;
    let adv = face.glyph_hor_advance(g).unwrap_or(0) as f32 / upem;
    face.glyph_bounding_box(g).map(|b| (b.x_max as f32 / upem - adv).max(0.0)).unwrap_or(0.0)
}

// ---------- the laid-out result ----------
#[derive(Clone, Debug)]
enum Item {
    Glyph { x: f32, y: f32, s: String, font: &'static str, size: f32, color: Color32 },
    Rule { x: f32, y: f32, w: f32, h: f32, color: Color32 },
    Path { pts: Vec<(f32, f32)>, width: f32, color: Color32 },
}

impl Item {
    fn shifted(mut self, dx: f32, dy: f32) -> Item {
        match &mut self {
            Item::Glyph { x, y, .. } | Item::Rule { x, y, .. } => {
                *x += dx;
                *y += dy;
            }
            Item::Path { pts, .. } => {
                for p in pts.iter_mut() {
                    p.0 += dx;
                    p.1 += dy;
                }
            }
        }
        self
    }
}

/// A box: width, height above the baseline, depth below it, and what's drawn in it (x right,
/// y down from the baseline's left end), in pixels.
#[derive(Clone, Debug, Default)]
struct Bx {
    w: f32,
    h: f32,
    d: f32,
    items: Vec<Item>,
    /// the last glyph's italic correction (included in w): how far an italic letter leans past
    /// its advance. A subscript tucks back under it; a superscript goes after it.
    ic: f32,
}

impl Bx {
    fn add(&mut self, b: Bx, dx: f32, dy: f32) {
        self.items.extend(b.items.into_iter().map(|i| i.shifted(dx, dy)));
    }
}

pub struct Laid {
    pub width: f32,
    pub height: f32,
    pub depth: f32,
    items: Vec<Item>,
}

#[derive(Default)]
pub struct MathCache {
    laid: HashMap<(String, u32, [u8; 4]), Arc<Laid>>,
}

impl MathCache {
    pub fn lay(&mut self, _ui: &Ui, tex: &str, size: f32, color: Color32) -> Arc<Laid> {
        let key = (tex.to_string(), size.to_bits(), color.to_array());
        if let Some(l) = self.laid.get(&key) {
            return l.clone();
        }
        if self.laid.len() > 2000 {
            self.laid.clear();
        }
        let l = Arc::new(layout(tex, size, color));
        self.laid.insert(key, l.clone());
        l
    }
}

/// Lay out a formula: `size` is the font size in pixels.
pub fn layout(tex: &str, size: f32, color: Color32) -> Laid {
    let nodes = Parser::new(tex).list(None);
    let cx = Cx { style: Style::T, base: size, color };
    let b = hlist(&nodes, &cx);
    Laid { width: b.w, height: b.h, depth: b.d, items: b.items }
}

/// Draw at `pos`, the left end of the formula's baseline.
pub fn paint(ui: &Ui, l: &Laid, pos: Pos2) {
    let p = ui.painter();
    for it in &l.items {
        match it {
            Item::Glyph { x, y, s, font, size, color } => {
                let g = p.layout_no_wrap(s.clone(), FontId::new(*size, FontFamily::Name((*font).into())), *color);
                let off = g.rows.first().map(|r| r.pos.y + r.row.glyphs.first().map(|gl| gl.pos.y).unwrap_or(r.row.height() * 0.8)).unwrap_or(0.0);
                p.galley(pos2(pos.x + x, pos.y + y - off), g, *color);
            }
            Item::Rule { x, y, w, h, color } => {
                let r = egui::Rect::from_min_size(pos2(pos.x + x, pos.y + y), egui::vec2(*w, h.max(0.8)));
                p.rect_filled(r, 0.0, *color);
            }
            Item::Path { pts, width, color } => {
                let pts: Vec<Pos2> = pts.iter().map(|(x, y)| pos2(pos.x + x, pos.y + y)).collect();
                p.add(egui::Shape::line(pts, Stroke::new(*width, *color)));
            }
        }
    }
}

// ---------- the tree ----------
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Cls {
    Ord,
    Op,
    Bin,
    Rel,
    Open,
    Close,
    Punct,
    Inner,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, PartialOrd, Ord)]
enum Style {
    D,
    T,
    S,
    SS,
}

impl Style {
    fn mult(self) -> f32 {
        match self {
            Style::D | Style::T => 1.0,
            Style::S => 0.7,
            Style::SS => 0.5,
        }
    }
    fn sup(self) -> Style {
        match self {
            Style::D | Style::T => Style::S,
            _ => Style::SS,
        }
    }
    fn frac(self) -> Style {
        match self {
            Style::D => Style::T,
            Style::T => Style::S,
            _ => Style::SS,
        }
    }
    fn tight(self) -> bool {
        matches!(self, Style::S | Style::SS)
    }
}

#[derive(Clone, Debug)]
enum Node {
    Sym { s: String, font: Fnt, cls: Cls },
    List(Vec<Node>, Cls),
    Scripts { base: Box<Node>, sup: Option<Box<Node>>, sub: Option<Box<Node>> },
    Frac { num: Vec<Node>, den: Vec<Node>, style: Option<Style>, bar: bool, delims: Option<(String, String)> },
    Sqrt { body: Vec<Node>, index: Option<Vec<Node>> },
    Accent { body: Vec<Node>, acc: char },
    Line { body: Vec<Node>, over: bool },
    LeftRight { l: String, r: String, body: Vec<Node> },
    Big { delim: String, size: usize, cls: Cls },
    Space(f32),
    Text { s: String, font: Fnt },
    Op { s: String, font: Fnt, limits: Option<bool>, big: bool },
    Style(Style),
    Array { rows: Vec<Vec<Vec<Node>>>, align: Vec<char>, l: String, r: String, style: Style, gap: f32, pair: bool },
    Color(Color32, Vec<Node>),
    Stack { base: Vec<Node>, over: Option<Vec<Node>>, under: Option<Vec<Node>>, cls: Cls },
    Arrow { ch: char, over: Option<Vec<Node>>, under: Option<Vec<Node>> },
    Boxed(Vec<Node>),
    Phantom(Vec<Node>),
    Cancel(Vec<Node>),
    Error(String),
}

fn cls_of(n: &Node) -> Cls {
    match n {
        Node::Sym { cls, .. } | Node::List(_, cls) | Node::Big { cls, .. } | Node::Stack { cls, .. } => *cls,
        Node::Scripts { base, .. } => cls_of(base),
        Node::Op { .. } => Cls::Op,
        Node::LeftRight { .. } | Node::Frac { delims: Some(_), .. } => Cls::Inner,
        Node::Arrow { .. } => Cls::Rel,
        Node::Frac { .. } => Cls::Inner,
        _ => Cls::Ord,
    }
}

// ---------- symbols ----------
fn symbol(name: &str) -> Option<(char, Cls, Fnt)> {
    use Cls::*;
    let greek_lower = [
        ("alpha", 'α'), ("beta", 'β'), ("gamma", 'γ'), ("delta", 'δ'), ("epsilon", 'ϵ'), ("varepsilon", 'ε'), ("zeta", 'ζ'), ("eta", 'η'), ("theta", 'θ'),
        ("vartheta", 'ϑ'), ("iota", 'ι'), ("kappa", 'κ'), ("lambda", 'λ'), ("mu", 'μ'), ("nu", 'ν'), ("xi", 'ξ'), ("pi", 'π'), ("varpi", 'ϖ'), ("rho", 'ρ'),
        ("varrho", 'ϱ'), ("sigma", 'σ'), ("varsigma", 'ς'), ("tau", 'τ'), ("upsilon", 'υ'), ("phi", 'ϕ'), ("varphi", 'φ'), ("chi", 'χ'), ("psi", 'ψ'), ("omega", 'ω'),
    ];
    if let Some((_, c)) = greek_lower.iter().find(|(n, _)| *n == name) {
        return Some((*c, Ord, Fnt::MathIt));
    }
    let greek_upper = [
        ("Gamma", 'Γ'), ("Delta", 'Δ'), ("Theta", 'Θ'), ("Lambda", 'Λ'), ("Xi", 'Ξ'), ("Pi", 'Π'), ("Sigma", 'Σ'), ("Upsilon", 'Υ'), ("Phi", 'Φ'), ("Psi", 'Ψ'), ("Omega", 'Ω'),
    ];
    if let Some((_, c)) = greek_upper.iter().find(|(n, _)| *n == name) {
        return Some((*c, Ord, Fnt::Main));
    }
    let t: &[(&str, char, Cls)] = &[
        // binary operators
        ("pm", '±', Bin), ("mp", '∓', Bin), ("times", '×', Bin), ("div", '÷', Bin), ("cdot", '⋅', Bin), ("ast", '∗', Bin), ("star", '⋆', Bin), ("circ", '∘', Bin),
        ("bullet", '∙', Bin), ("cap", '∩', Bin), ("cup", '∪', Bin), ("wedge", '∧', Bin), ("land", '∧', Bin), ("vee", '∨', Bin), ("lor", '∨', Bin), ("oplus", '⊕', Bin),
        ("otimes", '⊗', Bin), ("ominus", '⊖', Bin), ("odot", '⊙', Bin), ("oslash", '⊘', Bin), ("setminus", '∖', Bin), ("uplus", '⊎', Bin), ("sqcap", '⊓', Bin),
        ("sqcup", '⊔', Bin), ("diamond", '⋄', Bin), ("dagger", '†', Bin), ("ddagger", '‡', Bin), ("amalg", '⨿', Bin), ("wr", '≀', Bin), ("triangleleft", '◃', Bin),
        ("triangleright", '▹', Bin), ("bigtriangleup", '△', Bin), ("bigtriangledown", '▽', Bin), ("cdotp", '⋅', Punct),
        // relations
        ("leq", '≤', Rel), ("le", '≤', Rel), ("geq", '≥', Rel), ("ge", '≥', Rel), ("neq", '≠', Rel), ("ne", '≠', Rel), ("equiv", '≡', Rel), ("approx", '≈', Rel),
        ("sim", '∼', Rel), ("simeq", '≃', Rel), ("cong", '≅', Rel), ("propto", '∝', Rel), ("subset", '⊂', Rel), ("supset", '⊃', Rel), ("subseteq", '⊆', Rel),
        ("supseteq", '⊇', Rel), ("in", '∈', Rel), ("notin", '∉', Rel), ("ni", '∋', Rel), ("to", '→', Rel), ("rightarrow", '→', Rel), ("leftarrow", '←', Rel),
        ("gets", '←', Rel), ("leftrightarrow", '↔', Rel), ("Rightarrow", '⇒', Rel), ("Leftarrow", '⇐', Rel), ("Leftrightarrow", '⇔', Rel), ("iff", '⟺', Rel),
        ("implies", '⟹', Rel), ("impliedby", '⟸', Rel), ("mapsto", '↦', Rel), ("longrightarrow", '⟶', Rel), ("longleftarrow", '⟵', Rel),
        ("longleftrightarrow", '⟷', Rel), ("Longrightarrow", '⟹', Rel), ("Longleftarrow", '⟸', Rel), ("Longleftrightarrow", '⟺', Rel), ("longmapsto", '⟼', Rel),
        ("uparrow", '↑', Rel), ("downarrow", '↓', Rel), ("updownarrow", '↕', Rel), ("Uparrow", '⇑', Rel), ("Downarrow", '⇓', Rel), ("nearrow", '↗', Rel),
        ("searrow", '↘', Rel), ("swarrow", '↙', Rel), ("nwarrow", '↖', Rel), ("perp", '⊥', Rel), ("parallel", '∥', Rel), ("mid", '∣', Rel), ("nmid", '∤', Rel),
        ("ll", '≪', Rel), ("gg", '≫', Rel), ("prec", '≺', Rel), ("succ", '≻', Rel), ("preceq", '⪯', Rel), ("succeq", '⪰', Rel), ("models", '⊨', Rel), ("vdash", '⊢', Rel),
        ("dashv", '⊣', Rel), ("asymp", '≍', Rel), ("doteq", '≐', Rel), ("rightleftharpoons", '⇌', Rel), ("leftrightharpoons", '⇋', Rel), ("rightharpoonup", '⇀', Rel),
        ("rightharpoondown", '⇁', Rel), ("leftharpoonup", '↼', Rel), ("leftharpoondown", '↽', Rel), ("rightleftarrows", '⇄', Rel), ("leftrightarrows", '⇆', Rel),
        ("coloneqq", '≔', Rel), ("lesssim", '≲', Rel), ("gtrsim", '≳', Rel), ("leqslant", '⩽', Rel), ("geqslant", '⩾', Rel), ("sqsubseteq", '⊑', Rel),
        ("sqsupseteq", '⊒', Rel), ("nsubseteq", '⊈', Rel), ("subsetneq", '⊊', Rel), ("supsetneq", '⊋', Rel), ("nleq", '≰', Rel), ("ngeq", '≱', Rel),
        ("ncong", '≇', Rel), ("nsim", '≁', Rel), ("smile", '⌣', Rel), ("frown", '⌢', Rel), ("bowtie", '⋈', Rel), ("propto", '∝', Rel), ("hookrightarrow", '↪', Rel),
        ("hookleftarrow", '↩', Rel), ("leadsto", '⇝', Rel), ("therefore", '∴', Rel), ("because", '∵', Rel), ("triangleq", '≜', Rel), ("approxeq", '≊', Rel),
        // ordinary
        ("infty", '∞', Ord), ("partial", '∂', Ord), ("nabla", '∇', Ord), ("forall", '∀', Ord), ("exists", '∃', Ord), ("nexists", '∄', Ord), ("emptyset", '∅', Ord),
        ("varnothing", '∅', Ord), ("neg", '¬', Ord), ("lnot", '¬', Ord), ("hbar", 'ℏ', Ord), ("hslash", 'ℏ', Ord), ("ell", 'ℓ', Ord), ("Re", 'ℜ', Ord), ("Im", 'ℑ', Ord),
        ("aleph", 'ℵ', Ord), ("beth", 'ℶ', Ord), ("wp", '℘', Ord), ("angle", '∠', Ord), ("measuredangle", '∡', Ord), ("triangle", '△', Ord), ("prime", '′', Ord),
        ("degree", '°', Ord), ("dots", '…', Inner), ("ldots", '…', Inner), ("cdots", '⋯', Inner), ("vdots", '⋮', Ord), ("ddots", '⋱', Inner), ("top", '⊤', Ord),
        ("bot", '⊥', Ord), ("clubsuit", '♣', Ord), ("spadesuit", '♠', Ord), ("heartsuit", '♡', Ord), ("diamondsuit", '♢', Ord), ("checkmark", '✓', Ord), ("S", '§', Ord),
        ("P", '¶', Ord), ("%", '%', Ord), ("$", '$', Ord), ("#", '#', Ord), ("&", '&', Ord), ("_", '_', Ord), ("backslash", '\\', Ord), ("imath", 'ı', Ord),
        ("jmath", 'ȷ', Ord), ("surd", '√', Ord), ("flat", '♭', Ord), ("natural", '♮', Ord), ("sharp", '♯', Ord), ("mho", '℧', Ord), ("Box", '□', Ord), ("square", '□', Ord),
        ("blacksquare", '■', Ord), ("lozenge", '◊', Ord), ("star", '⋆', Bin), ("circledR", '®', Ord), ("copyright", '©', Ord), ("pounds", '£', Ord), ("yen", '¥', Ord),
        ("euro", '€', Ord), ("textdegree", '°', Ord), ("vert", '∣', Ord), ("Vert", '∥', Ord), ("|", '∥', Ord), ("colon", ':', Punct), ("ldotp", '.', Punct),
        // delimiters
        ("{", '{', Open), ("}", '}', Close), ("lbrace", '{', Open), ("rbrace", '}', Close), ("langle", '⟨', Open), ("rangle", '⟩', Close), ("lfloor", '⌊', Open),
        ("rfloor", '⌋', Close), ("lceil", '⌈', Open), ("rceil", '⌉', Close), ("lbrack", '[', Open), ("rbrack", ']', Close), ("lvert", '∣', Open), ("rvert", '∣', Close),
        ("lVert", '∥', Open), ("rVert", '∥', Close),
    ];
    t.iter().find(|(n, _, _)| *n == name).map(|(_, c, k)| (*c, *k, Fnt::Main))
}

fn big_op(name: &str) -> Option<(char, bool)> {
    // (glyph, limits in display style)
    Some(match name {
        "sum" => ('∑', true),
        "prod" => ('∏', true),
        "coprod" => ('∐', true),
        "bigcup" => ('⋃', true),
        "bigcap" => ('⋂', true),
        "bigoplus" => ('⨁', true),
        "bigotimes" => ('⨂', true),
        "bigodot" => ('⨀', true),
        "biguplus" => ('⨄', true),
        "bigvee" => ('⋁', true),
        "bigwedge" => ('⋀', true),
        "bigsqcup" => ('⨆', true),
        "int" => ('∫', false),
        "iint" => ('∬', false),
        "iiint" => ('∭', false),
        "oint" => ('∮', false),
        "oiint" => ('∯', false),
        "smallint" => ('∫', false),
        _ => return None,
    })
}

const FUNCS: &[&str] = &[
    "sin", "cos", "tan", "cot", "sec", "csc", "arcsin", "arccos", "arctan", "arccot", "sinh", "cosh", "tanh", "coth", "sech", "csch", "log", "ln", "lg", "exp", "ker",
    "dim", "deg", "arg", "hom", "Pr", "det", "gcd", "lim", "liminf", "limsup", "max", "min", "sup", "inf", "argmax", "argmin", "tr", "Tr", "rank", "sgn", "erf",
];
const LIMIT_FUNCS: &[&str] = &["lim", "liminf", "limsup", "max", "min", "sup", "inf", "det", "gcd", "Pr", "argmax", "argmin"];

fn accent_char(name: &str) -> Option<char> {
    Some(match name {
        "hat" | "widehat" => 'ˆ',
        "tilde" | "widetilde" => '˜',
        "bar" => 'ˉ',
        "vec" => '⃗',
        "dot" => '˙',
        "ddot" => '¨',
        "check" | "widecheck" => 'ˇ',
        "breve" => '˘',
        "acute" => 'ˊ',
        "grave" => 'ˋ',
        "mathring" => '˚',
        "overrightarrow" => '→',
        "overleftarrow" => '←',
        _ => return None,
    })
}

fn color_named(s: &str) -> Option<Color32> {
    let s = s.trim();
    if let Some(h) = s.strip_prefix('#') {
        let v = u32::from_str_radix(h, 16).ok()?;
        return Some(match h.len() {
            3 => Color32::from_rgb(((v >> 8) & 15) as u8 * 17, ((v >> 4) & 15) as u8 * 17, (v & 15) as u8 * 17),
            6 => Color32::from_rgb((v >> 16) as u8, (v >> 8) as u8, v as u8),
            _ => return None,
        });
    }
    Some(match s.to_ascii_lowercase().as_str() {
        "red" => Color32::from_rgb(0xdf, 0x00, 0x30),
        "blue" => Color32::from_rgb(0x00, 0x66, 0xcc),
        "green" => Color32::from_rgb(0x28, 0xae, 0x7b),
        "orange" => Color32::from_rgb(0xff, 0x80, 0x00),
        "purple" => Color32::from_rgb(0x96, 0x4b, 0xae),
        "magenta" => Color32::from_rgb(0xff, 0x00, 0xff),
        "cyan" => Color32::from_rgb(0x00, 0xb7, 0xeb),
        "yellow" => Color32::from_rgb(0xe5, 0xc0, 0x00),
        "gray" | "grey" => Color32::from_rgb(0x80, 0x80, 0x80),
        "black" => Color32::BLACK,
        "white" => Color32::WHITE,
        "brown" => Color32::from_rgb(0x8b, 0x45, 0x13),
        "teal" => Color32::from_rgb(0x00, 0x80, 0x80),
        "pink" => Color32::from_rgb(0xff, 0x69, 0xb4),
        _ => return None,
    })
}

// ---------- parsing ----------
struct Parser {
    s: Vec<char>,
    i: usize,
    font: Option<Fnt>,
    /// inside an environment, & and \\ end a cell
    env: usize,
}

enum Tok {
    Cmd(String),
    Ch(char),
    Open,
    Close,
    Sup,
    Sub,
    Amp,
    NewRow,
    End,
}

impl Parser {
    fn new(s: &str) -> Parser {
        Parser { s: s.chars().collect(), i: 0, font: None, env: 0 }
    }

    fn skip_ws(&mut self) {
        while self.i < self.s.len() && self.s[self.i].is_whitespace() {
            self.i += 1;
        }
    }

    fn peek(&mut self) -> Tok {
        let save = self.i;
        let t = self.next();
        self.i = save;
        t
    }

    fn next(&mut self) -> Tok {
        self.skip_ws();
        let Some(&c) = self.s.get(self.i) else { return Tok::End };
        self.i += 1;
        match c {
            '{' => Tok::Open,
            '}' => Tok::Close,
            '^' => Tok::Sup,
            '_' => Tok::Sub,
            '&' => Tok::Amp,
            '\\' => {
                let start = self.i;
                while self.i < self.s.len() && self.s[self.i].is_ascii_alphabetic() {
                    self.i += 1;
                }
                if self.i == start {
                    let Some(&c2) = self.s.get(self.i) else { return Tok::Ch('\\') };
                    self.i += 1;
                    if c2 == '\\' {
                        // \\[2pt]
                        if self.s.get(self.i) == Some(&'[') {
                            while self.i < self.s.len() && self.s[self.i] != ']' {
                                self.i += 1;
                            }
                            self.i += 1;
                        }
                        return Tok::NewRow;
                    }
                    return Tok::Cmd(c2.to_string());
                }
                Tok::Cmd(self.s[start..self.i].iter().collect())
            }
            _ => Tok::Ch(c),
        }
    }

    /// The raw text of a {…} argument (or one character).
    fn raw_arg(&mut self) -> String {
        self.skip_ws();
        if self.s.get(self.i) != Some(&'{') {
            return match self.next() {
                Tok::Ch(c) => c.to_string(),
                Tok::Cmd(c) => format!("\\{c}"),
                _ => String::new(),
            };
        }
        self.i += 1;
        let start = self.i;
        let mut depth = 1;
        while self.i < self.s.len() {
            match self.s[self.i] {
                '\\' => self.i += 1,
                '{' => depth += 1,
                '}' => {
                    depth -= 1;
                    if depth == 0 {
                        break;
                    }
                }
                _ => {}
            }
            self.i += 1;
        }
        let out: String = self.s[start..self.i.min(self.s.len())].iter().collect();
        self.i += 1;
        out
    }

    /// An optional [ … ] argument.
    fn opt_arg(&mut self) -> Option<String> {
        self.skip_ws();
        if self.s.get(self.i) != Some(&'[') {
            return None;
        }
        self.i += 1;
        let start = self.i;
        let mut depth = 0;
        while self.i < self.s.len() {
            match self.s[self.i] {
                '{' => depth += 1,
                '}' => depth -= 1,
                ']' if depth == 0 => break,
                _ => {}
            }
            self.i += 1;
        }
        let out: String = self.s[start..self.i.min(self.s.len())].iter().collect();
        self.i += 1;
        Some(out)
    }

    fn sub_parse(&self, s: &str) -> Vec<Node> {
        let mut p = Parser::new(s);
        p.font = self.font;
        p.list(None)
    }

    /// One argument as a list of nodes.
    fn arg(&mut self) -> Vec<Node> {
        match self.peek() {
            Tok::Open => {
                self.next();
                self.list(Some(()))
            }
            Tok::End | Tok::Close => vec![],
            _ => self.atom().map(|n| vec![n]).unwrap_or_default(),
        }
    }

    /// Nodes until a closing brace (when `braced`) or the end.
    fn list(&mut self, braced: Option<()>) -> Vec<Node> {
        let mut out: Vec<Node> = Vec::new();
        loop {
            match self.peek() {
                Tok::End => {
                    self.next();
                    break;
                }
                Tok::Close => {
                    self.next();
                    if braced.is_some() {
                        break;
                    }
                }
                Tok::Amp | Tok::NewRow if braced.is_none() && self.env > 0 => break,
                Tok::Amp | Tok::NewRow => {
                    // outside an environment: ignore
                    self.next();
                }
                Tok::Sup | Tok::Sub => {
                    let base = if matches!(out.last(), Some(Node::Scripts { .. }) | None | Some(Node::Space(_)) | Some(Node::Style(_))) {
                        Node::List(vec![], Cls::Ord)
                    } else {
                        out.pop().unwrap()
                    };
                    let n = self.scripts(base);
                    out.push(n);
                }
                Tok::Cmd(c) if c == "right" || c == "end" => break,
                Tok::Cmd(c) if c == "limits" || c == "nolimits" || c == "displaylimits" => {
                    self.next();
                    if let Some(Node::Op { limits, .. }) = out.last_mut() {
                        *limits = Some(c == "limits");
                    }
                }
                Tok::Cmd(c) if c == "over" || c == "choose" => {
                    self.next();
                    let num = std::mem::take(&mut out);
                    let den = self.list(braced);
                    let bin = c == "choose";
                    out.push(Node::Frac { num, den, style: None, bar: !bin, delims: bin.then(|| ("(".into(), ")".into())) });
                    return out;
                }
                Tok::Cmd(c) if c == "color" => {
                    self.next();
                    let col = color_named(&self.raw_arg());
                    let rest = self.list(braced);
                    out.push(match col {
                        Some(c) => Node::Color(c, rest),
                        None => Node::List(rest, Cls::Ord),
                    });
                    return out;
                }
                Tok::Cmd(c) if ["rm", "bf", "it", "cal", "sf", "tt"].contains(&c.as_str()) => {
                    self.next();
                    self.font = Some(match c.as_str() {
                        "rm" => Fnt::Main,
                        "bf" => Fnt::MainBold,
                        "it" => Fnt::MainIt,
                        "cal" => Fnt::Cal,
                        "sf" => Fnt::Sans,
                        _ => Fnt::Tt,
                    });
                }
                _ => {
                    if let Some(n) = self.atom() {
                        out.push(n);
                    }
                }
            }
        }
        out
    }

    fn scripts(&mut self, base: Node) -> Node {
        let mut sup = None;
        let mut sub = None;
        let (mut base, mut primes) = match base {
            Node::Scripts { base, sup: s, sub: b } => {
                sup = s.map(|x| *x);
                sub = b.map(|x| *x);
                (*base, 0)
            }
            b => (b, 0),
        };
        loop {
            match self.peek() {
                Tok::Sup if sup.is_none() => {
                    self.next();
                    sup = Some(Node::List(self.arg(), Cls::Ord));
                }
                Tok::Sub if sub.is_none() => {
                    self.next();
                    sub = Some(Node::List(self.arg(), Cls::Ord));
                }
                Tok::Ch('\'') => {
                    self.next();
                    primes += 1;
                }
                _ => break,
            }
        }
        if primes > 0 {
            let p = Node::Sym { s: "′".repeat(primes), font: Fnt::Main, cls: Cls::Ord };
            sup = Some(match sup {
                Some(Node::List(mut v, c)) => {
                    v.insert(0, p);
                    Node::List(v, c)
                }
                Some(n) => Node::List(vec![p, n], Cls::Ord),
                None => p,
            });
        }
        if let Node::Scripts { .. } = base {
            base = Node::List(vec![base], Cls::Ord);
        }
        Node::Scripts { base: Box::new(base), sup: sup.map(Box::new), sub: sub.map(Box::new) }
    }

    fn letter(&self, c: char) -> Node {
        let font = match self.font {
            Some(f) => f,
            None if c.is_ascii_alphabetic() => Fnt::MathIt,
            None => Fnt::Main,
        };
        let (s, font) = match font {
            // \mathbb: KaTeX keeps the double-struck capitals at the ASCII code points of AMS
            Fnt::Ams if c.is_ascii_uppercase() => (c.to_string(), Fnt::Ams),
            Fnt::Ams => (c.to_string(), Fnt::Main),
            Fnt::Cal | Fnt::Script if !c.is_ascii_uppercase() => (c.to_string(), Fnt::Main),
            f => (c.to_string(), if c.is_ascii_digit() && f == Fnt::MathIt { Fnt::Main } else { f }),
        };
        Node::Sym { s, font, cls: Cls::Ord }
    }

    fn atom(&mut self) -> Option<Node> {
        match self.next() {
            Tok::Ch(c) => Some(self.char_atom(c)),
            Tok::Open => Some(Node::List(self.list(Some(())), Cls::Ord)),
            Tok::Cmd(c) => Some(self.command(&c)),
            Tok::Sup | Tok::Sub | Tok::Amp | Tok::NewRow | Tok::Close | Tok::End => None,
        }
    }

    fn char_atom(&self, c: char) -> Node {
        use Cls::*;
        let (s, cls) = match c {
            '+' => ("+".to_string(), Bin),
            '-' => ("−".to_string(), Bin),
            '*' => ("∗".to_string(), Bin),
            '=' | '<' | '>' | ':' => (c.to_string(), Rel),
            ',' | ';' => (c.to_string(), Punct),
            '(' | '[' => (c.to_string(), Open),
            ')' | ']' | '!' | '?' => (c.to_string(), Close),
            '~' => return Node::Space(0.333),
            '/' | '|' | '.' | '@' | '"' => (c.to_string(), Ord),
            c if c.is_ascii_alphanumeric() => return self.letter(c),
            'α'..='ω' => return Node::Sym { s: c.to_string(), font: Fnt::MathIt, cls: Ord },
            '×' | '·' | '±' | '÷' | '∓' => (c.to_string(), Bin),
            '≤' | '≥' | '≠' | '≈' | '→' | '←' | '⇌' | '⇒' | '∈' | '≡' => (c.to_string(), Rel),
            _ => (c.to_string(), Ord),
        };
        Node::Sym { s, font: Fnt::Main, cls }
    }

    fn with_font(&mut self, f: Fnt) -> Vec<Node> {
        let save = self.font;
        self.font = Some(f);
        let v = self.arg();
        self.font = save;
        v
    }

    fn text_arg(&mut self, font: Fnt) -> Node {
        let raw = self.raw_arg();
        // \text{…} may hold $…$ math and a few escapes
        if raw.contains('$') {
            let mut parts = Vec::new();
            for (i, seg) in raw.split('$').enumerate() {
                if i % 2 == 1 {
                    parts.push(Node::List(self.sub_parse(seg), Cls::Ord));
                } else if !seg.is_empty() {
                    parts.push(Node::Text { s: unescape_text(seg), font });
                }
            }
            return Node::List(parts, Cls::Ord);
        }
        Node::Text { s: unescape_text(&raw), font }
    }

    fn command(&mut self, c: &str) -> Node {
        use Cls::*;
        if let Some((ch, big)) = big_op(c) {
            let _ = big;
            return Node::Op { s: ch.to_string(), font: Fnt::Main, limits: None, big: true };
        }
        if FUNCS.contains(&c) {
            let s = match c {
                "liminf" => "lim inf".to_string(),
                "limsup" => "lim sup".to_string(),
                "argmax" => "arg max".to_string(),
                "argmin" => "arg min".to_string(),
                _ => c.to_string(),
            };
            return Node::Op { s, font: Fnt::Main, limits: if LIMIT_FUNCS.contains(&c) { None } else { Some(false) }, big: false };
        }
        if let Some((ch, cls, font)) = symbol(c) {
            return Node::Sym { s: ch.to_string(), font, cls };
        }
        if let Some(acc) = accent_char(c) {
            let body = self.arg();
            return Node::Accent { body, acc };
        }
        match c {
            "," | "thinspace" => Node::Space(3.0 / 18.0),
            ":" | ">" | "medspace" => Node::Space(4.0 / 18.0),
            ";" | "thickspace" => Node::Space(5.0 / 18.0),
            "!" | "negthinspace" => Node::Space(-3.0 / 18.0),
            " " => Node::Space(0.333),
            "quad" => Node::Space(1.0),
            "qquad" => Node::Space(2.0),
            "enspace" => Node::Space(0.5),
            "hspace" | "kern" | "mkern" | "mskip" | "hskip" => {
                let a = self.raw_arg();
                Node::Space(parse_len(&a))
            }
            "frac" | "dfrac" | "tfrac" | "cfrac" | "binom" | "dbinom" | "tbinom" => {
                let num = self.arg();
                let den = self.arg();
                let style = match c {
                    "dfrac" | "cfrac" | "dbinom" => Some(Style::D),
                    "tfrac" | "tbinom" => Some(Style::T),
                    _ => None,
                };
                let bin = c.contains("binom");
                Node::Frac { num, den, style, bar: !bin, delims: bin.then(|| ("(".into(), ")".into())) }
            }
            "sqrt" => {
                let index = self.opt_arg().map(|s| self.sub_parse(&s));
                Node::Sqrt { body: self.arg(), index }
            }
            "overline" => Node::Line { body: self.arg(), over: true },
            "underline" => Node::Line { body: self.arg(), over: false },
            "overbrace" => Node::Line { body: self.arg(), over: true },
            "underbrace" => Node::Line { body: self.arg(), over: false },
            "left" => {
                let l = self.delim();
                let body = self.list(None);
                let r = if matches!(self.peek(), Tok::Cmd(ref x) if x == "right") {
                    self.next();
                    self.delim()
                } else {
                    ".".into()
                };
                Node::LeftRight { l, r, body }
            }
            "middle" => {
                let d = self.delim();
                Node::Sym { s: d, font: Fnt::Main, cls: Rel }
            }
            "big" | "bigl" | "bigr" | "bigm" | "Big" | "Bigl" | "Bigr" | "Bigm" | "bigg" | "biggl" | "biggr" | "biggm" | "Bigg" | "Biggl" | "Biggr" | "Biggm" => {
                let d = self.delim();
                let size = match c.trim_end_matches(['l', 'r', 'm']) {
                    "big" => 1,
                    "Big" => 2,
                    "bigg" => 3,
                    _ => 4,
                };
                let cls = if c.ends_with('l') { Open } else if c.ends_with('r') { Close } else if c.ends_with('m') { Rel } else { Ord };
                Node::Big { delim: d, size, cls }
            }
            "text" | "textrm" | "mbox" | "textnormal" | "hbox" | "textup" => self.text_arg(Fnt::Main),
            "textbf" => self.text_arg(Fnt::MainBold),
            "textit" | "emph" | "textsl" => self.text_arg(Fnt::MainIt),
            "texttt" => self.text_arg(Fnt::Tt),
            "textsf" => self.text_arg(Fnt::Sans),
            "mathrm" | "operatorname" | "operatorname*" | "mathop" => {
                let v = self.with_font(Fnt::Main);
                match c {
                    "operatorname" | "mathop" => Node::Op { s: plain(&v), font: Fnt::Main, limits: Some(false), big: false },
                    _ => Node::List(v, Ord),
                }
            }
            "mathbf" | "boldsymbol" | "bm" | "pmb" => Node::List(self.with_font(Fnt::MainBold), Ord),
            "mathit" => Node::List(self.with_font(Fnt::MainIt), Ord),
            "mathcal" => Node::List(self.with_font(Fnt::Cal), Ord),
            "mathscr" => Node::List(self.with_font(Fnt::Script), Ord),
            "mathbb" => Node::List(self.with_font(Fnt::Ams), Ord),
            "mathfrak" => Node::List(self.with_font(Fnt::Frak), Ord),
            "mathsf" => Node::List(self.with_font(Fnt::Sans), Ord),
            "mathtt" => Node::List(self.with_font(Fnt::Tt), Ord),
            "mathnormal" => Node::List(self.with_font(Fnt::MathIt), Ord),
            "mathbin" => Node::List(self.arg(), Bin),
            "mathrel" => Node::List(self.arg(), Rel),
            "mathord" => Node::List(self.arg(), Ord),
            "mathopen" => Node::List(self.arg(), Open),
            "mathclose" => Node::List(self.arg(), Close),
            "mathpunct" => Node::List(self.arg(), Punct),
            "mathinner" => Node::List(self.arg(), Inner),
            "displaystyle" => Node::Style(Style::D),
            "textstyle" => Node::Style(Style::T),
            "scriptstyle" => Node::Style(Style::S),
            "scriptscriptstyle" => Node::Style(Style::SS),
            "textcolor" | "colorbox" => {
                let col = color_named(&self.raw_arg());
                let body = self.arg();
                match col {
                    Some(k) if c == "textcolor" => Node::Color(k, body),
                    _ => Node::List(body, Ord),
                }
            }
            "boxed" | "fbox" => Node::Boxed(self.arg()),
            "phantom" | "hphantom" | "vphantom" => Node::Phantom(self.arg()),
            "cancel" | "bcancel" | "xcancel" | "sout" => Node::Cancel(self.arg()),
            "overset" | "stackrel" => {
                let over = self.arg();
                let base = self.arg();
                Node::Stack { base, over: Some(over), under: None, cls: if c == "stackrel" { Rel } else { Ord } }
            }
            "underset" => {
                let under = self.arg();
                let base = self.arg();
                Node::Stack { base, over: None, under: Some(under), cls: Ord }
            }
            "xrightarrow" | "xleftarrow" | "xrightleftharpoons" | "xleftrightarrow" | "xRightarrow" | "xLeftarrow" | "xLeftrightarrow" | "xmapsto" | "xrightharpoonup" => {
                let under = self.opt_arg().map(|s| self.sub_parse(&s));
                let over = self.arg();
                let ch = match c {
                    "xleftarrow" => '←',
                    "xrightleftharpoons" => '⇌',
                    "xleftrightarrow" => '↔',
                    "xRightarrow" => '⇒',
                    "xLeftarrow" => '⇐',
                    "xLeftrightarrow" => '⇔',
                    "xmapsto" => '↦',
                    "xrightharpoonup" => '⇀',
                    _ => '→',
                };
                Node::Arrow { ch, over: (!over.is_empty()).then_some(over), under }
            }
            "bmod" => Node::Op { s: "mod".into(), font: Fnt::Main, limits: Some(false), big: false },
            "pmod" => {
                let a = self.arg();
                let mut v = vec![Node::Space(0.444), Node::Sym { s: "(".into(), font: Fnt::Main, cls: Open }, Node::Text { s: "mod".into(), font: Fnt::Main }, Node::Space(0.333)];
                v.extend(a);
                v.push(Node::Sym { s: ")".into(), font: Fnt::Main, cls: Close });
                Node::List(v, Ord)
            }
            "not" => {
                // \not= and friends
                match self.atom() {
                    Some(Node::Sym { s, cls, .. }) => {
                        let neg = match s.as_str() {
                            "=" => "≠",
                            "∈" => "∉",
                            "≡" => "≢",
                            "<" => "≮",
                            ">" => "≯",
                            "⊂" => "⊄",
                            "⊆" => "⊈",
                            _ => "̸",
                        };
                        Node::Sym { s: if neg == "̸" { format!("{s}{neg}") } else { neg.into() }, font: Fnt::Main, cls }
                    }
                    other => other.unwrap_or(Node::Space(0.0)),
                }
            }
            "ce" => {
                let raw = self.raw_arg();
                Node::List(self.sub_parse(&ce_to_tex(&raw)), Ord)
            }
            "pu" => {
                let raw = self.raw_arg();
                Node::List(self.sub_parse(&pu_to_tex(&raw)), Ord)
            }
            "begin" => self.env(),
            "tag" | "label" | "nonumber" | "notag" => {
                if c == "tag" || c == "label" {
                    self.raw_arg();
                }
                Node::Space(0.0)
            }
            "limits" | "nolimits" | "relax" | "strut" | "mathstrut" | "displaylimits" | "allowbreak" | "nobreak" | "hfill" | "left." | "right." => Node::Space(0.0),
            "lbrack" => Node::Sym { s: "[".into(), font: Fnt::Main, cls: Open },
            "{" => Node::Sym { s: "{".into(), font: Fnt::Main, cls: Open },
            _ => Node::Error(format!("\\{c}")),
        }
    }

    /// A delimiter after \left, \right, \big…
    fn delim(&mut self) -> String {
        match self.next() {
            Tok::Ch(c) => c.to_string(),
            Tok::Cmd(c) => match symbol(&c) {
                Some((ch, _, _)) => ch.to_string(),
                None if c == "." => ".".into(),
                None if c == "|" => "∥".into(),
                None => ".".into(),
            },
            _ => ".".into(),
        }
    }

    fn env(&mut self) -> Node {
        let name = self.raw_arg();
        let cols = if name == "array" || name == "alignat" || name == "alignedat" { self.raw_arg() } else { String::new() };
        let mut rows: Vec<Vec<Vec<Node>>> = vec![vec![]];
        self.env += 1;
        loop {
            let cell = self.list(None);
            rows.last_mut().unwrap().push(cell);
            match self.next() {
                Tok::Amp => {}
                Tok::NewRow => rows.push(vec![]),
                Tok::Cmd(c) if c == "end" => {
                    self.raw_arg();
                    break;
                }
                Tok::End => break,
                _ => {}
            }
        }
        self.env -= 1;
        // a trailing \\ leaves an empty row
        if rows.len() > 1 && rows.last().map(|r| r.len() == 1 && r[0].is_empty()).unwrap_or(false) {
            rows.pop();
        }
        let n = name.trim_end_matches('*');
        let (l, r) = match n {
            "pmatrix" => ("(", ")"),
            "bmatrix" => ("[", "]"),
            "Bmatrix" => ("{", "}"),
            "vmatrix" => ("∣", "∣"),
            "Vmatrix" => ("∥", "∥"),
            "cases" | "dcases" => ("{", "."),
            "rcases" => (".", "}"),
            _ => (".", "."),
        };
        let pair = matches!(n, "aligned" | "align" | "split" | "alignat" | "alignedat" | "eqnarray" | "flalign");
        let align: Vec<char> = if !cols.is_empty() { cols.chars().filter(|c| "lcr".contains(*c)).collect() } else if matches!(n, "cases" | "dcases" | "rcases") { vec!['l', 'l'] } else { vec![] };
        let style = if matches!(n, "smallmatrix") { Style::S } else if pair || matches!(n, "dcases" | "gathered" | "gather" | "equation" | "multline") { Style::D } else { Style::T };
        let gap = if pair { 0.0 } else if matches!(n, "cases" | "dcases") { 1.0 } else if n == "smallmatrix" { 0.4 } else { 1.0 };
        if matches!(n, "equation" | "displaymath") {
            let body = rows.into_iter().flatten().flatten().collect();
            return Node::List(body, Cls::Ord);
        }
        Node::Array { rows, align, l: l.into(), r: r.into(), style, gap, pair }
    }
}

fn unescape_text(s: &str) -> String {
    let mut out = String::new();
    let mut it = s.chars().peekable();
    while let Some(c) = it.next() {
        if c == '\\' {
            match it.peek() {
                Some(&n) if "%$#&_{} ".contains(n) => {
                    out.push(n);
                    it.next();
                }
                Some(&n) if n.is_ascii_alphabetic() => {
                    let mut name = String::new();
                    while let Some(&n) = it.peek() {
                        if n.is_ascii_alphabetic() {
                            name.push(n);
                            it.next();
                        } else {
                            break;
                        }
                    }
                    out.push_str(match name.as_str() {
                        "textdegree" | "degree" => "°",
                        "ldots" | "dots" => "…",
                        _ => "",
                    });
                }
                _ => {}
            }
        } else if c != '{' && c != '}' {
            out.push(if c == '~' { '\u{a0}' } else { c });
        }
    }
    out
}

fn plain(v: &[Node]) -> String {
    v.iter()
        .map(|n| match n {
            Node::Sym { s, .. } | Node::Text { s, .. } => s.clone(),
            Node::List(v, _) => plain(v),
            _ => String::new(),
        })
        .collect()
}

fn parse_len(s: &str) -> f32 {
    let s = s.trim();
    let num: String = s.chars().take_while(|c| c.is_ascii_digit() || *c == '.' || *c == '-').collect();
    let v: f32 = num.parse().unwrap_or(0.0);
    let unit = &s[num.len()..].trim();
    match *unit {
        "em" => v,
        "ex" => v * 0.431,
        "mu" => v / 18.0,
        "pt" => v / 10.0,
        "px" => v / 10.0,
        "cm" => v * 2.845,
        "mm" => v * 0.2845,
        "in" => v * 7.227,
        _ => v / 18.0,
    }
}

// ---------- mhchem ----------
/// \ce{…} as TeX: formulas upright with subscript counts and superscript charges, reaction arrows,
/// states, and stoichiometric numbers.
pub fn ce_to_tex(s: &str) -> String {
    let mut out = String::new();
    let chars: Vec<char> = s.chars().collect();
    let mut i = 0;
    let arrows: &[(&str, &str)] = &[
        ("<=>>", "\\rightleftharpoons"),
        ("<<=>", "\\rightleftharpoons"),
        ("<=>", "\\rightleftharpoons"),
        ("<-->", "\\leftrightarrows"),
        ("<->", "\\leftrightarrow"),
        ("->", "\\rightarrow"),
        ("<-", "\\leftarrow"),
    ];
    while i < chars.len() {
        let rest: String = chars[i..].iter().collect();
        // arrows, with optional [above][below]
        if let Some((a, t)) = arrows.iter().find(|(a, _)| rest.starts_with(a)) {
            i += a.chars().count();
            let mut labels = Vec::new();
            while chars.get(i) == Some(&'[') {
                let start = i + 1;
                let mut j = start;
                while j < chars.len() && chars[j] != ']' {
                    j += 1;
                }
                labels.push(chars[start..j.min(chars.len())].iter().collect::<String>());
                i = j + 1;
            }
            let x = match *t {
                "\\rightarrow" => "\\xrightarrow",
                "\\leftarrow" => "\\xleftarrow",
                "\\rightleftharpoons" => "\\xrightleftharpoons",
                _ => "\\xleftrightarrow",
            };
            let label = |l: &str| if l.contains('$') { l.replace('$', "") } else { format!("\\ce{{{l}}}") };
            match labels.len() {
                0 => out += &format!(" \\mathrel{{{x}{{\\quad}}}} "),
                1 => out += &format!(" {x}{{{}}} ", label(&labels[0])),
                _ => out += &format!(" {x}[{}]{{{}}} ", label(&labels[1]), label(&labels[0])),
            }
            continue;
        }
        let c = chars[i];
        if c == '$' {
            let start = i + 1;
            let mut j = start;
            while j < chars.len() && chars[j] != '$' {
                j += 1;
            }
            out += &chars[start..j.min(chars.len())].iter().collect::<String>();
            i = j + 1;
            continue;
        }
        if c.is_whitespace() {
            // a lone + between species is an operator
            if chars.get(i + 1) == Some(&'+') && chars.get(i + 2).map(|c| c.is_whitespace()).unwrap_or(true) {
                out += " + ";
                i += 3;
                continue;
            }
            out.push(' ');
            i += 1;
            continue;
        }
        // a word: everything up to the next space or arrow
        let mut j = i;
        while j < chars.len() && !chars[j].is_whitespace() {
            let r: String = chars[j..].iter().collect();
            if arrows.iter().any(|(a, _)| r.starts_with(a)) {
                break;
            }
            j += 1;
        }
        let word: Vec<char> = chars[i..j].to_vec();
        out += &ce_word(&word);
        i = j;
    }
    out
}

fn ce_word(w: &[char]) -> String {
    let s: String = w.iter().collect();
    match s.as_str() {
        "+" => return " + ".into(),
        "v" | "(v)" => return "\\downarrow ".into(),
        "^" | "(^)" => return "\\uparrow ".into(),
        "=" => return " = ".into(),
        _ => {}
    }
    let mut out = String::new();
    let mut i = 0;
    // a leading number (or fraction) is the stoichiometric coefficient
    let mut coef = String::new();
    while i < w.len() && (w[i].is_ascii_digit() || w[i] == '/' || w[i] == '.') {
        coef.push(w[i]);
        i += 1;
    }
    if !coef.is_empty() && i < w.len() {
        if let Some((a, b)) = coef.split_once('/') {
            out += &format!("\\tfrac{{{a}}}{{{b}}}\\,");
        } else {
            out += &coef;
            out += "\\,";
        }
    } else if !coef.is_empty() {
        return coef; // just a number
    }
    let elements = w[i..].iter().filter(|c| c.is_ascii_uppercase()).count();
    let mut last_was_atom = false;
    while i < w.len() {
        let c = w[i];
        if c.is_ascii_uppercase() {
            let mut sym = c.to_string();
            i += 1;
            while i < w.len() && w[i].is_ascii_lowercase() {
                sym.push(w[i]);
                i += 1;
            }
            out += &format!("\\mathrm{{{sym}}}");
            last_was_atom = true;
            continue;
        }
        if c.is_ascii_digit() && last_was_atom {
            let mut num = String::new();
            while i < w.len() && w[i].is_ascii_digit() {
                num.push(w[i]);
                i += 1;
            }
            // Cu2+ (one element, digits, then a sign at the end) is a charge
            let at_end_sign = i + 1 == w.len() && (w[i] == '+' || w[i] == '-');
            if at_end_sign && elements == 1 {
                out += &format!("^{{{num}{}}}", w[i]);
                i += 1;
                continue;
            }
            out += &format!("_{{{num}}}");
            continue;
        }
        if c == '^' {
            i += 1;
            let mut sup = String::new();
            if w.get(i) == Some(&'{') {
                i += 1;
                while i < w.len() && w[i] != '}' {
                    sup.push(w[i]);
                    i += 1;
                }
                i += 1;
            } else {
                while i < w.len() && (w[i].is_ascii_alphanumeric() || w[i] == '+' || w[i] == '-') {
                    sup.push(w[i]);
                    i += 1;
                }
            }
            out += &format!("^{{{}}}", sup.replace('-', "{-}"));
            continue;
        }
        if (c == '+' || c == '-') && i + 1 == w.len() && last_was_atom {
            out += &format!("^{{{}}}", if c == '-' { "{-}" } else { "+" });
            i += 1;
            continue;
        }
        if c == '_' {
            i += 1;
            let mut sub = String::new();
            if w.get(i) == Some(&'{') {
                i += 1;
                while i < w.len() && w[i] != '}' {
                    sub.push(w[i]);
                    i += 1;
                }
                i += 1;
            } else if i < w.len() {
                sub.push(w[i]);
                i += 1;
            }
            out += &format!("_{{{sub}}}");
            continue;
        }
        if c == '(' || c == '[' {
            // a state: (s), (l), (g), (aq)
            let close = if c == '(' { ')' } else { ']' };
            let rest: String = w[i..].iter().collect();
            if let Some(end) = rest.find(close) {
                let inner = &rest[1..end];
                if !inner.is_empty() && inner.chars().all(|ch| ch.is_ascii_lowercase()) {
                    out += &format!("\\mathrm{{{c}{inner}{close}}}");
                    i += end + 1;
                    last_was_atom = false;
                    continue;
                }
            }
            out.push(c);
            i += 1;
            last_was_atom = false;
            continue;
        }
        if c == ')' || c == ']' {
            out.push(c);
            i += 1;
            last_was_atom = true;
            continue;
        }
        if c == '.' || c == '*' || c == '·' {
            out += "\\cdot ";
            i += 1;
            last_was_atom = false;
            continue;
        }
        if c == '-' {
            out += "{-}";
            i += 1;
            continue;
        }
        if c == '=' {
            out += "=";
            i += 1;
            continue;
        }
        if c == '#' {
            out += "\\equiv ";
            i += 1;
            continue;
        }
        if c.is_ascii_lowercase() {
            out += &format!("\\mathrm{{{c}}}");
            i += 1;
            last_was_atom = true;
            continue;
        }
        out.push(c);
        i += 1;
        last_was_atom = false;
    }
    out
}

/// \pu{…}: numbers and units, upright with thin spaces.
fn pu_to_tex(s: &str) -> String {
    let mut out = String::new();
    for (k, w) in s.split_whitespace().enumerate() {
        if k > 0 {
            out += "\\,";
        }
        if w.chars().next().map(|c| c.is_ascii_digit() || c == '-').unwrap_or(false) {
            out += &w.replace('-', "{-}");
        } else {
            let unit = w.replace('^', "}^{").replace('.', "}\\cdot\\mathrm{");
            out += &format!("\\mathrm{{{unit}}}");
        }
    }
    out
}

// ---------- layout ----------
#[derive(Clone, Copy)]
struct Cx {
    style: Style,
    base: f32,
    color: Color32,
}

impl Cx {
    fn em(&self) -> f32 {
        self.base * self.style.mult()
    }
    fn with(&self, style: Style) -> Cx {
        Cx { style, ..*self }
    }
}

const AXIS: f32 = 0.25;
const RULE: f32 = 0.04;
const XHEIGHT: f32 = 0.431;

/// A run of text in one font.
fn glyphs(s: &str, font: Fnt, cx: &Cx) -> Bx {
    let em = cx.em();
    let mut b = Bx::default();
    let mut run = String::new();
    let mut run_font = "";
    let mut run_x = 0.0;
    let mut x = 0.0;
    for ch in s.chars() {
        let (adv, h, d, f) = metrics(font.name(), ch);
        if (f != run_font || b.ic > 0.0) && !run.is_empty() {
            b.items.push(Item::Glyph { x: run_x, y: 0.0, s: std::mem::take(&mut run), font: run_font, size: em, color: cx.color });
        }
        if run.is_empty() {
            run_font = f;
            run_x = x;
        }
        run.push(ch);
        // an italic letter is followed by its overhang (KaTeX's margin-right: italic)
        b.ic = italic(f, ch) * em;
        x += adv * em + b.ic;
        b.h = b.h.max(h * em);
        b.d = b.d.max(d * em);
    }
    if !run.is_empty() {
        b.items.push(Item::Glyph { x: run_x, y: 0.0, s: run, font: run_font, size: em, color: cx.color });
    }
    b.w = x;
    b
}

fn space_between(a: Cls, b: Cls, style: Style) -> f32 {
    use Cls::*;
    // in mu: 3 thin, 4 medium, 5 thick; negative: only outside script styles
    let v: i32 = match (a, b) {
        (Ord, Op) | (Ord, Inner) | (Op, Ord) | (Op, Op) | (Op, Inner) | (Close, Op) | (Close, Inner) => 3,
        (Ord, Bin) | (Bin, Ord) | (Bin, Op) | (Bin, Open) | (Bin, Inner) | (Close, Bin) | (Inner, Bin) => -4,
        (Ord, Rel) | (Op, Rel) | (Rel, Ord) | (Rel, Op) | (Rel, Open) | (Rel, Inner) | (Close, Rel) | (Inner, Rel) => -5,
        (Punct, _) => -3,
        (Inner, Ord) | (Inner, Op) | (Inner, Open) | (Inner, Punct) | (Inner, Inner) | (Inner, Close) => -3,
        _ => 0,
    };
    if v == 0 {
        return 0.0;
    }
    if v < 0 && style.tight() {
        return 0.0;
    }
    v.abs() as f32 / 18.0
}

fn hlist(nodes: &[Node], cx: &Cx) -> Bx {
    let mut cx = *cx;
    let mut out = Bx::default();
    let mut prev: Option<Cls> = None;
    for n in nodes {
        match n {
            Node::Style(s) => {
                cx = cx.with(*s);
                continue;
            }
            Node::Space(em) => {
                out.w += em * cx.em();
                continue;
            }
            _ => {}
        }
        let mut cls = cls_of(n);
        // a binary operator with nothing to its left is ordinary
        if cls == Cls::Bin && matches!(prev, None | Some(Cls::Bin) | Some(Cls::Op) | Some(Cls::Rel) | Some(Cls::Open) | Some(Cls::Punct)) {
            cls = Cls::Ord;
        }
        if let Some(p) = prev {
            // and so is one before a relation or closing
            let p2 = if p == Cls::Bin && matches!(cls, Cls::Rel | Cls::Close | Cls::Punct) { Cls::Ord } else { p };
            out.w += space_between(p2, cls, cx.style) * cx.em();
        }
        let b = node(n, &cx);
        let x = out.w;
        out.h = out.h.max(b.h);
        out.d = out.d.max(b.d);
        out.w += b.w;
        out.add(b, x, 0.0);
        prev = Some(cls);
    }
    out
}

fn is_char(n: &Node) -> bool {
    matches!(n, Node::Sym { s, .. } if s.chars().count() == 1)
}

fn node(n: &Node, cx: &Cx) -> Bx {
    let em = cx.em();
    match n {
        Node::Sym { s, font, .. } => glyphs(s, *font, cx),
        Node::Text { s, font } => glyphs(s, *font, cx),
        Node::List(v, _) => hlist(v, cx),
        Node::Error(s) => glyphs(s, Fnt::Main, &Cx { color: Color32::from_rgb(0xcc, 0x00, 0x00), ..*cx }),
        Node::Style(_) | Node::Space(_) => Bx::default(),
        Node::Color(c, v) => hlist(v, &Cx { color: *c, ..*cx }),
        Node::Phantom(v) => {
            let b = hlist(v, cx);
            Bx { w: b.w, h: b.h, d: b.d, items: vec![], ic: 0.0 }
        }
        Node::Boxed(v) => {
            let b = hlist(v, cx);
            let pad = 0.3 * em;
            let t = (RULE * em).max(1.0);
            let mut out = Bx { w: b.w + 2.0 * pad, h: b.h + pad, d: b.d + pad, items: vec![], ic: 0.0 };
            let (w, h, d) = (out.w, out.h, out.d);
            out.add(b, pad, 0.0);
            out.items.push(Item::Path { pts: vec![(0.0, -h), (w, -h), (w, d), (0.0, d), (0.0, -h)], width: t, color: cx.color });
            out
        }
        Node::Cancel(v) => {
            let mut b = hlist(v, cx);
            let (w, h, d) = (b.w, b.h, b.d);
            b.items.push(Item::Path { pts: vec![(0.0, d), (w, -h)], width: (RULE * em).max(1.0), color: cx.color });
            b
        }
        Node::Scripts { base, sup, sub } => scripts(base, sup.as_deref(), sub.as_deref(), cx),
        Node::Op { .. } => op(n, cx).0,
        Node::Frac { num, den, style, bar, delims } => frac(num, den, *style, *bar, delims.as_ref(), cx),
        Node::Sqrt { body, index } => sqrt(body, index.as_deref(), cx),
        Node::Accent { body, acc } => accent(body, *acc, cx),
        Node::Line { body, over } => {
            let b = hlist(body, cx);
            let t = RULE * em;
            let mut out = Bx { w: b.w, h: b.h, d: b.d, items: vec![], ic: 0.0 };
            if *over {
                let y = -(b.h + 3.0 * t);
                out.h = b.h + 4.0 * t;
                out.items.push(Item::Rule { x: 0.0, y: y - t, w: b.w, h: t, color: cx.color });
            } else {
                let y = b.d + 3.0 * t;
                out.d = b.d + 4.0 * t;
                out.items.push(Item::Rule { x: 0.0, y, w: b.w, h: t, color: cx.color });
            }
            out.add(b, 0.0, 0.0);
            out
        }
        Node::LeftRight { l, r, body } => {
            let b = hlist(body, cx);
            let need = 2.0 * (b.h - AXIS * em).max(b.d + AXIS * em);
            let lb = delimiter(l, need, cx);
            let rb = delimiter(r, need, cx);
            let mut out = Bx { w: lb.w + b.w + rb.w, h: b.h.max(lb.h).max(rb.h), d: b.d.max(lb.d).max(rb.d), items: vec![], ic: 0.0 };
            let (lw, bw) = (lb.w, b.w);
            out.add(lb, 0.0, 0.0);
            out.add(b, lw, 0.0);
            out.add(rb, lw + bw, 0.0);
            out
        }
        Node::Big { delim, size, .. } => {
            let need = [0.0, 1.2, 1.8, 2.4, 3.0][*size] * em;
            delimiter(delim, need, cx)
        }
        Node::Array { rows, align, l, r, style, gap, pair } => array(rows, align, l, r, *style, *gap, *pair, cx),
        Node::Stack { base, over, under, .. } => {
            let b = hlist(base, cx);
            let sc = cx.with(cx.style.sup());
            let ob = over.as_ref().map(|o| hlist(o, &sc));
            let ub = under.as_ref().map(|u| hlist(u, &sc));
            stack(b, ob, ub, 0.111 * em, cx)
        }
        Node::Arrow { ch, over, under } => {
            let sc = cx.with(cx.style.sup());
            let ob = over.as_ref().map(|o| hlist(o, &sc));
            let ub = under.as_ref().map(|u| hlist(u, &sc));
            let label_w = ob.as_ref().map(|b| b.w).unwrap_or(0.0).max(ub.as_ref().map(|b| b.w).unwrap_or(0.0));
            let w = (label_w + 0.9 * em).max(1.6 * em);
            let arrow = stretch_arrow(*ch, w, cx);
            let mut s = stack(arrow, ob, ub, 0.1 * em, cx);
            // a little room on each side
            let pad = 0.14 * em;
            s.items = s.items.into_iter().map(|i| i.shifted(pad, 0.0)).collect();
            s.w += 2.0 * pad;
            s
        }
    }
}

/// An arrow `w` wide: the glyph for short ones, a drawn shaft and head for long ones.
fn stretch_arrow(ch: char, w: f32, cx: &Cx) -> Bx {
    let em = cx.em();
    let y = -AXIS * em;
    let t = (0.045 * em).max(1.0);
    let head = 0.28 * em;
    let mut b = Bx { w, h: 0.5 * em, d: 0.0, items: vec![], ic: 0.0 };
    let col = cx.color;
    let right = |b: &mut Bx, y: f32, harpoon: bool| {
        b.items.push(Item::Path { pts: vec![(0.0, y), (w - t, y)], width: t, color: col });
        if harpoon {
            b.items.push(Item::Path { pts: vec![(w - head, y - head * 0.75), (w - t / 2.0, y)], width: t, color: col });
        } else {
            b.items.push(Item::Path { pts: vec![(w - head, y - head * 0.75), (w - t / 2.0, y), (w - head, y + head * 0.75)], width: t, color: col });
        }
    };
    let left = |b: &mut Bx, y: f32, harpoon: bool| {
        b.items.push(Item::Path { pts: vec![(t, y), (w, y)], width: t, color: col });
        if harpoon {
            b.items.push(Item::Path { pts: vec![(head, y + head * 0.75), (t / 2.0, y)], width: t, color: col });
        } else {
            b.items.push(Item::Path { pts: vec![(head, y - head * 0.75), (t / 2.0, y), (head, y + head * 0.75)], width: t, color: col });
        }
    };
    match ch {
        '←' => left(&mut b, y, false),
        '↔' => {
            right(&mut b, y, false);
            left(&mut b, y, false);
        }
        '⇌' => {
            right(&mut b, y - 0.1 * em, true);
            left(&mut b, y + 0.1 * em, true);
        }
        '⇀' => right(&mut b, y, true),
        '⇒' | '⇐' | '⇔' => {
            let d = 0.09 * em;
            b.items.push(Item::Path { pts: vec![(0.0, y - d), (w - head * 0.6, y - d)], width: t, color: col });
            b.items.push(Item::Path { pts: vec![(0.0, y + d), (w - head * 0.6, y + d)], width: t, color: col });
            b.items.push(Item::Path { pts: vec![(w - head, y - head), (w - t / 2.0, y), (w - head, y + head)], width: t, color: col });
        }
        '↦' => {
            right(&mut b, y, false);
            b.items.push(Item::Path { pts: vec![(t / 2.0, y - 0.2 * em), (t / 2.0, y + 0.2 * em)], width: t, color: col });
        }
        _ => right(&mut b, y, false),
    }
    b.d = 0.1 * em;
    b
}

/// A base with things centered above and below it.
fn stack(b: Bx, over: Option<Bx>, under: Option<Bx>, gap: f32, _cx: &Cx) -> Bx {
    let w = b.w.max(over.as_ref().map(|o| o.w).unwrap_or(0.0)).max(under.as_ref().map(|u| u.w).unwrap_or(0.0));
    let mut out = Bx { w, h: b.h, d: b.d, items: vec![], ic: 0.0 };
    let (bh, bd, bw) = (b.h, b.d, b.w);
    out.add(b, (w - bw) / 2.0, 0.0);
    if let Some(o) = over {
        let y = -(bh + gap + o.d);
        out.h = bh + gap + o.d + o.h;
        let ow = o.w;
        out.add(o, (w - ow) / 2.0, y);
    }
    if let Some(u) = under {
        let y = bd + gap + u.h;
        out.d = bd + gap + u.h + u.d;
        let uw = u.w;
        out.add(u, (w - uw) / 2.0, y);
    }
    out
}

/// A big operator or a named function; returns the box and whether its scripts go above/below.
fn op(n: &Node, cx: &Cx) -> (Bx, bool) {
    let Node::Op { s, font, limits, big } = n else { return (Bx::default(), false) };
    let em = cx.em();
    if *big {
        let ch = s.chars().next().unwrap_or('∑');
        let font = if cx.style == Style::D { Fnt::Size2 } else { Fnt::Size1 };
        let mut b = glyphs(s, font, cx);
        // centered on the axis
        let shift = (b.h - b.d) / 2.0 - AXIS * em;
        b.items = b.items.into_iter().map(|i| i.shifted(0.0, shift)).collect();
        let (h, d) = (b.h - shift, b.d + shift);
        b.h = h;
        b.d = d;
        let integral = matches!(ch, '∫' | '∬' | '∭' | '∮' | '∯');
        let lim = limits.unwrap_or(cx.style == Style::D && !integral);
        return (b, lim);
    }
    let b = glyphs(s, *font, &Cx { ..*cx });
    let lim = limits.unwrap_or(cx.style == Style::D);
    (b, lim)
}

fn scripts(base: &Node, sup: Option<&Node>, sub: Option<&Node>, cx: &Cx) -> Bx {
    let em = cx.em();
    let sc = cx.with(cx.style.sup());
    let sup_b = sup.map(|s| node(s, &sc));
    let sub_b = sub.map(|s| node(s, &sc));
    // operators with limits: above and below
    if let Node::Op { .. } = base {
        let (b, lim) = op(base, cx);
        if lim {
            let (bh, bd, bw) = (b.h, b.d, b.w);
            let w = bw.max(sup_b.as_ref().map(|x| x.w).unwrap_or(0.0)).max(sub_b.as_ref().map(|x| x.w).unwrap_or(0.0));
            let mut out = Bx { w, h: bh, d: bd, items: vec![], ic: 0.0 };
            out.add(b, (w - bw) / 2.0, 0.0);
            if let Some(s) = sup_b {
                let gap = (0.111 * em).max(0.2 * em - s.d);
                let y = -(bh + gap + s.d);
                out.h = bh + gap + s.d + s.h + 0.1 * em;
                let sw = s.w;
                out.add(s, (w - sw) / 2.0, y);
            }
            if let Some(s) = sub_b {
                let gap = (0.166 * em).max(0.6 * em - s.h);
                let y = bd + gap + s.h;
                out.d = bd + gap + s.h + s.d + 0.1 * em;
                let sw = s.w;
                out.add(s, (w - sw) / 2.0, y);
            }
            return out;
        }
        return attach(b, false, sup_b, sub_b, cx, matches!(base, Node::Op { big: true, s, .. } if s.starts_with('∫')));
    }
    let b = node(base, cx);
    attach(b, is_char(base), sup_b, sub_b, cx, false)
}

fn attach(b: Bx, simple: bool, sup: Option<Bx>, sub: Option<Bx>, cx: &Cx, integral: bool) -> Bx {
    let em = cx.em();
    let sem = cx.with(cx.style.sup()).em();
    let (mut sup_shift, mut sub_shift) = if simple { (0.0, 0.0) } else { (b.h - 0.386 * sem, b.d + 0.05 * sem) };
    let min_sup = match cx.style {
        Style::D => 0.413,
        _ => 0.363,
    } * em;
    let mut out = Bx { w: b.w, h: b.h, d: b.d, items: vec![], ic: if sup.is_none() && sub.is_none() { b.ic } else { 0.0 } };
    let bw = b.w;
    let kern = if integral { -0.2 * em } else { -b.ic };
    out.add(b, 0.0, 0.0);
    match (sup, sub) {
        (Some(s), None) => {
            sup_shift = sup_shift.max(min_sup).max(s.d + 0.25 * XHEIGHT * em);
            out.h = out.h.max(sup_shift + s.h);
            out.w = bw + s.w + 0.05 * em;
            out.add(s, bw, -sup_shift);
        }
        (None, Some(s)) => {
            sub_shift = sub_shift.max(0.15 * em).max(s.h - 0.8 * XHEIGHT * em);
            out.d = out.d.max(sub_shift + s.d);
            out.w = bw + s.w + 0.05 * em + kern.min(0.0);
            out.add(s, bw + kern, sub_shift);
        }
        (Some(p), Some(s)) => {
            sup_shift = sup_shift.max(min_sup).max(p.d + 0.25 * XHEIGHT * em);
            sub_shift = sub_shift.max(0.247 * em);
            let psi = 4.0 * RULE * em;
            let gap = (sup_shift - p.d) - (s.h - sub_shift);
            if gap < psi {
                sub_shift += psi - gap;
                let psi2 = 0.8 * XHEIGHT * em - (sup_shift - p.d);
                if psi2 > 0.0 {
                    sup_shift += psi2;
                    sub_shift -= psi2;
                }
            }
            out.h = out.h.max(sup_shift + p.h);
            out.d = out.d.max(sub_shift + s.d);
            out.w = bw + p.w.max(s.w + kern) + 0.05 * em;
            out.add(p, bw, -sup_shift);
            out.add(s, bw + kern, sub_shift);
        }
        (None, None) => {}
    }
    out
}

fn frac(num: &[Node], den: &[Node], style: Option<Style>, bar: bool, delims: Option<&(String, String)>, cx: &Cx) -> Bx {
    let cx = style.map(|s| cx.with(s)).unwrap_or(*cx);
    let em = cx.em();
    let inner = cx.with(cx.style.frac());
    let nb = hlist(num, &inner);
    let db = hlist(den, &inner);
    let t = if bar { RULE * em } else { 0.0 };
    let display = cx.style == Style::D;
    let (mut num_shift, mut den_shift) = if display { (0.677 * em, 0.686 * em) } else { (if bar { 0.394 } else { 0.444 } * em, 0.345 * em) };
    let clearance = if display { 3.0 * RULE * em } else { RULE * em };
    let axis = AXIS * em;
    if bar {
        let c1 = (num_shift - nb.d) - (axis + t / 2.0);
        if c1 < clearance {
            num_shift += clearance - c1;
        }
        let c2 = (axis - t / 2.0) - (db.h - den_shift);
        if c2 < clearance {
            den_shift += clearance - c2;
        }
    } else {
        let c = (num_shift - nb.d) - (db.h - den_shift);
        let need = if display { 7.0 * RULE * em } else { 3.0 * RULE * em };
        if c < need {
            num_shift += (need - c) / 2.0;
            den_shift += (need - c) / 2.0;
        }
    }
    let pad = 0.12 * em;
    let w = nb.w.max(db.w) + 2.0 * pad;
    let mut out = Bx { w, h: num_shift + nb.h, d: den_shift + db.d, items: vec![], ic: 0.0 };
    let (nw, dw) = (nb.w, db.w);
    out.add(nb, (w - nw) / 2.0, -num_shift);
    out.add(db, (w - dw) / 2.0, den_shift);
    if bar {
        out.items.push(Item::Rule { x: pad * 0.5, y: -axis - t / 2.0, w: w - pad, h: t, color: cx.color });
    }
    if let Some((l, r)) = delims {
        let need = 2.0 * (out.h - axis).max(out.d + axis);
        let lb = delimiter(l, need, &cx);
        let rb = delimiter(r, need, &cx);
        let mut o2 = Bx { w: lb.w + out.w + rb.w, h: out.h.max(lb.h), d: out.d.max(lb.d), items: vec![], ic: 0.0 };
        let (lw, ow) = (lb.w, out.w);
        o2.add(lb, 0.0, 0.0);
        o2.add(out, lw, 0.0);
        o2.add(rb, lw + ow, 0.0);
        return o2;
    }
    out
}

fn sqrt(body: &[Node], index: Option<&[Node]>, cx: &Cx) -> Bx {
    let em = cx.em();
    let b = hlist(body, cx);
    let t = (RULE * em).max(1.0);
    let phi = if cx.style == Style::D { XHEIGHT * em } else { t };
    let clear = t + phi / 4.0;
    let top = -(b.h.max(0.7 * em) + clear);
    let bottom = b.d.max(0.2 * em) + 0.05 * em;
    let hh = bottom - top;
    let sw = (0.55 * em).max(0.25 * hh).min(0.9 * em);
    let mut out = Bx { w: 0.0, h: -top + t, d: bottom, items: vec![], ic: 0.0 };
    let mut x0 = 0.0;
    if let Some(ix) = index {
        let ib = hlist(ix, &cx.with(Style::SS));
        let iw = ib.w;
        let y = bottom - 0.6 * hh;
        out.h = out.h.max(-y + ib.h);
        out.add(ib, 0.1 * em, y);
        x0 = (iw + 0.1 * em - 0.5 * sw).max(0.0);
    }
    let pts = vec![
        (x0, bottom - 0.42 * hh),
        (x0 + 0.15 * sw, bottom - 0.5 * hh),
        (x0 + 0.5 * sw, bottom),
        (x0 + sw, top + t / 2.0),
        (x0 + sw + b.w + 0.1 * em, top + t / 2.0),
    ];
    out.items.push(Item::Path { pts, width: t * 1.1, color: cx.color });
    let bw = b.w;
    out.add(b, x0 + sw + 0.05 * em, 0.0);
    out.w = x0 + sw + bw + 0.15 * em;
    out
}

fn accent(body: &[Node], acc: char, cx: &Cx) -> Bx {
    let em = cx.em();
    let b = hlist(body, cx);
    let single = body.len() == 1 && is_char(&body[0]);
    let mut out = Bx { w: b.w, h: b.h, d: b.d, items: vec![], ic: 0.0 };
    let (bw, bh) = (b.w, b.h);
    out.add(b, 0.0, 0.0);
    if acc == '→' || acc == '←' || (!single && matches!(acc, 'ˆ' | '˜')) {
        // wide: a drawn arrow or hat over the whole body
        let y = -(bh + 0.12 * em);
        let t = (0.045 * em).max(1.0);
        let w = bw.max(0.5 * em);
        let items = match acc {
            '→' => vec![vec![(0.0, y), (w, y)], vec![(w - 0.2 * em, y - 0.12 * em), (w, y), (w - 0.2 * em, y + 0.12 * em)]],
            '←' => vec![vec![(0.0, y), (w, y)], vec![(0.2 * em, y - 0.12 * em), (0.0, y), (0.2 * em, y + 0.12 * em)]],
            'ˆ' => vec![vec![(0.0, y + 0.02 * em), (w / 2.0, y - 0.18 * em), (w, y + 0.02 * em)]],
            _ => vec![(0..=12).map(|i| {
                let f = i as f32 / 12.0;
                (f * w, y - 0.06 * em * (f * std::f32::consts::TAU).sin())
            }).collect()],
        };
        for pts in items {
            out.items.push(Item::Path { pts, width: t, color: cx.color });
        }
        out.h = bh + 0.32 * em;
        return out;
    }
    let a = glyphs(&acc.to_string(), Fnt::Main, cx);
    // accents are drawn for x-height letters; lift for taller ones
    let lift = (bh - XHEIGHT * em).max(0.0);
    let skew = if single { 0.08 * em } else { 0.0 };
    let (aw, ah) = (a.w, a.h);
    out.add(a, (bw - aw) / 2.0 + skew, -lift);
    out.h = out.h.max(ah + lift);
    out
}

/// A delimiter at least `need` tall (height + depth), centered on the axis: the normal glyph if
/// it's tall enough, else KaTeX's Size1–4 fonts, else stretched.
fn delimiter(d: &str, need: f32, cx: &Cx) -> Bx {
    let em = cx.em();
    if d == "." || d.is_empty() {
        return Bx { w: 0.12 * em, ..Default::default() };
    }
    let ch = d.chars().next().unwrap();
    let ch = match ch {
        '|' => '∣',
        '<' => '⟨',
        '>' => '⟩',
        c => c,
    };
    let fonts = [Fnt::Main, Fnt::Size1, Fnt::Size2, Fnt::Size3, Fnt::Size4];
    let mut chosen = None;
    for f in fonts {
        let (_, h, dd, name) = metrics(f.name(), ch);
        if name != f.name() {
            continue;
        }
        if (h + dd) * em >= need * 0.9 {
            chosen = Some((f, 1.0));
            break;
        }
    }
    let (font, scale) = chosen.unwrap_or_else(|| {
        let (_, h, dd, _) = metrics(Fnt::Size4.name(), ch);
        (Fnt::Size4, (need / ((h + dd) * em).max(1.0)).max(1.0))
    });
    let c2 = Cx { base: cx.base * scale, ..*cx };
    let mut b = glyphs(&ch.to_string(), font, &c2);
    let shift = (b.h - b.d) / 2.0 - AXIS * em;
    b.items = b.items.into_iter().map(|i| i.shifted(0.0, shift)).collect();
    let (h, dd) = (b.h - shift, b.d + shift);
    b.h = h;
    b.d = dd;
    b
}

#[allow(clippy::too_many_arguments)]
fn array(rows: &[Vec<Vec<Node>>], align: &[char], l: &str, r: &str, style: Style, gap: f32, pair: bool, cx: &Cx) -> Bx {
    let cx2 = cx.with(style);
    let em = cx2.em();
    let cells: Vec<Vec<Bx>> = rows.iter().map(|row| row.iter().map(|c| hlist(c, &cx2)).collect()).collect();
    let ncols = cells.iter().map(|r| r.len()).max().unwrap_or(0);
    let mut col_w = vec![0.0f32; ncols];
    for row in &cells {
        for (j, c) in row.iter().enumerate() {
            col_w[j] = col_w[j].max(c.w);
        }
    }
    let strut_h = 0.7 * cx.em() * if style == Style::S { 0.7 } else { 1.0 };
    let strut_d = 0.3 * cx.em() * if style == Style::S { 0.7 } else { 1.0 };
    let row_gap = if pair { 0.25 * em } else if style == Style::D { 0.15 * em } else { 0.0 };
    let row_hd: Vec<(f32, f32)> = cells.iter().map(|r| (r.iter().map(|c| c.h).fold(strut_h, f32::max), r.iter().map(|c| c.d).fold(strut_d, f32::max))).collect();
    let total: f32 = row_hd.iter().map(|(h, d)| h + d).sum::<f32>() + row_gap * (rows.len().saturating_sub(1)) as f32;
    let col_sep = gap * em;
    let mut xs = Vec::with_capacity(ncols);
    let mut x = if col_sep > 0.0 && l == "." && r == "." && !pair { 0.0 } else { 0.0 };
    for (j, w) in col_w.iter().enumerate() {
        xs.push(x);
        x += w;
        if j + 1 < ncols {
            // aligned: pairs of (right, left) columns with no gap inside a pair
            x += if pair { if j % 2 == 0 { 0.0 } else { 1.0 * em } } else { col_sep };
        }
    }
    let width = x;
    let axis = AXIS * cx.em();
    let top = -(total / 2.0 + axis);
    let mut body = Bx { w: width, h: total / 2.0 + axis, d: total / 2.0 - axis, items: vec![], ic: 0.0 };
    let mut y = top;
    for (i, row) in cells.into_iter().enumerate() {
        let (h, d) = row_hd[i];
        let base = y + h;
        for (j, c) in row.into_iter().enumerate() {
            let a = if pair { if j % 2 == 0 { 'r' } else { 'l' } } else { align.get(j).copied().unwrap_or('c') };
            let cw = c.w;
            let dx = match a {
                'l' => 0.0,
                'r' => col_w[j] - cw,
                _ => (col_w[j] - cw) / 2.0,
            };
            body.add(c, xs[j] + dx, base);
        }
        y = base + d + row_gap;
    }
    if l == "." && r == "." {
        return body;
    }
    let need = body.h + body.d;
    let pad = 0.2 * em;
    let lb = delimiter(l, need, cx);
    let rb = delimiter(r, need, cx);
    let mut out = Bx { w: lb.w + pad + body.w + pad + rb.w, h: body.h.max(lb.h), d: body.d.max(lb.d), items: vec![], ic: 0.0 };
    let (lw, bw) = (lb.w, body.w);
    out.add(lb, 0.0, 0.0);
    out.add(body, lw + pad, 0.0);
    out.add(rb, lw + pad + bw + pad, 0.0);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lay(s: &str) -> Laid {
        layout(s, 20.0, Color32::BLACK)
    }

    fn texts(l: &Laid) -> String {
        l.items.iter().filter_map(|i| if let Item::Glyph { s, .. } = i { Some(s.as_str()) } else { None }).collect()
    }

    #[test]
    fn basics() {
        let l = lay(r"Z_C = \dfrac{1}{j\omega C}");
        assert!(l.width > 50.0 && l.height > 15.0 && l.depth > 5.0, "{} {} {}", l.width, l.height, l.depth);
        assert!(!l.items.iter().any(|i| matches!(i, Item::Glyph { color, .. } if *color != Color32::BLACK)), "no errors");
        assert_eq!(texts(&lay(r"\alpha + \beta")), "α+β");
        let s = lay(r"\sum_{i=1}^{n} i^2");
        assert!(s.height > 10.0);
        let m = lay(r"\begin{pmatrix} a & b \\ c & d \end{pmatrix}");
        assert!(m.height + m.depth > 30.0);
        let e = lay(r"\foo");
        assert!(e.items.iter().any(|i| matches!(i, Item::Glyph { color, .. } if *color != Color32::BLACK)));
    }

    #[test]
    fn chemistry() {
        assert_eq!(ce_to_tex("H2O"), "\\mathrm{H}_{2}\\mathrm{O}");
        assert_eq!(ce_to_tex("Cu2+"), "\\mathrm{Cu}^{2+}");
        assert_eq!(ce_to_tex("NH4+"), "\\mathrm{N}\\mathrm{H}_{4}^{+}");
        assert_eq!(ce_to_tex("SO4^2-"), "\\mathrm{S}\\mathrm{O}_{4}^{2{-}}");
        assert!(ce_to_tex("C7H6O3 + (CH3CO)2O -> C9H8O4 + CH3COOH").contains("rightarrow"));
        let l = lay(r"\ce{C9H8O4}");
        assert_eq!(texts(&l), "C9H8O4");
        assert!(l.depth > 2.0);
    }

    #[test]
    fn italic_correction() {
        // an italic V leans past its advance: room after it, a superscript after that, and a
        // subscript tucked back under it
        let v = lay("V");
        let (adv, ..) = metrics("KaTeX_Math-Italic", 'V');
        assert!(v.width > adv * 20.0 + 1.0, "{} vs {}", v.width, adv * 20.0);
        let x_of = |l: &Laid, s: &str| l.items.iter().find_map(|i| if let Item::Glyph { x, s: t, .. } = i { (t == s).then_some(*x) } else { None }).unwrap();
        let both = lay("V_1^2");
        assert!(x_of(&both, "2") > x_of(&both, "1") + 1.0);
        // upright text has none
        assert_eq!(lay(r"\mathrm{V}").width, metrics("KaTeX_Main-Regular", 'V').0 * 20.0);
    }
}
