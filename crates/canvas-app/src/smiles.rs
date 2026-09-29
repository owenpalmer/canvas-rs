//! Molecules from SMILES (\smiles{…} in questions and pages), drawn like SmilesDrawer's light
//! theme: the SMILES is parsed into atoms and bonds, laid out in 2D (rings as regular polygons,
//! chains as zigzags, by stress majorization from a classical-MDS start), then drawn as SVG and
//! rasterized with resvg. Anki gets the same drawing as a PNG.

use std::collections::VecDeque;
use std::sync::Arc;

use egui::{Color32, Rect, Sense, Stroke, StrokeKind, Ui};
use once_cell::sync::Lazy;

use crate::app::App;
use crate::theme::t;
use crate::widgets::{Ts, cr};

#[derive(Clone, Debug)]
struct Atom {
    el: String,
    aromatic: bool,
    charge: i32,
    hcount: Option<u32>,
    bracket: bool,
}

#[derive(Clone, Debug)]
struct Bond {
    a: usize,
    b: usize,
    order: u8, // 1, 2, 3; 4 = aromatic
    closure: bool,
}

#[derive(Debug)]
pub struct Mol {
    atoms: Vec<Atom>,
    bonds: Vec<Bond>,
}

// ---------- parsing ----------
pub fn parse(s: &str) -> Result<Mol, String> {
    let c: Vec<char> = s.trim().chars().collect();
    let mut atoms: Vec<Atom> = Vec::new();
    let mut bonds: Vec<Bond> = Vec::new();
    let mut stack: Vec<usize> = Vec::new();
    let mut prev: Option<usize> = None;
    let mut pending: Option<u8> = None;
    let mut rings: std::collections::HashMap<u32, (usize, Option<u8>)> = Default::default();
    let mut i = 0;
    let bad = |m: &str| Err(format!("Invalid SMILES ({m})"));
    while i < c.len() {
        let ch = c[i];
        match ch {
            '(' => {
                let Some(p) = prev else { return bad("branch without an atom") };
                stack.push(p);
                i += 1;
            }
            ')' => {
                prev = stack.pop();
                if prev.is_none() {
                    return bad("unbalanced )");
                }
                i += 1;
            }
            '-' | '/' | '\\' => {
                pending = Some(1);
                i += 1;
            }
            '=' => {
                pending = Some(2);
                i += 1;
            }
            '#' => {
                pending = Some(3);
                i += 1;
            }
            '$' => {
                pending = Some(3);
                i += 1;
            }
            ':' => {
                pending = Some(4);
                i += 1;
            }
            '.' => {
                prev = None;
                i += 1;
            }
            '%' | '0'..='9' => {
                let n: u32 = if ch == '%' {
                    let d: String = c.get(i + 1..i + 3).map(|x| x.iter().collect()).unwrap_or_default();
                    i += 3;
                    d.parse().map_err(|_| "bad ring number".to_string())?
                } else {
                    i += 1;
                    ch.to_digit(10).unwrap()
                };
                let Some(p) = prev else { return bad("ring bond without an atom") };
                if let Some((other, o2)) = rings.remove(&n) {
                    let order = pending.or(o2).unwrap_or(if atoms[p].aromatic && atoms[other].aromatic { 4 } else { 1 });
                    bonds.push(Bond { a: other, b: p, order, closure: true });
                } else {
                    rings.insert(n, (p, pending));
                }
                pending = None;
            }
            '[' => {
                let end = c[i..].iter().position(|x| *x == ']').map(|e| i + e).ok_or("unclosed [")?;
                let inner: String = c[i + 1..end].iter().collect();
                let a = bracket_atom(&inner)?;
                atoms.push(a);
                let idx = atoms.len() - 1;
                if let Some(p) = prev {
                    let order = pending.take().unwrap_or(if atoms[p].aromatic && atoms[idx].aromatic { 4 } else { 1 });
                    bonds.push(Bond { a: p, b: idx, order, closure: false });
                }
                pending = None;
                prev = Some(idx);
                i = end + 1;
            }
            _ => {
                // the organic subset
                let two: String = c[i..(i + 2).min(c.len())].iter().collect();
                let (el, arom, len) = if two == "Cl" || two == "Br" {
                    (two.clone(), false, 2)
                } else if "BCNOPSFI".contains(ch) {
                    (ch.to_string(), false, 1)
                } else if "bcnops".contains(ch) {
                    (ch.to_ascii_uppercase().to_string(), true, 1)
                } else if ch == '*' {
                    ("*".into(), false, 1)
                } else {
                    return bad(&format!("unexpected '{ch}'"));
                };
                atoms.push(Atom { el, aromatic: arom, charge: 0, hcount: None, bracket: false });
                let idx = atoms.len() - 1;
                if let Some(p) = prev {
                    let order = pending.take().unwrap_or(if atoms[p].aromatic && arom { 4 } else { 1 });
                    bonds.push(Bond { a: p, b: idx, order, closure: false });
                }
                pending = None;
                prev = Some(idx);
                i += len;
            }
        }
    }
    if !rings.is_empty() {
        return bad("unclosed ring");
    }
    if !stack.is_empty() {
        return bad("unbalanced (");
    }
    if atoms.is_empty() {
        return bad("empty");
    }
    Ok(Mol { atoms, bonds })
}

fn bracket_atom(s: &str) -> Result<Atom, String> {
    let c: Vec<char> = s.chars().collect();
    let mut i = 0;
    while i < c.len() && c[i].is_ascii_digit() {
        i += 1; // isotope
    }
    let mut el = String::new();
    let mut aromatic = false;
    if i < c.len() && c[i].is_ascii_lowercase() {
        aromatic = true;
        el.push(c[i].to_ascii_uppercase());
        i += 1;
        if i < c.len() && c[i].is_ascii_lowercase() && "e".contains(c[i]) {
            el.push(c[i]);
            i += 1;
        }
    } else if i < c.len() && c[i].is_ascii_uppercase() {
        el.push(c[i]);
        i += 1;
        if i < c.len() && c[i].is_ascii_lowercase() {
            el.push(c[i]);
            i += 1;
        }
    } else if c.get(i) == Some(&'*') {
        el.push('*');
        i += 1;
    } else {
        return Err(format!("Invalid SMILES (bad atom [{s}])"));
    }
    while i < c.len() && c[i] == '@' {
        i += 1;
    }
    let mut h = 0;
    if c.get(i) == Some(&'H') {
        i += 1;
        h = 1;
        let mut d = String::new();
        while i < c.len() && c[i].is_ascii_digit() {
            d.push(c[i]);
            i += 1;
        }
        if !d.is_empty() {
            h = d.parse().unwrap_or(1);
        }
    }
    let mut charge = 0;
    while i < c.len() && (c[i] == '+' || c[i] == '-') {
        let sign = if c[i] == '+' { 1 } else { -1 };
        i += 1;
        let mut d = String::new();
        while i < c.len() && c[i].is_ascii_digit() {
            d.push(c[i]);
            i += 1;
        }
        charge += sign * d.parse::<i32>().unwrap_or(1);
    }
    Ok(Atom { el, aromatic, charge, hcount: Some(h), bracket: true })
}

impl Mol {
    fn neighbors(&self) -> Vec<Vec<usize>> {
        let mut n = vec![vec![]; self.atoms.len()];
        for b in &self.bonds {
            n[b.a].push(b.b);
            n[b.b].push(b.a);
        }
        n
    }

    /// Hydrogens to write next to a heteroatom's symbol.
    fn hydrogens(&self, i: usize) -> u32 {
        let a = &self.atoms[i];
        if let Some(h) = a.hcount {
            return h;
        }
        let mut sum = 0.0f32;
        let mut arom = 0;
        for b in self.bonds.iter().filter(|b| b.a == i || b.b == i) {
            sum += if b.order == 4 { 1.0 } else { b.order as f32 };
            if b.order == 4 {
                arom += 1;
            }
        }
        if arom > 0 {
            sum += 1.0; // one of the aromatic bonds is double
        }
        let vals: &[u32] = match a.el.as_str() {
            "B" => &[3],
            "C" => &[4],
            "N" => &[3, 5],
            "O" => &[2],
            "P" => &[3, 5],
            "S" => &[2, 4, 6],
            "F" | "Cl" | "Br" | "I" => &[1],
            _ => &[0],
        };
        let s = sum.round() as u32;
        vals.iter().find(|v| **v >= s).map(|v| v - s).unwrap_or(0)
    }

    /// Rings: each ring bond closes the shortest path between its ends. Paths through other ring
    /// bonds cost a little more, so fused rings (naphthalene) come out as their own small rings
    /// rather than the perimeter.
    fn rings(&self) -> Vec<Vec<usize>> {
        let n = self.atoms.len();
        let mut out = Vec::new();
        for (k, c) in self.bonds.iter().enumerate().filter(|(_, b)| b.closure) {
            let mut dist = vec![f32::INFINITY; n];
            let mut prev = vec![usize::MAX; n];
            let mut done = vec![false; n];
            dist[c.a] = 0.0;
            loop {
                let Some(x) = (0..n).filter(|i| !done[*i] && dist[*i].is_finite()).min_by(|a, b| dist[*a].total_cmp(&dist[*b])) else { break };
                done[x] = true;
                if x == c.b {
                    break;
                }
                for (j, b) in self.bonds.iter().enumerate() {
                    if j == k || (b.a != x && b.b != x) {
                        continue;
                    }
                    let y = if b.a == x { b.b } else { b.a };
                    let w = if b.closure { 1.001 } else { 1.0 };
                    if dist[x] + w < dist[y] {
                        dist[y] = dist[x] + w;
                        prev[y] = x;
                    }
                }
            }
            if !dist[c.b].is_finite() {
                continue;
            }
            let mut ring = vec![c.b];
            let mut x = c.b;
            while x != c.a {
                x = prev[x];
                ring.push(x);
            }
            out.push(ring);
        }
        out
    }
}

// ---------- 2D layout ----------
/// Atom positions, in bond lengths.
fn layout(m: &Mol) -> Vec<(f32, f32)> {
    let n = m.atoms.len();
    if n == 1 {
        return vec![(0.0, 0.0)];
    }
    let nb = m.neighbors();
    let rings = m.rings();
    // graph distances
    let mut g = vec![vec![usize::MAX; n]; n];
    for s in 0..n {
        g[s][s] = 0;
        let mut q = VecDeque::from([s]);
        while let Some(x) = q.pop_front() {
            for &y in &nb[x] {
                if g[s][y] == usize::MAX {
                    g[s][y] = g[s][x] + 1;
                    q.push_back(y);
                }
            }
        }
    }
    // target distances: ring chords within a ring, zigzag lengths elsewhere
    let mut d = vec![vec![0.0f32; n]; n];
    for i in 0..n {
        for j in 0..n {
            if i == j {
                continue;
            }
            let l = g[i][j];
            let mut t = if l == usize::MAX {
                // separate fragments: side by side
                (n as f32).sqrt() * 2.0
            } else if l % 2 == 0 {
                l as f32 * 0.866
            } else {
                ((l as f32 * 0.866).powi(2) + 0.25).sqrt()
            };
            if l == 1 {
                t = 1.0;
            }
            let mut best: Option<usize> = None;
            for r in &rings {
                if let (Some(pi), Some(pj)) = (r.iter().position(|x| *x == i), r.iter().position(|x| *x == j)) {
                    if best.map(|b| r.len() < b).unwrap_or(true) {
                        best = Some(r.len());
                        let k = pi.abs_diff(pj).min(r.len() - pi.abs_diff(pj)) as f32;
                        let rn = r.len() as f32;
                        t = (std::f32::consts::PI * k / rn).sin() / (std::f32::consts::PI / rn).sin();
                    }
                }
            }
            d[i][j] = t;
        }
    }
    // start: classical MDS (the top two eigenvectors of the double-centered squared distances)
    let mut b = vec![vec![0.0f64; n]; n];
    let sq: Vec<Vec<f64>> = d.iter().map(|r| r.iter().map(|x| (*x as f64).powi(2)).collect()).collect();
    let row: Vec<f64> = sq.iter().map(|r| r.iter().sum::<f64>() / n as f64).collect();
    let all: f64 = row.iter().sum::<f64>() / n as f64;
    for i in 0..n {
        for j in 0..n {
            b[i][j] = -0.5 * (sq[i][j] - row[i] - row[j] + all);
        }
    }
    let mut vecs: Vec<(f64, Vec<f64>)> = Vec::new();
    for k in 0..2 {
        let mut v: Vec<f64> = (0..n).map(|i| ((i * 7 + k * 13 + 1) % 11) as f64 - 5.0 + 0.1 * i as f64).collect();
        let mut lambda = 0.0;
        for _ in 0..300 {
            let mut w: Vec<f64> = (0..n).map(|i| (0..n).map(|j| b[i][j] * v[j]).sum()).collect();
            for (l, u) in &vecs {
                let dot: f64 = w.iter().zip(u).map(|(a, b)| a * b).sum();
                for (wi, ui) in w.iter_mut().zip(u) {
                    *wi -= dot * ui;
                }
                let _ = l;
            }
            let norm = w.iter().map(|x| x * x).sum::<f64>().sqrt();
            if norm < 1e-12 {
                break;
            }
            lambda = norm;
            v = w.iter().map(|x| x / norm).collect();
        }
        vecs.push((lambda, v));
    }
    let mut p: Vec<(f64, f64)> = (0..n).map(|i| (vecs[0].1[i] * vecs[0].0.sqrt(), vecs[1].1[i] * vecs[1].0.sqrt())).collect();
    // refine: stress majorization, node by node
    for _ in 0..400 {
        for i in 0..n {
            let (mut sx, mut sy, mut sw) = (0.0, 0.0, 0.0);
            for j in 0..n {
                if i == j {
                    continue;
                }
                let dij = d[i][j] as f64;
                let w = 1.0 / (dij * dij) * if g[i][j] == 1 { 4.0 } else { 1.0 };
                let (dx, dy) = (p[i].0 - p[j].0, p[i].1 - p[j].1);
                let dist = (dx * dx + dy * dy).sqrt().max(1e-6);
                sx += w * (p[j].0 + dij * dx / dist);
                sy += w * (p[j].1 + dij * dy / dist);
                sw += w;
            }
            p[i] = (sx / sw, sy / sw);
        }
    }
    // lay it along its long axis
    let (cx, cy) = (p.iter().map(|x| x.0).sum::<f64>() / n as f64, p.iter().map(|x| x.1).sum::<f64>() / n as f64);
    let (mut sxx, mut syy, mut sxy) = (0.0, 0.0, 0.0);
    for (x, y) in &p {
        sxx += (x - cx).powi(2);
        syy += (y - cy).powi(2);
        sxy += (x - cx) * (y - cy);
    }
    let ang = 0.5 * (2.0 * sxy).atan2(sxx - syy);
    let (s, c) = (-ang).sin_cos();
    p.iter().map(|(x, y)| (((x - cx) * c - (y - cy) * s) as f32, ((x - cx) * s + (y - cy) * c) as f32)).collect()
}

// ---------- drawing ----------
fn color(el: &str) -> &'static str {
    match el {
        "O" => "#e74c3c",
        "N" => "#3498db",
        "F" => "#27ae60",
        "Cl" => "#16a085",
        "Br" => "#d35400",
        "I" => "#8e44ad",
        "P" => "#d35400",
        "S" => "#f1c40f",
        "B" | "Si" => "#e67e22",
        _ => "#222222",
    }
}

fn esc(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
}

/// The molecule as SVG, fitted to w×h.
pub fn svg(m: &Mol, w: f32, h: f32, background: bool) -> String {
    let pos = layout(m);
    let rings = m.rings();
    let nb = m.neighbors();
    let labeled: Vec<bool> = (0..m.atoms.len()).map(|i| {
        let a = &m.atoms[i];
        a.el != "C" || a.charge != 0 || nb[i].is_empty() || (a.bracket && a.hcount.unwrap_or(0) > 0 && nb[i].len() <= 1 && false)
    }).collect();
    // fit
    let (minx, maxx) = pos.iter().fold((f32::MAX, f32::MIN), |(a, b), p| (a.min(p.0), b.max(p.0)));
    let (miny, maxy) = pos.iter().fold((f32::MAX, f32::MIN), |(a, b), p| (a.min(p.1), b.max(p.1)));
    let pad = 18.0;
    let span_x = (maxx - minx).max(0.01);
    let span_y = (maxy - miny).max(0.01);
    let bl = (30.0 * (w / 240.0).max(1.0)).min((w - 2.0 * pad) / span_x).min((h - 2.0 * pad) / span_y).max(6.0);
    let ox = w / 2.0 - (minx + maxx) / 2.0 * bl;
    let oy = h / 2.0 - (miny + maxy) / 2.0 * bl;
    let at = |i: usize| (ox + pos[i].0 * bl, oy + pos[i].1 * bl);
    let lw = (bl * 0.045).max(1.1);
    let fs = (bl * 0.5).clamp(8.0, 16.0);
    let gap = fs * 0.62; // bonds stop short of a label
    let mut out = format!(r#"<svg xmlns="http://www.w3.org/2000/svg" width="{w}" height="{h}" viewBox="0 0 {w} {h}">"#);
    if background {
        out += &format!(r##"<rect width="{w}" height="{h}" fill="#ffffff"/>"##);
    }
    let line = |out: &mut String, (x1, y1): (f32, f32), (x2, y2): (f32, f32), c1: &str, c2: &str| {
        if c1 == c2 {
            *out += &format!(r#"<line x1="{x1:.2}" y1="{y1:.2}" x2="{x2:.2}" y2="{y2:.2}" stroke="{c1}" stroke-width="{lw:.2}" stroke-linecap="round"/>"#);
        } else {
            let (mx, my) = ((x1 + x2) / 2.0, (y1 + y2) / 2.0);
            *out += &format!(r#"<line x1="{x1:.2}" y1="{y1:.2}" x2="{mx:.2}" y2="{my:.2}" stroke="{c1}" stroke-width="{lw:.2}" stroke-linecap="round"/>"#);
            *out += &format!(r#"<line x1="{mx:.2}" y1="{my:.2}" x2="{x2:.2}" y2="{y2:.2}" stroke="{c2}" stroke-width="{lw:.2}" stroke-linecap="round"/>"#);
        }
    };
    for bnd in &m.bonds {
        let (mut p1, mut p2) = (at(bnd.a), at(bnd.b));
        let (dx, dy) = (p2.0 - p1.0, p2.1 - p1.1);
        let len = (dx * dx + dy * dy).sqrt().max(0.01);
        let (ux, uy) = (dx / len, dy / len);
        if labeled[bnd.a] {
            p1 = (p1.0 + ux * gap, p1.1 + uy * gap);
        }
        if labeled[bnd.b] {
            p2 = (p2.0 - ux * gap, p2.1 - uy * gap);
        }
        let (c1, c2) = (color(&m.atoms[bnd.a].el), color(&m.atoms[bnd.b].el));
        let (nx, ny) = (-uy, ux);
        let off = bl * 0.16;
        // a ring the bond is in: its second line goes inside
        let ring = rings.iter().filter(|r| r.contains(&bnd.a) && r.contains(&bnd.b)).min_by_key(|r| r.len());
        match bnd.order {
            2 | 4 if ring.is_some() => {
                line(&mut out, p1, p2, c1, c2);
                if bnd.order == 2 {
                    let r = ring.unwrap();
                    let (rcx, rcy) = r.iter().fold((0.0, 0.0), |(x, y), i| (x + at(*i).0, y + at(*i).1));
                    let (rcx, rcy) = (rcx / r.len() as f32, rcy / r.len() as f32);
                    let side = if (rcx - p1.0) * nx + (rcy - p1.1) * ny > 0.0 { 1.0 } else { -1.0 };
                    let sh = 0.15 * len;
                    let q1 = (p1.0 + nx * off * side + ux * sh, p1.1 + ny * off * side + uy * sh);
                    let q2 = (p2.0 + nx * off * side - ux * sh, p2.1 + ny * off * side - uy * sh);
                    line(&mut out, q1, q2, c1, c2);
                }
            }
            2 | 4 => {
                let h = off / 2.0;
                line(&mut out, (p1.0 + nx * h, p1.1 + ny * h), (p2.0 + nx * h, p2.1 + ny * h), c1, c2);
                line(&mut out, (p1.0 - nx * h, p1.1 - ny * h), (p2.0 - nx * h, p2.1 - ny * h), c1, c2);
            }
            3 => {
                line(&mut out, p1, p2, c1, c2);
                line(&mut out, (p1.0 + nx * off, p1.1 + ny * off), (p2.0 + nx * off, p2.1 + ny * off), c1, c2);
                line(&mut out, (p1.0 - nx * off, p1.1 - ny * off), (p2.0 - nx * off, p2.1 - ny * off), c1, c2);
            }
            _ => line(&mut out, p1, p2, c1, c2),
        }
    }
    // aromatic rings (lowercase SMILES): a circle inside
    for r in &rings {
        if r.iter().all(|i| m.atoms[*i].aromatic) && m.bonds.iter().filter(|b| r.contains(&b.a) && r.contains(&b.b)).all(|b| b.order == 4) {
            let (rcx, rcy) = r.iter().fold((0.0, 0.0), |(x, y), i| (x + at(*i).0, y + at(*i).1));
            let (rcx, rcy) = (rcx / r.len() as f32, rcy / r.len() as f32);
            let rad = bl / (2.0 * (std::f32::consts::PI / r.len() as f32).tan()) * 0.62;
            out += &format!(r##"<circle cx="{rcx:.2}" cy="{rcy:.2}" r="{rad:.2}" fill="none" stroke="#222222" stroke-width="{lw:.2}"/>"##);
        }
    }
    // labels
    for (i, a) in m.atoms.iter().enumerate() {
        if !labeled[i] {
            continue;
        }
        let (x, y) = at(i);
        let hs = m.hydrogens(i);
        let col = color(&a.el);
        // hydrogens go on the side away from the bonds
        let left = nb[i].iter().map(|j| at(*j).0 - x).sum::<f32>() > 0.1;
        let hpart = match hs {
            0 => String::new(),
            1 => "H".into(),
            n => format!(r#"H<tspan baseline-shift="sub" font-size="{:.1}">{n}</tspan>"#, fs * 0.7),
        };
        let charge = match a.charge {
            0 => String::new(),
            1 => "+".into(),
            -1 => "−".into(),
            c if c > 0 => format!("{c}+"),
            c => format!("{}−", -c),
        };
        let charge = if charge.is_empty() { charge } else { format!(r#"<tspan baseline-shift="super" font-size="{:.1}">{}</tspan>"#, fs * 0.7, esc(&charge)) };
        let el = esc(&a.el);
        let (text, anchor, ax) = if hs > 0 && left {
            (format!("{hpart}{el}{charge}"), "end", x + fs * 0.38 * a.el.len() as f32)
        } else {
            (format!("{el}{hpart}{charge}"), "start", x - fs * 0.36 * a.el.len() as f32)
        };
        out += &format!(r#"<text x="{ax:.2}" y="{:.2}" font-family="Inter" font-size="{fs:.1}" font-weight="600" fill="{col}" text-anchor="{anchor}">{text}</text>"#, y + fs * 0.36);
    }
    out += "</svg>";
    out
}

/// Inter for the labels, and the family name it goes by.
static FONTS: Lazy<(Arc<resvg::usvg::fontdb::Database>, String)> = Lazy::new(|| {
    let mut db = resvg::usvg::fontdb::Database::new();
    db.load_font_data(crate::theme::INTER_400.to_vec());
    db.load_font_data(crate::theme::INTER_600.to_vec());
    let family = db.faces().next().and_then(|f| f.families.first().map(|x| x.0.clone())).unwrap_or_else(|| "Inter".into());
    (Arc::new(db), family)
});

/// The drawing as RGBA pixels, `scale` pixels per unit.
fn raster(smi: &str, w: f32, h: f32, scale: f32, background: bool) -> Result<resvg::tiny_skia::Pixmap, String> {
    let m = parse(smi)?;
    let (db, family) = &*FONTS;
    let s = svg(&m, w, h, background).replace("font-family=\"Inter\"", &format!("font-family=\"{family}\""));
    let opt = resvg::usvg::Options { fontdb: db.clone(), font_family: family.clone(), ..Default::default() };
    let tree = resvg::usvg::Tree::from_str(&s, &opt).map_err(|e| e.to_string())?;
    let (pw, ph) = ((w * scale).ceil() as u32, (h * scale).ceil() as u32);
    let mut pm = resvg::tiny_skia::Pixmap::new(pw.max(1), ph.max(1)).ok_or("size")?;
    resvg::render(&tree, resvg::tiny_skia::Transform::from_scale(scale, scale), &mut pm.as_mut());
    Ok(pm)
}

/// A PNG for Anki (on white, so it reads in Anki's dark mode too).
pub fn png(smi: &str, w: u32, h: u32) -> Option<Vec<u8>> {
    raster(smi, w as f32, h as f32, 1.0, true).ok()?.encode_png().ok()
}

/// canvas.mol: the molecule on white in a bordered box, or the SMILES in red if it can't be read.
pub fn show(app: &mut App, ui: &mut Ui, smi: &str, r: Rect) {
    let tk = t();
    let ppp = ui.ctx().pixels_per_point();
    let key = format!("mol:{smi}:{}x{}@{ppp}", r.width(), r.height());
    let tex = match app.mem_image(&key) {
        Some(t) => Some(t),
        None if app.image_failed(&crate::images::Src::Mem(key.clone())) => None,
        None => match raster(smi, r.width(), r.height(), ppp, false) {
            Ok(pm) => {
                let img = egui::ColorImage::from_rgba_premultiplied([pm.width() as usize, pm.height() as usize], pm.data());
                Some(app.put_image(ui.ctx(), &key, img))
            }
            Err(_) => {
                app.mark_failed(&key);
                None
            }
        },
    };
    let p = ui.painter();
    match tex {
        Some((id, _)) => {
            p.rect_filled(r, cr(6.0), Color32::WHITE);
            p.image(id, r, Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)), Color32::WHITE);
            p.rect_stroke(r, cr(6.0), Stroke::new(1.0, tk.line), StrokeKind::Inside);
        }
        None => {
            // .mol-err
            let g = crate::widgets::lay(ui, smi, Ts::new(12.0, 400, tk.bad).mono(), Some(r.width()), false);
            p.galley(r.min, g, tk.bad);
        }
    }
    let _ = ui.interact(r, egui::Id::new(("mol", smi)), Sense::hover());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_and_lays_out() {
        let m = parse("CC(=O)OC1=CC=CC=C1C(=O)O").unwrap();
        assert_eq!(m.atoms.len(), 13);
        assert_eq!(m.rings().len(), 1);
        assert_eq!(m.rings()[0].len(), 6);
        let p = layout(&m);
        // bonded atoms about one bond length apart
        for b in &m.bonds {
            let d = ((p[b.a].0 - p[b.b].0).powi(2) + (p[b.a].1 - p[b.b].1).powi(2)).sqrt();
            assert!((d - 1.0).abs() < 0.2, "bond {b:?} is {d}");
        }
        assert_eq!(m.hydrogens(12), 1); // the acid's OH
        let n = parse("c1ccncc1").unwrap();
        assert_eq!(n.hydrogens(3), 0);
        let nap = parse("c1ccc2ccccc2c1").unwrap();
        let mut sets: Vec<Vec<usize>> = nap.rings().into_iter().map(|mut r| { r.sort(); r }).collect();
        sets.sort();
        assert_eq!(sets, vec![vec![0, 1, 2, 3, 8, 9], vec![3, 4, 5, 6, 7, 8]]);
        let q = parse("[NH4+].[Cl-]").unwrap();
        assert_eq!(q.atoms[0].charge, 1);
        assert!(parse("C1CC").is_err());
        assert!(png("CC(=O)OC1=CC=CC=C1C(=O)O", 480, 340).map(|b| b.len() > 1000).unwrap_or(false));
    }
}
