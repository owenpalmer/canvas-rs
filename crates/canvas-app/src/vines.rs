//! Edge-hugging vines: space colonization (Runions et al. 2005) with attractors confined to a band
//! along the inside edges of a rectangle, framing the sidebar and the main view.
//!
//! Pipeline: sample attractors in the edge band → grow a node tree toward them → thicken branches
//! from tips to roots (pipe model) → extract branches as smooth polylines → draw tapered stems,
//! leaves, and tendrils with growth timing. The growth runs off the UI thread; the drawing is
//! incremental: growth only ever adds ink, so each frame strokes just the segments that grew (into
//! a stems texture) and redraws only the leaves still unfurling (into a leaves texture), uploading
//! just the changed region.
//!
//! The simulation is the web version's, number for number (same PRNG, same order of draws), so the
//! same settings grow the same vines.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;

use egui::{Color32, ColorImage, Id, Rect, TextureHandle, TextureOptions, Ui, pos2, vec2};
use serde_json::{Value, json};
use tiny_skia::{FillRule, LineCap, Paint, PathBuilder, Pixmap, Stroke, Transform};

use crate::app::App;
use crate::theme::t;
use crate::widgets::{self as w, Ts};

// --- options -------------------------------------------------------------------------------------------
#[derive(Clone, Debug)]
pub struct Opts {
    pub width: f64,
    pub height: f64,
    pub seed: f64,
    pub band: f64,
    pub margin: f64,
    pub coverage: f64,
    pub corner_boost: f64,
    pub seed_corner_dist: f64,
    pub edges: [f64; 4],
    pub attract_dist: f64,
    pub kill_dist: f64,
    pub step: f64,
    pub wander: f64,
    pub max_nodes: usize,
    pub leaf_every: f64,
    pub leaf_size: f64,
    pub tendrils: f64,
    pub duration: f64,
    pub attractors: usize,
    pub nroots: usize,
}

pub const PALETTE: [u32; 5] = [0x7f9a83, 0x93ab94, 0x6b8570, 0xa7b9a3, 0x5f7a67];
pub const STEM: u32 = 0x7a7f69;

/// Vines.DEFAULTS for the settings the UI exposes.
pub fn defaults() -> Vec<(&'static str, f64)> {
    vec![
        ("seed", 96.0),
        ("band", 23.0),
        ("coverage", 0.9),
        ("cornerBoost", 8.9),
        ("seedCornerDist", 0.0),
        ("density", 37.0),
        ("roots", 2.5),
        ("attractDist", 60.0),
        ("killDist", 9.0),
        ("step", 4.0),
        ("wander", 1.0),
        ("leafEvery", 4.5),
        ("leafSize", 17.5),
        ("tendrils", 0.35),
        ("duration", 6.0),
    ]
}

// --- utilities -----------------------------------------------------------------------------------------
/// mulberry32, as JavaScript computes it (32-bit wrapping integer math).
#[derive(Clone)]
pub struct Rand {
    pub a: i32,
}

fn to_int32(x: f64) -> i32 {
    if !x.is_finite() {
        return 0;
    }
    let t = x.trunc();
    (t.rem_euclid(4294967296.0) as u64 as u32) as i32
}

impl Rand {
    pub fn new(seed: f64) -> Rand {
        Rand { a: to_int32(seed) }
    }
    pub fn next(&mut self) -> f64 {
        self.a = self.a.wrapping_add(0x6d2b79f5u32 as i32);
        let a = self.a;
        let mut t = (a ^ ((a as u32) >> 15) as i32).wrapping_mul(1 | a);
        t = (t.wrapping_add((t ^ ((t as u32) >> 7) as i32).wrapping_mul(61 | t))) ^ t;
        ((t ^ ((t as u32) >> 14) as i32) as u32) as f64 / 4294967296.0
    }
}

/// Smooth periodic 1D value noise over [0, period).
struct Noise {
    v: Vec<f64>,
    period: f64,
}

impl Noise {
    fn new(rand: &mut Rand, period: f64, cells: usize) -> Noise {
        Noise { v: (0..cells).map(|_| rand.next()).collect(), period }
    }
    fn at(&self, s: f64) -> f64 {
        let cells = self.v.len();
        let x = (((s / self.period) % 1.0) + 1.0) % 1.0 * cells as f64;
        let i = x.floor() as usize;
        let f = x - i as f64;
        let t = f * f * (3.0 - 2.0 * f);
        self.v[i % cells] * (1.0 - t) + self.v[(i + 1) % cells] * t
    }
}

/// Uniform spatial hash for nearest-node queries.
struct Grid {
    cell: f64,
    map: HashMap<i32, Vec<usize>>,
}

impl Grid {
    fn key(&self, x: f64, y: f64) -> i32 {
        ((x / self.cell).floor() as i32).wrapping_shl(16) ^ ((y / self.cell).floor() as i32)
    }
    fn add(&mut self, id: usize, x: f64, y: f64) {
        let k = self.key(x, y);
        self.map.entry(k).or_default().push(id);
    }
    fn near(&self, x: f64, y: f64, r: f64, out: &mut Vec<usize>) {
        out.clear();
        let c = self.cell;
        let (x0, x1) = (((x - r) / c).floor() as i32, ((x + r) / c).floor() as i32);
        let (y0, y1) = (((y - r) / c).floor() as i32, ((y + r) / c).floor() as i32);
        for i in x0..=x1 {
            for j in y0..=y1 {
                if let Some(b) = self.map.get(&(i.wrapping_shl(16) ^ j)) {
                    out.extend_from_slice(b);
                }
            }
        }
    }
}

/// Map a perimeter coordinate s ∈ [0, 2W+2H) and inward offset d to a point.
fn perimeter_point(s: f64, d: f64, w: f64, h: f64) -> (f64, f64) {
    if s < w {
        return (s, d);
    }
    let s = s - w;
    if s < h {
        return (w - d, s);
    }
    let s = s - h;
    if s < w {
        return (w - s, h - d);
    }
    let s = s - w;
    (d, h - s)
}

// --- growth ----------------------------------------------------------------------------------------------
struct Attractor {
    x: f64,
    y: f64,
    s: f64,
    alive: bool,
}

#[derive(Clone)]
pub struct Node {
    pub x: f64,
    pub y: f64,
    pub parent: Option<usize>,
    pub children: Vec<usize>,
    pub t: f64,
    pub thick: f64,
}

fn sample_attractors(o: &Opts, rand: &mut Rand) -> (Vec<Attractor>, f64) {
    let (w, h, band, margin) = (o.width, o.height, o.band, o.margin);
    let p = 2.0 * (w + h);
    let noise = Noise::new(rand, p, 6usize.max((p / 180.0).round() as usize));
    // Threshold chosen so ~coverage of the perimeter is "on".
    let thresh = 1.0 - o.coverage;
    let corners = [0.0, w, w + h, 2.0 * w + h];
    let mut pts = Vec::new();
    let mut tries = 0usize;
    while pts.len() < o.attractors && {
        let go = tries < o.attractors * 40;
        tries += 1;
        go
    } {
        let s = rand.next() * p;
        let edge = if s < w { 0 } else if s < w + h { 1 } else if s < 2.0 * w + h { 2 } else { 3 };
        if o.edges[edge] == 0.0 {
            continue;
        }
        let cd = corners.iter().map(|c| (s - c).abs().min(p - (s - c).abs())).fold(f64::INFINITY, f64::min);
        let corner_w = 1.0 + o.corner_boost * (-cd / (band * 2.5)).exp();
        let density = ((noise.at(s) - thresh) / (1e-3f64).max(1.0 - thresh)).max(0.0) * corner_w;
        if rand.next() * (1.0 + o.corner_boost) > density {
            continue;
        }
        // Denser near the edge, thinning out toward the inner side of the band; wider at corners.
        let depth = band * (0.55 + 0.45 * (corner_w - 1.0).min(1.0));
        let d = margin + rand.next().powf(1.6) * depth;
        let (x, y) = perimeter_point(s, d, w, h);
        if x < margin || y < margin || x > w - margin || y > h - margin {
            continue;
        }
        pts.push(Attractor { x, y, s, alive: true });
    }
    (pts, p)
}

const SEED_ANYWHERE: f64 = 400.0;

fn pick_roots(o: &Opts, rand: &mut Rand, att: &[Attractor], p: f64) -> Vec<(f64, f64)> {
    let (w, h, m) = (o.width, o.height, o.margin);
    let mut roots: Vec<(f64, f64, f64)> = Vec::new();
    if o.seed_corner_dist >= SEED_ANYWHERE {
        // Anywhere: random attractors, spread apart along the perimeter.
        let mut cand: Vec<(f64, f64)> = att.iter().map(|a| (rand.next() - 0.5, a.s)).collect();
        cand.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
        for (_, s) in cand {
            if roots.len() >= o.nroots {
                break;
            }
            if roots.iter().any(|r| (r.2 - s).abs().min(p - (r.2 - s).abs()) < p / (o.nroots as f64 * 2.0)) {
                continue;
            }
            let (x, y) = perimeter_point(s, m, w, h);
            roots.push((x, y, s));
        }
        return roots.into_iter().map(|r| (r.0, r.1)).collect();
    }
    // Near corners: corner i has edge i running forward from it and edge i-1 running backward.
    let c = [0.0, w, w + h, 2.0 * w + h];
    let len = [w, h, w, h];
    let mut sides: Vec<(usize, f64, f64)> = Vec::new();
    for i in 0..4 {
        if o.edges[i] != 0.0 {
            sides.push((i, 1.0, len[i]));
        }
        if o.edges[(i + 3) % 4] != 0.0 {
            sides.push((i, -1.0, len[(i + 3) % 4]));
        }
    }
    if sides.is_empty() {
        return Vec::new();
    }
    for i in (1..sides.len()).rev() {
        let j = (rand.next() * (i + 1) as f64).floor() as usize;
        sides.swap(i, j);
    }
    for k in 0..o.nroots {
        let (i, dir, edge_len) = sides[k % sides.len()];
        let off = (rand.next() * o.seed_corner_dist).min(edge_len / 2.0);
        let sp = (((c[i] + dir * off) % p) + p) % p;
        let (x, y) = perimeter_point(sp, m, w, h);
        roots.push(((w - m).min(m.max(x)), (h - m).min(m.max(y)), sp));
    }
    roots.into_iter().map(|r| (r.0, r.1)).collect()
}

fn edge_info(x: f64, y: f64, w: f64, h: f64) -> (f64, f64, f64) {
    let d = [x, w - x, y, h - y];
    let mut i = 0;
    for k in 1..4 {
        if d[k] < d[i] {
            i = k;
        }
    }
    let n = [(1.0, 0.0), (-1.0, 0.0), (0.0, 1.0), (0.0, -1.0)][i];
    (d[i], n.0, n.1)
}

pub fn grow(o: &Opts, rand: &mut Rand) -> Vec<Node> {
    let (w, h) = (o.width, o.height);
    let (mut att, p) = sample_attractors(o, rand);
    let mut nodes: Vec<Node> = Vec::new();
    let mut grid = Grid { cell: o.attract_dist, map: HashMap::new() };
    let add = |nodes: &mut Vec<Node>, grid: &mut Grid, x: f64, y: f64, parent: Option<usize>, t: f64| {
        let id = nodes.len();
        if let Some(pi) = parent {
            nodes[pi].children.push(id);
        }
        nodes.push(Node { x, y, parent, children: Vec::new(), t, thick: 0.0 });
        grid.add(id, x, y);
    };
    for (x, y) in pick_roots(o, rand, &att, p) {
        add(&mut nodes, &mut grid, x, y, None, 0.0);
    }
    if nodes.is_empty() {
        return nodes;
    }
    let mut t = 0.0;
    let mut near = Vec::new();
    for _ in 0..2000 {
        if nodes.len() >= o.max_nodes {
            break;
        }
        t += 1.0;
        // 1. each live attractor pulls on its nearest node within attractDist
        let mut order: Vec<usize> = Vec::new();
        let mut pull: HashMap<usize, (f64, f64, f64)> = HashMap::new();
        for a in att.iter().filter(|a| a.alive) {
            let mut best: Option<usize> = None;
            let mut bd = o.attract_dist * o.attract_dist;
            grid.near(a.x, a.y, o.attract_dist, &mut near);
            for &n in &near {
                let (dx, dy) = (a.x - nodes[n].x, a.y - nodes[n].y);
                let d2 = dx * dx + dy * dy;
                if d2 < bd {
                    bd = d2;
                    best = Some(n);
                }
            }
            let Some(b) = best else { continue };
            let d = { let s = bd.sqrt(); if s == 0.0 { 1.0 } else { s } };
            let e = pull.entry(b).or_insert_with(|| {
                order.push(b);
                (0.0, 0.0, 0.0)
            });
            e.0 += (a.x - nodes[b].x) / d;
            e.1 += (a.y - nodes[b].y) / d;
            e.2 += 1.0;
        }
        if pull.is_empty() {
            break;
        }
        // 2. grow a new node from each pulled node
        let mut grew = 0;
        for n in order {
            let (px, py, k) = pull[&n];
            let (mut dx, mut dy) = (px / k, py / k);
            // keep some momentum from the parent so stems curve instead of zig-zag
            if let Some(par) = nodes[n].parent {
                let (ppx, ppy) = (nodes[n].x - nodes[par].x, nodes[n].y - nodes[par].y);
                let pl = { let l = (ppx * ppx + ppy * ppy).sqrt(); if l == 0.0 { 1.0 } else { l } };
                dx += (ppx / pl) * 0.6;
                dy += (ppy / pl) * 0.6;
            }
            let ang = dy.atan2(dx) + (rand.next() - 0.5) * 2.0 * o.wander;
            let (mut nx, mut ny) = (nodes[n].x + ang.cos() * o.step, nodes[n].y + ang.sin() * o.step);
            // stay inside the band: push off the wall, pull back if drifting inward
            let (dist, enx, eny) = edge_info(nx, ny, w, h);
            if dist < o.margin {
                nx += enx * (o.margin - dist);
                ny += eny * (o.margin - dist);
            }
            let max_in = o.margin + o.band * 1.2;
            if dist > max_in {
                nx -= enx * (dist - max_in);
                ny -= eny * (dist - max_in);
            }
            // skip if it would land on an existing node (stalled growth)
            grid.near(nx, ny, o.step * 0.5, &mut near);
            if near.iter().any(|&m| ((nodes[m].x - nx).powi(2) + (nodes[m].y - ny).powi(2)).sqrt() < o.step * 0.5) {
                continue;
            }
            add(&mut nodes, &mut grid, nx, ny, Some(n), t);
            grew += 1;
        }
        // 3. remove attractors that have been reached
        for a in att.iter_mut().filter(|a| a.alive) {
            grid.near(a.x, a.y, o.kill_dist, &mut near);
            if near.iter().any(|&n| ((a.x - nodes[n].x).powi(2) + (a.y - nodes[n].y).powi(2)).sqrt() < o.kill_dist) {
                a.alive = false;
            }
        }
        if grew == 0 {
            break;
        }
    }
    // pipe-model thickening: leaves are 1, parents accumulate children
    for i in (0..nodes.len()).rev() {
        if nodes[i].children.is_empty() {
            nodes[i].thick = 1.0;
        }
        if let Some(p) = nodes[i].parent {
            let th = nodes[i].thick;
            nodes[p].thick += th;
        }
    }
    nodes
}

/// Split the node tree into branches: each runs from a root or a fork down the heaviest child
/// until a tip; lighter children start new branches.
fn branches(nodes: &[Node]) -> Vec<Vec<usize>> {
    let mut out = Vec::new();
    fn sorted_kids(nodes: &[Node], n: usize) -> Vec<usize> {
        let mut kids = nodes[n].children.clone();
        kids.sort_by(|a, b| nodes[*b].thick.partial_cmp(&nodes[*a].thick).unwrap_or(std::cmp::Ordering::Equal));
        kids
    }
    fn walk2(nodes: &[Node], from: usize, k: usize, out: &mut Vec<Vec<usize>>) {
        let mut line = vec![from, k];
        let mut n = k;
        while !nodes[n].children.is_empty() {
            let kids = sorted_kids(nodes, n);
            for &c in &kids[1..] {
                walk2(nodes, n, c, out);
            }
            n = kids[0];
            line.push(n);
        }
        out.push(line);
    }
    for r in (0..nodes.len()).filter(|&i| nodes[i].parent.is_none()) {
        let mut line = vec![r];
        let mut n = r;
        while !nodes[n].children.is_empty() {
            let kids = sorted_kids(nodes, n);
            for &c in &kids[1..] {
                walk2(nodes, n, c, &mut out);
            }
            n = kids[0];
            line.push(n);
        }
        out.push(line);
    }
    out
}

#[derive(Clone, Copy)]
struct P {
    x: f64,
    y: f64,
    t: f64,
    w: f64,
}

/// Chaikin corner-cutting to smooth a polyline (keeps endpoints and timing).
fn smooth(mut pts: Vec<P>, passes: usize) -> Vec<P> {
    for _ in 0..passes {
        if pts.len() < 3 {
            return pts;
        }
        let mut o = vec![pts[0]];
        for i in 0..pts.len() - 1 {
            let (a, b) = (pts[i], pts[i + 1]);
            o.push(P { x: a.x * 0.75 + b.x * 0.25, y: a.y * 0.75 + b.y * 0.25, t: a.t * 0.75 + b.t * 0.25, w: a.w * 0.75 + b.w * 0.25 });
            o.push(P { x: a.x * 0.25 + b.x * 0.75, y: a.y * 0.25 + b.y * 0.75, t: a.t * 0.25 + b.t * 0.75, w: a.w * 0.25 + b.w * 0.75 });
        }
        o.push(*pts.last().unwrap());
        pts = o;
    }
    pts
}

#[derive(Clone)]
pub struct Seg {
    pub x0: f32,
    pub y0: f32,
    pub x1: f32,
    pub y1: f32,
    pub w: f32,
    pub t: f64,
}

#[derive(Clone)]
pub struct Leaf {
    pub x: f32,
    pub y: f32,
    pub ang: f32,
    pub t: f64,
    pub color: u32,
    pub alpha: f32,
    pub s: f32,
    pub curl: f32,
}

pub struct Scene {
    pub segs: Vec<Seg>,
    pub leaves: Vec<Leaf>,
}

fn f1(v: f64) -> f32 {
    ((v * 10.0).round() / 10.0) as f32
}

/// Tendril: a spiral whose curvature increases toward the end.
fn tendril_pts(mut x: f64, mut y: f64, mut ang: f64, len: f64, dir: f64) -> Vec<(f64, f64)> {
    let mut pts = Vec::new();
    let mut k = 0.07;
    let mut d = 0.0;
    while d < len {
        pts.push((x, y));
        ang += dir * k;
        k *= 1.06;
        x += ang.cos() * 1.2;
        y += ang.sin() * 1.2;
        d += 1.2;
    }
    pts
}

pub fn build_scene(o: &Opts, rand: &mut Rand, nodes: &[Node]) -> Scene {
    let t_max = nodes.iter().map(|n| n.t).fold(1.0, f64::max);
    let sec = |t: f64| (t / t_max) * o.duration;
    let max_thick = nodes.iter().map(|n| n.thick).fold(1.0, f64::max);
    let width_of = |n: &Node| 0.6 + 2.6 * (n.thick / max_thick).powf(0.45);
    let mut segs = Vec::new();
    let mut leaves = Vec::new();
    for b in branches(nodes) {
        let pts = smooth(b.iter().map(|&i| P { x: nodes[i].x, y: nodes[i].y, t: nodes[i].t, w: width_of(&nodes[i]) }).collect(), 2);
        for i in 0..pts.len().saturating_sub(1) {
            let (p, q) = (pts[i], pts[i + 1]);
            segs.push(Seg { x0: p.x as f32, y0: p.y as f32, x1: q.x as f32, y1: q.y as f32, w: ((p.w + q.w) / 2.0) as f32, t: sec(q.t) });
        }
        // Leaves: alternate sides along the branch, smaller toward the tip.
        let mut side = if rand.next() < 0.5 { 1.0 } else { -1.0 };
        let len = b.len();
        for i in 2..len.saturating_sub(1) {
            if rand.next() > 1.0 / o.leaf_every {
                continue;
            }
            let (n, a) = (&nodes[b[i]], &nodes[b[i - 1]]);
            let heading = (n.y - a.y).atan2(n.x - a.x);
            let ang = heading + side * (0.6 + rand.next() * 0.5);
            side = -side;
            let frac = i as f64 / len as f64;
            let s = o.leaf_size * (0.55 + 0.6 * rand.next()) * (1.0 - 0.45 * frac) * (0.7 + 0.3 * (n.thick / 4.0).min(1.0));
            let color = PALETTE[(rand.next() * PALETTE.len() as f64).floor() as usize];
            let alpha = 0.55 + rand.next() * 0.35;
            let curl = (rand.next() - 0.5) * 0.4;
            leaves.push(Leaf { x: n.x as f32, y: n.y as f32, ang: ang as f32, t: sec(n.t) + 0.15, color, alpha: alpha as f32, s: s as f32, curl: curl as f32 });
        }
        // Tendril at some tips: drawn like stem segments, spread over 0.8s.
        let tip = &nodes[b[len - 1]];
        let prev = &nodes[b[len.saturating_sub(3)]];
        if len > 6 && rand.next() < o.tendrils {
            let heading = (tip.y - prev.y).atan2(tip.x - prev.x);
            let l = 10.0 + rand.next() * 14.0;
            let dir = if rand.next() < 0.5 { 1.0 } else { -1.0 };
            let tp = tendril_pts(tip.x, tip.y, heading, l, dir);
            let t0 = sec(tip.t) + 0.1;
            for j in 0..tp.len().saturating_sub(1) {
                segs.push(Seg { x0: tp[j].0 as f32, y0: tp[j].1 as f32, x1: tp[j + 1].0 as f32, y1: tp[j + 1].1 as f32, w: 0.7, t: t0 + (0.8 * (j + 1) as f64) / tp.len() as f64 });
            }
        }
    }
    segs.sort_by(|p, q| p.t.partial_cmp(&q.t).unwrap_or(std::cmp::Ordering::Equal));
    leaves.sort_by(|p, q| p.t.partial_cmp(&q.t).unwrap_or(std::cmp::Ordering::Equal));
    Scene { segs, leaves }
}

pub fn options(cfg: &HashMap<String, f64>, width: f64, height: f64, edges: [f64; 4], seed: f64) -> Opts {
    let g = |k: &str, d: f64| cfg.get(k).copied().unwrap_or(d);
    let p = 2.0 * (width + height);
    let density = g("density", 37.0);
    let roots = g("roots", 2.5);
    Opts {
        width,
        height,
        seed,
        band: g("band", 23.0).min(width.min(height) * 0.25),
        margin: 6.0,
        coverage: g("coverage", 0.9),
        corner_boost: g("cornerBoost", 8.9),
        seed_corner_dist: g("seedCornerDist", 0.0),
        edges,
        attract_dist: g("attractDist", 60.0),
        kill_dist: g("killDist", 9.0),
        step: g("step", 4.0),
        wander: g("wander", 1.0),
        max_nodes: 6000,
        leaf_every: g("leafEvery", 4.5),
        leaf_size: g("leafSize", 17.5),
        tendrils: g("tendrils", 0.35),
        duration: g("duration", 6.0),
        // Counts scale with the perimeter so every box gets the same density.
        attractors: ((density * p) / 100.0).round() as usize,
        nroots: 1usize.max(((roots * p) / 1000.0).round() as usize),
    }
}

/// The whole simulation: nodes, then the scene (continuing the same random stream).
pub fn simulate(o: &Opts) -> Scene {
    let mut rand = Rand::new(o.seed * 9973.0 + 17.0);
    let nodes = grow(o, &mut rand);
    build_scene(o, &mut rand, &nodes)
}

// --- drawing --------------------------------------------------------------------------------------------
fn paint_of(rgb: u32, a: f32) -> Paint<'static> {
    let mut p = Paint::default();
    p.set_color_rgba8((rgb >> 16) as u8, (rgb >> 8) as u8, rgb as u8, (a.clamp(0.0, 1.0) * 255.0).round() as u8);
    p.anti_alias = true;
    p
}

/// Almond leaf pointing along +x, length s, with a slight asymmetric curl.
fn leaf_path(s: f32, curl: f32) -> Option<tiny_skia::Path> {
    let (s, curl) = (s as f64, curl as f64);
    let w = s * 0.42;
    let mut pb = PathBuilder::new();
    pb.move_to(0.0, 0.0);
    pb.cubic_to(f1(s * 0.25), f1(-w), f1(s * 0.75), f1(-w * (0.9 + curl)), f1(s), f1(curl * s * 0.2));
    pb.cubic_to(f1(s * 0.7), f1(w * (0.8 - curl)), f1(s * 0.3), f1(w), 0.0, 0.0);
    pb.close();
    pb.finish()
}

fn rib_path(s: f32) -> Option<tiny_skia::Path> {
    let s = s as f64;
    let mut pb = PathBuilder::new();
    pb.move_to(f1(s * 0.08), 0.0);
    pb.quad_to(f1(s * 0.5), f1(s * 0.03), f1(s * 0.85), 0.0);
    pb.finish()
}

/// Draw a leaf at scale k (0.2..1 while it unfurls). Returns its bounding box in pixels.
fn draw_leaf(pm: &mut Pixmap, l: &Leaf, k: f32, dpr: f32) -> Option<[i32; 4]> {
    let tf = Transform::from_scale(dpr, dpr).pre_translate(l.x, l.y).pre_rotate(l.ang.to_degrees()).pre_scale(k, k);
    let body = leaf_path(l.s, l.curl)?;
    let alpha = l.alpha * (k * 1.4).min(1.0);
    pm.fill_path(&body, &paint_of(l.color, alpha), FillRule::Winding, tf, None);
    if let Some(rib) = rib_path(l.s) {
        let st = Stroke { width: 0.4, ..Default::default() };
        pm.stroke_path(&rib, &paint_of(STEM, alpha * 0.5), &st, tf, None);
    }
    let r = (l.s * 1.1 * k + 2.0) * dpr;
    let (cx, cy) = (l.x * dpr, l.y * dpr);
    Some([(cx - r) as i32, (cy - r) as i32, (cx + r) as i32 + 1, (cy + r) as i32 + 1])
}

fn union(a: Option<[i32; 4]>, b: [i32; 4]) -> Option<[i32; 4]> {
    Some(match a {
        None => b,
        Some(a) => [a[0].min(b[0]), a[1].min(b[1]), a[2].max(b[2]), a[3].max(b[3])],
    })
}

fn stroke_segs(pm: &mut Pixmap, segs: &[Seg], dpr: f32) -> Option<[i32; 4]> {
    // batched by width (quantized to .25) into as few strokes as possible
    let mut dirty = None;
    let mut i = 0;
    while i < segs.len() {
        let w = (segs[i].w * 4.0).round() / 4.0;
        let mut pb = PathBuilder::new();
        while i < segs.len() && (segs[i].w * 4.0).round() / 4.0 == w {
            let g = &segs[i];
            pb.move_to(g.x0, g.y0);
            pb.line_to(g.x1, g.y1);
            let pad = w + 1.0;
            dirty = union(dirty, [((g.x0.min(g.x1) - pad) * dpr) as i32, ((g.y0.min(g.y1) - pad) * dpr) as i32, ((g.x0.max(g.x1) + pad) * dpr) as i32 + 1, ((g.y0.max(g.y1) + pad) * dpr) as i32 + 1]);
            i += 1;
        }
        if let Some(path) = pb.finish() {
            let st = Stroke { width: w, line_cap: LineCap::Round, ..Default::default() };
            pm.stroke_path(&path, &paint_of(STEM, 1.0), &st, Transform::from_scale(dpr, dpr), None);
        }
    }
    dirty
}

const LEAF_GROW: f64 = 0.55;

/// One region's vines: the scene and its two textures, growing or grown.
pub struct Layer {
    pub size: (f32, f32),
    dpr: f32,
    scene: Arc<Scene>,
    stems: Pixmap,
    still: Pixmap,
    leaves: Pixmap,
    stems_tex: TextureHandle,
    leaves_tex: TextureHandle,
    start: Instant,
    animate: bool,
    si: usize,
    li: usize,
    growing: Vec<usize>,
    prev_active: Option<[i32; 4]>,
    pub done: bool,
}

fn to_image(pm: &Pixmap, r: [i32; 4]) -> Option<(ColorImage, [usize; 2])> {
    let (w, h) = (pm.width() as i32, pm.height() as i32);
    let x0 = r[0].clamp(0, w);
    let y0 = r[1].clamp(0, h);
    let x1 = r[2].clamp(0, w);
    let y1 = r[3].clamp(0, h);
    if x1 <= x0 || y1 <= y0 {
        return None;
    }
    let (rw, rh) = ((x1 - x0) as usize, (y1 - y0) as usize);
    let mut px = Vec::with_capacity(rw * rh * 4);
    let data = pm.data();
    for y in y0..y1 {
        let s = ((y * w + x0) * 4) as usize;
        px.extend_from_slice(&data[s..s + rw * 4]);
    }
    Some((ColorImage::from_rgba_premultiplied([rw, rh], &px), [x0 as usize, y0 as usize]))
}

impl Layer {
    fn new(ctx: &egui::Context, key: &str, scene: Arc<Scene>, size: (f32, f32), dpr: f32, animate: bool) -> Option<Layer> {
        let (pw, ph) = (((size.0 * dpr).round() as u32).max(1), ((size.1 * dpr).round() as u32).max(1));
        let stems = Pixmap::new(pw, ph)?;
        let still = Pixmap::new(pw, ph)?;
        let leaves = Pixmap::new(pw, ph)?;
        let blank = ColorImage::new([pw as usize, ph as usize], vec![Color32::TRANSPARENT; (pw * ph) as usize]);
        let stems_tex = ctx.load_texture(format!("vines-stems-{key}"), blank.clone(), TextureOptions::LINEAR);
        let leaves_tex = ctx.load_texture(format!("vines-leaves-{key}"), blank, TextureOptions::LINEAR);
        let mut l = Layer { size, dpr, scene, stems, still, leaves, stems_tex, leaves_tex, start: Instant::now(), animate, si: 0, li: 0, growing: Vec::new(), prev_active: None, done: false };
        if !animate {
            l.finish_now();
        }
        Some(l)
    }

    /// Everything drawn at once (no animation).
    fn finish_now(&mut self) {
        let dpr = self.dpr;
        self.stems.fill(tiny_skia::Color::TRANSPARENT);
        self.still.fill(tiny_skia::Color::TRANSPARENT);
        stroke_segs(&mut self.stems, &self.scene.segs, dpr);
        let scene = self.scene.clone();
        for l in &scene.leaves {
            draw_leaf(&mut self.still, l, 1.0, dpr);
        }
        self.leaves = self.still.clone();
        self.si = scene.segs.len();
        self.li = scene.leaves.len();
        self.growing.clear();
        self.done = true;
        let full = [0, 0, self.stems.width() as i32, self.stems.height() as i32];
        if let Some((img, _)) = to_image(&self.stems, full) {
            self.stems_tex.set(img, TextureOptions::LINEAR);
        }
        if let Some((img, _)) = to_image(&self.leaves, full) {
            self.leaves_tex.set(img, TextureOptions::LINEAR);
        }
    }

    pub fn replay(&mut self) {
        self.stems.fill(tiny_skia::Color::TRANSPARENT);
        self.still.fill(tiny_skia::Color::TRANSPARENT);
        self.leaves.fill(tiny_skia::Color::TRANSPARENT);
        let full = [0, 0, self.stems.width() as i32, self.stems.height() as i32];
        if let Some((img, _)) = to_image(&self.stems, full) {
            self.stems_tex.set(img.clone(), TextureOptions::LINEAR);
            self.leaves_tex.set(img, TextureOptions::LINEAR);
        }
        self.si = 0;
        self.li = 0;
        self.growing.clear();
        self.prev_active = None;
        self.start = Instant::now();
        self.animate = true;
        self.done = false;
    }

    /// Advance the growth to now; upload what changed.
    fn step(&mut self) {
        if self.done {
            return;
        }
        let tnow = self.start.elapsed().as_secs_f64();
        let dpr = self.dpr;
        let scene = self.scene.clone();
        // new stem segments
        let from = self.si;
        while self.si < scene.segs.len() && scene.segs[self.si].t <= tnow {
            self.si += 1;
        }
        if self.si > from {
            if let Some(d) = stroke_segs(&mut self.stems, &scene.segs[from..self.si], dpr) {
                if let Some((img, pos)) = to_image(&self.stems, d) {
                    self.stems_tex.set_partial(pos, img, TextureOptions::LINEAR);
                }
            }
        }
        while self.li < scene.leaves.len() && scene.leaves[self.li].t <= tnow {
            self.growing.push(self.li);
            self.li += 1;
        }
        // leaves that finished unfurling are stamped onto the still layer
        let mut dirty = self.prev_active;
        let mut active: Option<[i32; 4]> = None;
        let mut still_changed = false;
        self.growing.retain(|&i| {
            let l = &scene.leaves[i];
            let p = (tnow - l.t) / LEAF_GROW;
            if p >= 1.0 {
                if let Some(b) = draw_leaf(&mut self.still, l, 1.0, dpr) {
                    dirty = union(dirty, b);
                }
                still_changed = true;
                false
            } else {
                true
            }
        });
        let _ = still_changed;
        for &i in &self.growing {
            let l = &scene.leaves[i];
            let r = (l.s * 1.1 + 2.0) * dpr;
            let (cx, cy) = (l.x * dpr, l.y * dpr);
            let b = [(cx - r) as i32, (cy - r) as i32, (cx + r) as i32 + 1, (cy + r) as i32 + 1];
            active = union(active, b);
            dirty = union(dirty, b);
        }
        if let Some(d) = dirty {
            // leaves = still, plus the leaves unfurling, in the changed region
            let (w, h) = (self.leaves.width() as i32, self.leaves.height() as i32);
            let x0 = d[0].clamp(0, w);
            let y0 = d[1].clamp(0, h);
            let x1 = d[2].clamp(0, w);
            let y1 = d[3].clamp(0, h);
            if x1 > x0 && y1 > y0 {
                let (sd, ld) = (self.still.data(), self.leaves.data_mut());
                for y in y0..y1 {
                    let s = ((y * w + x0) * 4) as usize;
                    let e = ((y * w + x1) * 4) as usize;
                    ld[s..e].copy_from_slice(&sd[s..e]);
                }
                let mask_rect = tiny_skia::Rect::from_xywh(x0 as f32, y0 as f32, (x1 - x0) as f32, (y1 - y0) as f32);
                let mask = mask_rect.and_then(|r| {
                    let mut m = tiny_skia::Mask::new(w as u32, h as u32)?;
                    m.fill_path(&PathBuilder::from_rect(r), FillRule::Winding, false, Transform::identity());
                    Some(m)
                });
                for &i in &self.growing {
                    let l = &scene.leaves[i];
                    let p = ((tnow - l.t) / LEAF_GROW).clamp(0.0, 1.0);
                    let k = 0.2 + 0.8 * (1.0 - (1.0 - p).powi(3));
                    let tf = Transform::from_scale(dpr, dpr).pre_translate(l.x, l.y).pre_rotate(l.ang.to_degrees()).pre_scale(k as f32, k as f32);
                    if let Some(body) = leaf_path(l.s, l.curl) {
                        let alpha = l.alpha * (k as f32 * 1.4).min(1.0);
                        self.leaves.fill_path(&body, &paint_of(l.color, alpha), FillRule::Winding, tf, mask.as_ref());
                        if let Some(rib) = rib_path(l.s) {
                            self.leaves.stroke_path(&rib, &paint_of(STEM, alpha * 0.5), &Stroke { width: 0.4, ..Default::default() }, tf, mask.as_ref());
                        }
                    }
                }
                if let Some((img, pos)) = to_image(&self.leaves, [x0, y0, x1, y1]) {
                    self.leaves_tex.set_partial(pos, img, TextureOptions::LINEAR);
                }
            }
        }
        self.prev_active = active;
        if self.si >= scene.segs.len() && self.li >= scene.leaves.len() && self.growing.is_empty() {
            self.done = true;
        }
    }
}

// --- the app's vines -----------------------------------------------------------------------------------
pub struct Vines {
    pub cfg: HashMap<String, f64>,
    pub flags: HashMap<String, bool>,
    pub edges: HashMap<String, [f64; 4]>,
    pub layers: HashMap<String, Layer>,
    pending: HashMap<String, (u64, (f32, f32))>,
    generation: u64,
    started: bool,
    seen: std::collections::HashSet<String>,
    changed_at: Option<Instant>,
    resize_at: HashMap<String, (Instant, (f32, f32))>,
}

fn hash_str(s: &str) -> u32 {
    let mut x: u32 = 2166136261;
    for c in s.encode_utf16() {
        x = (x ^ c as u32).wrapping_mul(16777619);
    }
    x
}

impl Vines {
    pub fn load(prefs: &crate::prefs::Prefs) -> Vines {
        let mut v = Vines::defaults();
        if let Some(saved) = prefs.get("vines").and_then(|v| v.as_object()) {
            for (k, x) in saved {
                match x {
                    Value::Bool(b) => {
                        v.flags.insert(k.clone(), *b);
                    }
                    Value::Number(n) => {
                        v.cfg.insert(k.clone(), n.as_f64().unwrap_or(0.0));
                    }
                    Value::Array(a) if k.ends_with("Edges") => {
                        let mut e = [0.0; 4];
                        for (i, x) in a.iter().take(4).enumerate() {
                            e[i] = x.as_f64().or_else(|| x.as_bool().map(|b| b as i32 as f64)).unwrap_or(0.0);
                        }
                        v.edges.insert(k.trim_end_matches("Edges").to_string(), e);
                    }
                    _ => {}
                }
            }
        }
        v
    }

    fn defaults() -> Vines {
        let mut cfg: HashMap<String, f64> = defaults().into_iter().map(|(k, v)| (k.to_string(), v)).collect();
        cfg.insert("opacity".into(), 1.0);
        let flags = [("enabled", true), ("main", true), ("sidebar", true), ("animate", true)].into_iter().map(|(k, v)| (k.to_string(), v)).collect();
        let edges = [("sidebar".to_string(), [1.0, 1.0, 1.0, 0.0]), ("main".to_string(), [1.0, 1.0, 1.0, 1.0])].into_iter().collect();
        Vines { cfg, flags, edges, layers: HashMap::new(), pending: HashMap::new(), generation: 0, started: false, seen: Default::default(), changed_at: None, resize_at: HashMap::new() }
    }

    fn flag(&self, k: &str) -> bool {
        self.flags.get(k).copied().unwrap_or(true)
    }

    pub fn to_json(&self) -> Value {
        let mut m = serde_json::Map::new();
        for k in ["enabled", "main", "sidebar", "animate"] {
            m.insert(k.into(), json!(self.flag(k)));
        }
        m.insert("opacity".into(), json!(self.cfg.get("opacity").copied().unwrap_or(1.0)));
        for (r, e) in &self.edges {
            m.insert(format!("{r}Edges"), json!(e.iter().map(|x| *x as i64).collect::<Vec<_>>()));
        }
        for (k, _) in defaults() {
            m.insert(k.into(), json!(self.cfg.get(k).copied().unwrap_or(0.0)));
        }
        Value::Object(m)
    }
}

fn save(app: &mut App) {
    let v = app.vines.to_json();
    app.set_pref("vines", v);
}

/// Regrow everything (without animating) once the settings settle.
fn changed(app: &mut App) {
    save(app);
    app.vines.changed_at = Some(Instant::now());
}

fn grow_layer(app: &mut App, ctx: &egui::Context, key: &str, size: (f32, f32), animate: bool) {
    app.vines.generation += 1;
    let generation = app.vines.generation;
    app.vines.pending.insert(key.to_string(), (generation, size));
    let edges = app.vines.edges.get(key).copied().unwrap_or([1.0; 4]);
    let seed = (hash_str(key) % 100000) as f64 + app.vines.cfg.get("seed").copied().unwrap_or(96.0) * 7919.0;
    let o = options(&app.vines.cfg, size.0 as f64, size.1 as f64, edges, seed);
    let dpr = ctx.pixels_per_point().min(2.0);
    let (ctx2, key2) = (ctx.clone(), key.to_string());
    app.spawn(
        async move {
            tokio::task::spawn_blocking(move || {
                let scene = Arc::new(simulate(&o));
                Layer::new(&ctx2, &key2, scene, size, dpr, animate)
            })
            .await
            .ok()
            .flatten()
        },
        {
            let key = key.to_string();
            move |app, layer| {
                if app.vines.pending.get(&key).map(|p| p.0) != Some(generation) {
                    return; // superseded
                }
                app.vines.pending.remove(&key);
                match layer {
                    Some(mut l) => {
                        l.start = Instant::now();
                        app.vines.layers.insert(key, l);
                    }
                    None => {
                        app.vines.layers.remove(&key);
                    }
                }
            }
        },
    );
}

/// Paint the vines behind the sidebar and main (called before their content), growing them the
/// first time (after the launch intro starts) and regrowing when a region changes size.
pub fn paint(app: &mut App, ui: &mut Ui, sidebar: Option<Rect>, main: Option<Rect>, fade: f32) {
    let ctx = ui.ctx().clone();
    if app.intro.is_none() {
        return;
    }
    app.vines.started = true;
    let reduced = app.settings.reduced_motion;
    let enabled = app.vines.flag("enabled");
    // settle settings changes (150ms)
    if let Some(t) = app.vines.changed_at {
        if t.elapsed().as_millis() >= 150 {
            app.vines.changed_at = None;
            app.vines.layers.clear();
            app.vines.pending.clear();
        } else {
            ctx.request_repaint_after(std::time::Duration::from_millis(160));
        }
    }
    let opacity = app.vines.cfg.get("opacity").copied().unwrap_or(1.0) as f32 * fade;
    let leaf_a = t().leaf_alpha;
    for (key, rect) in [("sidebar", sidebar), ("main", main)] {
        let Some(rect) = rect else { continue };
        let region_w = if key == "sidebar" { rect.width() - 1.0 } else { rect.width() };
        let size = (region_w.round(), rect.height().round());
        if !enabled || !app.vines.flag(key) || size.0 < 120.0 || size.1 < 120.0 {
            app.vines.layers.remove(key);
            continue;
        }
        let have = app.vines.layers.get(key).map(|l| l.size);
        let pending = app.vines.pending.get(key).map(|p| p.1);
        if have.is_none() && pending.is_none() {
            let animate = app.vines.flag("animate") && !reduced && !app.vines.seen.contains(key);
            app.vines.seen.insert(key.to_string());
            grow_layer(app, &ctx, key, size, animate);
        } else if have != Some(size) && pending != Some(size) {
            // regrow (without animating) once the size settles for 200ms
            let entry = app.vines.resize_at.entry(key.to_string()).or_insert((Instant::now(), size));
            if entry.1 != size {
                *entry = (Instant::now(), size);
            }
            if entry.0.elapsed().as_millis() >= 200 {
                app.vines.resize_at.remove(key);
                grow_layer(app, &ctx, key, size, false);
            } else {
                ctx.request_repaint_after(std::time::Duration::from_millis(210));
            }
        }
        if let Some(l) = app.vines.layers.get_mut(key) {
            l.step();
            if !l.done {
                ctx.request_repaint();
            }
            // pinned to the visible area (top-left of the region)
            let r = Rect::from_min_size(rect.min, vec2(l.size.0, l.size.1));
            let uv = Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0));
            let p = ui.painter();
            p.image(l.stems_tex.id(), r, uv, Color32::from_white_alpha((opacity * 255.0) as u8));
            p.image(l.leaves_tex.id(), r, uv, Color32::from_white_alpha((opacity * leaf_a * 255.0) as u8));
        }
    }
}

pub fn replay(app: &mut App) {
    for l in app.vines.layers.values_mut() {
        l.replay();
    }
}

// --- the settings -----------------------------------------------------------------------------------------
/// [key, label, min, max, step, unit, advanced]
const CONTROLS: &[(&str, &str, f64, f64, f64, &str, bool)] = &[
    ("band", "Band width", 10.0, 90.0, 1.0, "px", false),
    ("coverage", "Coverage", 0.1, 1.0, 0.05, "", false),
    ("cornerBoost", "Corner emphasis", 0.0, 15.0, 0.1, "", false),
    ("seedCornerDist", "Seed distance from corner", 0.0, 400.0, 5.0, "px", false),
    ("density", "Density", 5.0, 100.0, 1.0, "", false),
    ("roots", "Vines per 1000px", 0.3, 5.0, 0.1, "", false),
    ("wander", "Wander", 0.0, 1.0, 0.05, "", false),
    ("leafEvery", "Leaf spacing", 1.0, 10.0, 0.5, "", false),
    ("leafSize", "Leaf size", 4.0, 24.0, 0.5, "px", false),
    ("tendrils", "Tendrils", 0.0, 1.0, 0.05, "", false),
    ("duration", "Growth time", 1.0, 20.0, 0.5, "s", false),
    ("opacity", "Opacity", 0.2, 1.0, 0.05, "", false),
    ("seed", "Seed", 1.0, 999.0, 1.0, "", true),
    ("attractDist", "Attraction distance", 15.0, 150.0, 1.0, "px", true),
    ("killDist", "Kill distance", 3.0, 30.0, 1.0, "px", true),
    ("step", "Segment length", 2.0, 10.0, 0.5, "px", true),
];

fn shown(k: &str, v: f64, unit: &str) -> String {
    if k == "seedCornerDist" && v >= 400.0 {
        return "anywhere".into();
    }
    format!("{}{unit}", crate::fmt::num_f(v))
}

fn edges_chips(app: &mut App, ui: &mut Ui, region: &str) {
    ui.horizontal_wrapped(|ui| {
        ui.add_space(24.0);
        ui.spacing_mut().item_spacing = vec2(4.0, 4.0);
        for (i, name) in ["Top", "Right", "Bottom", "Left"].iter().enumerate() {
            let on = app.vines.edges.get(region).map(|e| e[i] != 0.0).unwrap_or(true);
            if w::chip_sized(ui, name, on, 11.0, vec2(8.0, 1.0)).clicked() {
                let e = app.vines.edges.entry(region.to_string()).or_insert([1.0; 4]);
                e[i] = if on { 0.0 } else { 1.0 };
                changed(app);
            }
        }
    });
    ui.add_space(6.0);
}

pub fn settings(app: &mut App, ui: &mut Ui) {
    let tk = t();
    w::h2(ui, "Vines");
    w::text_block(ui, "Vines grow along the inside edges of the sidebar and the main view, using a space-colonization simulation. The same settings always grow the same vines, and changes show up on the vines around this page as you make them.", Ts::muted(14.0));
    ui.add_space(16.0);
    let wdt = ui.available_width().min(420.0);
    let mut c = ui.new_child(egui::UiBuilder::new().max_rect(Rect::from_min_size(ui.cursor().min, vec2(wdt, f32::INFINITY))).layout(egui::Layout::top_down(egui::Align::Min)));
    w::boxed(&mut c, egui::Margin { left: 16, right: 16, top: 12, bottom: 12 }, 8.0, |ui| {
        for (k, label, hint) in [("enabled", "Show vines", None), ("sidebar", "In the sidebar", None)] {
            if crate::settings::toggle(ui, Id::new(("vine", k)), app.vines.flag(k), label, hint) {
                let v = !app.vines.flag(k);
                app.vines.flags.insert(k.into(), v);
                changed(app);
            }
        }
        edges_chips(app, ui, "sidebar");
        if crate::settings::toggle(ui, Id::new(("vine", "main")), app.vines.flag("main"), "Around the main view", None) {
            let v = !app.vines.flag("main");
            app.vines.flags.insert("main".into(), v);
            changed(app);
        }
        edges_chips(app, ui, "main");
        if crate::settings::toggle(ui, Id::new(("vine", "animate")), app.vines.flag("animate"), "Animate growth", Some("When the app opens")) {
            let v = !app.vines.flag("animate");
            app.vines.flags.insert("animate".into(), v);
            changed(app);
        }
        // .set-sep
        ui.add_space(8.0);
        let y = ui.cursor().min.y;
        ui.painter().hline(ui.cursor().min.x..=ui.cursor().min.x + ui.available_width(), y + 0.5, egui::Stroke::new(1.0, tk.line));
        ui.add_space(5.0);
        let control = |app: &mut App, ui: &mut Ui, (k, label, min, max, step, unit, _): &(&str, &str, f64, f64, f64, &str, bool)| {
            let mut v = app.vines.cfg.get(*k).copied().unwrap_or(0.0);
            let label_v = shown(k, v, unit);
            if crate::settings::slider(ui, Id::new(("vine-slider", *k)), &mut v, *min, *max, *step, label, &label_v) {
                app.vines.cfg.insert(k.to_string(), v);
                changed(app);
            }
        };
        for c in CONTROLS.iter().filter(|c| !c.6) {
            control(app, ui, c);
        }
        // <details> Advanced
        ui.add_space(6.0);
        let open = app.settings.adv_open;
        let resp = w::text_button(ui, &format!("{} Advanced", if open { "▾" } else { "▸" }), Ts::muted(13.0), Ts::new(13.0, 400, tk.text));
        if resp.clicked() {
            app.settings.adv_open = !open;
        }
        if open {
            for c in CONTROLS.iter().filter(|c| c.6) {
                control(app, ui, c);
            }
        }
        ui.add_space(12.0);
        w::hwrap(ui, vec2(8.0, 8.0), |ui| {
            if w::button(ui, "Replay growth").clicked() {
                replay(app);
            }
            if w::button(ui, "New seed").clicked() {
                let s = 1.0 + (rand::random::<u32>() % 999) as f64;
                app.vines.cfg.insert("seed".into(), s);
                changed(app);
            }
            if w::button(ui, "Reset to defaults").clicked() {
                let layers = std::mem::take(&mut app.vines.layers);
                app.vines = Vines::defaults();
                app.vines.layers = layers;
                app.vines.started = true;
                changed(app);
            }
        });
    });
    let h = c.min_rect().height();
    ui.allocate_space(vec2(wdt, h));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prng_matches_js() {
        // mulberry32(1) in JavaScript: 0.6270739405881613, 0.002735721180215478, 0.5274470399599522
        let mut r = Rand::new(1.0);
        assert!((r.next() - 0.6270739405881613).abs() < 1e-15);
        assert!((r.next() - 0.002735721180215478).abs() < 1e-15);
        assert!((r.next() - 0.5274470399599522).abs() < 1e-15);
        assert_eq!(to_int32(4294967296.0 + 5.0), 5);
        assert_eq!(hash_str("main"), {
            let mut x: u32 = 2166136261;
            for c in "main".bytes() {
                x = (x ^ c as u32).wrapping_mul(16777619);
            }
            x
        });
    }

    /// Node counts, attractor counts and the sum of node x from the web version's vines.js.
    #[test]
    fn same_vines_as_the_web_version() {
        let cfg: HashMap<String, f64> = defaults().into_iter().map(|(k, v)| (k.to_string(), v)).collect();
        for (w, h, seed, edges, nodes, atts, x_sum) in [
            (800.0, 420.0, 96.0, [1.0, 1.0, 1.0, 1.0], 841usize, 903usize, 279587.456737),
            (247.0, 860.0, (3363491783u32 % 100000) as f64 + 96.0 * 7919.0, [1.0, 1.0, 1.0, 0.0], 599, 819, 111076.238309),
            (1000.0, 860.0, (3935363592u32 % 100000) as f64 + 96.0 * 7919.0, [1.0, 1.0, 1.0, 1.0], 949, 1376, 457104.361920),
        ] {
            let o = options(&cfg, w, h, edges, seed);
            let mut rand = Rand::new(o.seed * 9973.0 + 17.0);
            let (att, _) = sample_attractors(&o, &mut Rand::new(o.seed * 9973.0 + 17.0));
            assert_eq!(att.len(), atts);
            let n = grow(&o, &mut rand);
            assert_eq!(n.len(), nodes);
            let sum: f64 = n.iter().map(|n| n.x).sum();
            assert!((sum - x_sum).abs() < 1e-3, "{sum} vs {x_sum}");
        }
        assert_eq!(hash_str("main"), 3935363592);
        assert_eq!(hash_str("sidebar"), 3363491783);
    }

    #[test]
    fn grows_something() {
        let cfg: HashMap<String, f64> = defaults().into_iter().map(|(k, v)| (k.to_string(), v)).collect();
        let o = options(&cfg, 800.0, 420.0, [1.0; 4], 96.0);
        let s = simulate(&o);
        assert!(s.segs.len() > 200, "{}", s.segs.len());
        assert!(!s.leaves.is_empty());
        // deterministic
        let s2 = simulate(&o);
        assert_eq!(s.segs.len(), s2.segs.len());
    }
}
