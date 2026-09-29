//! Pictures: images in course pages (through Canvas, with your session), avatars, file previews,
//! and Anki's media. Each is fetched and decoded in the background, then kept as a texture.

use std::collections::HashMap;

use egui::{ColorImage, Context, TextureHandle, TextureOptions, Vec2};

use crate::app::App;

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Src {
    /// A Canvas URL, fetched with your session and cached on disk.
    Proxy(String),
    /// A Canvas file by id.
    File(String),
    /// A file in Anki's media folder.
    Anki(String),
    /// A picture drawn by the app (a molecule, a PDF crop).
    Mem(String),
    /// A public https image (not Canvas: no cookies are sent).
    Web(String),
    /// A data: URL.
    Data(String),
}

pub enum Img {
    Loading,
    Ready(TextureHandle),
    Failed,
}

#[derive(Default)]
pub struct Images {
    map: HashMap<Src, Img>,
}

/// Decode a picture (PNG, JPEG, GIF's first frame, WebP, BMP, or SVG).
pub fn decode(bytes: &[u8], max_side: u32) -> Option<ColorImage> {
    if let Ok(img) = image::load_from_memory(bytes) {
        let img = if img.width() > max_side || img.height() > max_side { img.resize(max_side, max_side, image::imageops::FilterType::Triangle) } else { img };
        let rgba = img.to_rgba8();
        return Some(ColorImage::from_rgba_unmultiplied([rgba.width() as usize, rgba.height() as usize], rgba.as_raw()));
    }
    let head = String::from_utf8_lossy(&bytes[..bytes.len().min(512)]).to_lowercase();
    if head.contains("<svg") || head.contains("<?xml") {
        let opt = resvg::usvg::Options::default();
        let tree = resvg::usvg::Tree::from_data(bytes, &opt).ok()?;
        let size = tree.size();
        let scale = (2.0f32).min(max_side as f32 / size.width().max(size.height()).max(1.0));
        let (w, h) = ((size.width() * scale).ceil() as u32, (size.height() * scale).ceil() as u32);
        let mut pm = resvg::tiny_skia::Pixmap::new(w.max(1), h.max(1))?;
        resvg::render(&tree, resvg::tiny_skia::Transform::from_scale(scale, scale), &mut pm.as_mut());
        return Some(ColorImage::from_rgba_premultiplied([w as usize, h as usize], pm.data()));
    }
    None
}

impl App {
    /// A picture's texture, starting its load the first time it's asked for.
    pub fn image(&mut self, ctx: &Context, src: &Src) -> Option<(egui::TextureId, Vec2)> {
        match self.images.map.get(src) {
            Some(Img::Ready(t)) => return Some((t.id(), t.size_vec2())),
            Some(_) => return None,
            None => {}
        }
        self.images.map.insert(src.clone(), Img::Loading);
        let svc = self.svc.clone();
        let s2 = src.clone();
        let ctx2 = ctx.clone();
        self.spawn(
            async move {
                let bytes: Option<Vec<u8>> = match &s2 {
                    Src::Proxy(u) => svc.proxy(u).await.ok().map(|(b, _)| b),
                    Src::File(fid) => match svc.file(fid).await {
                        Ok((path, _)) => tokio::fs::read(path).await.ok(),
                        Err(_) => None,
                    },
                    Src::Anki(name) => svc.anki_media(name).await.ok().flatten().map(|(b, _)| b),
                    Src::Mem(_) => None,
                    Src::Web(u) => match reqwest::Client::builder().timeout(std::time::Duration::from_secs(30)).build() {
                        Ok(c) => match c.get(u).send().await {
                            Ok(r) if r.status().is_success() => r.bytes().await.ok().map(|b| b.to_vec()),
                            _ => None,
                        },
                        Err(_) => None,
                    },
                    Src::Data(d) => d.split_once(";base64,").and_then(|(_, b)| {
                        use base64::Engine;
                        base64::engine::general_purpose::STANDARD.decode(b.trim()).ok()
                    }),
                };
                let img = match bytes {
                    Some(b) => tokio::task::spawn_blocking(move || decode(&b, 4096)).await.ok().flatten(),
                    None => None,
                };
                img.map(|i| ctx2.load_texture(format!("{s2:?}"), i, TextureOptions::LINEAR))
            },
            {
                let s3 = src.clone();
                move |app, tex| {
                    app.images.map.insert(s3, match tex {
                        Some(t) => Img::Ready(t),
                        None => Img::Failed,
                    });
                }
            },
        );
        None
    }

    pub fn image_failed(&self, src: &Src) -> bool {
        matches!(self.images.map.get(src), Some(Img::Failed))
    }

    /// Put a picture the app made into the cache.
    pub fn put_image(&mut self, ctx: &Context, key: &str, img: ColorImage) -> (egui::TextureId, Vec2) {
        let t = ctx.load_texture(key.to_string(), img, TextureOptions::LINEAR);
        let out = (t.id(), t.size_vec2());
        self.images.map.insert(Src::Mem(key.to_string()), Img::Ready(t));
        out
    }

    /// Remember that a picture the app makes can't be made.
    pub fn mark_failed(&mut self, key: &str) {
        self.images.map.insert(Src::Mem(key.to_string()), Img::Failed);
    }

    pub fn mem_image(&self, key: &str) -> Option<(egui::TextureId, Vec2)> {
        match self.images.map.get(&Src::Mem(key.to_string())) {
            Some(Img::Ready(t)) => Some((t.id(), t.size_vec2())),
            _ => None,
        }
    }
}
