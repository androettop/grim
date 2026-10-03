//! `Engine.Canvas`: what the game's menus, HUD and text draw through.
//!
//! The scripts set the canvas up (font, colour, position, clipping) and call the draw natives;
//! each call turns into a rectangle here, which the renderer draws over the finished scene.

use grim_assets::font::Font;
use grim_assets::Asset;
use grim_object::{ObjectKey, Value};

use crate::vm::{floats3, Vm, VmResult};

/// `ERenderStyle` values the canvas draws with.
pub const STY_NORMAL: u8 = 1;
pub const STY_MASKED: u8 = 2;
pub const STY_TRANSLUCENT: u8 = 3;
pub const STY_MODULATED: u8 = 4;

/// One rectangle of the game's own drawing, in screen pixels.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CanvasQuad {
    pub rect: [f32; 4],
    /// Texture rectangle in texels.
    pub uv: [f32; 4],
    pub texture: Option<ObjectKey>,
    pub color: [f32; 4],
    pub masked: bool,
    /// `Canvas.Style`, one of Unreal's draw styles.
    pub style: u8,
}

/// A font's glyphs, by character, with the page each one lives on.
#[derive(Debug, Clone, Default)]
pub struct FontMetrics {
    pub pages: Vec<Option<ObjectKey>>,
    /// Per character: page, and its rectangle in that page's texels.
    pub glyphs: Vec<(usize, [f32; 4])>,
}

impl FontMetrics {
    /// Fonts hold one entry per character code, split over pages.
    fn glyph(&self, c: char) -> Option<(usize, [f32; 4])> {
        let glyph = self.glyphs.get(c as usize).copied()?;
        (glyph.1[2] > 0.0).then_some(glyph)
    }

    pub fn size(&self, text: &str) -> [f32; 2] {
        let mut size = [0.0f32, 0.0f32];
        for c in text.chars() {
            if let Some((_, r)) = self.glyph(c) {
                size[0] += r[2];
                size[1] = size[1].max(r[3]);
            }
        }
        size
    }
}

impl Vm {
    /// Glyph rectangles of a font object, read from the package once.
    pub fn font_metrics(&mut self, font: ObjectKey) -> Option<&FontMetrics> {
        if !self.fonts.contains_key(&font) {
            let pkg = self.world.package(font.package).clone();
            let metrics = match grim_assets::load_export(&pkg, font.export).ok().map(|o| o.asset) {
                Some(Asset::Font(f)) => build_metrics(&pkg, &f, font.package),
                _ => FontMetrics::default(),
            };
            self.fonts.insert(font, metrics);
        }
        self.fonts.get(&font)
    }

    fn canvas_font(&mut self) -> Option<ObjectKey> {
        let canvas = self.canvas?;
        match self.get_prop(canvas, "Font") {
            Ok(Value::Object(f)) => f,
            _ => None,
        }
    }

    fn canvas_float(&mut self, name: &str) -> f32 {
        let Some(canvas) = self.canvas else { return 0.0 };
        match self.get_prop(canvas, name) {
            Ok(Value::Float(v)) => v,
            _ => 0.0,
        }
    }

    fn canvas_color(&mut self) -> [f32; 4] {
        let Some(canvas) = self.canvas else { return [1.0; 4] };
        // `DrawColor` is a Color struct of bytes; Unreal draws text and tiles modulated by it.
        match self.get_prop(canvas, "DrawColor") {
            Ok(Value::Struct(fields)) => {
                let byte = |i: usize| match fields.get(i) {
                    Some(Value::Byte(b)) => *b as f32 / 255.0,
                    _ => 1.0,
                };
                [byte(0), byte(1), byte(2), 1.0]
            }
            _ => [1.0; 4],
        }
    }

    /// Where the next drawing lands, with the canvas origin applied.
    fn canvas_pos(&mut self) -> [f32; 2] {
        [
            self.canvas_float("OrgX") + self.canvas_float("CurX"),
            self.canvas_float("OrgY") + self.canvas_float("CurY"),
        ]
    }

    /// The part of the screen the canvas lets a clipped draw reach: from its origin, as far as
    /// its clipping size goes.
    fn canvas_clip(&mut self) -> [f32; 4] {
        let (x, y) = (self.canvas_float("OrgX"), self.canvas_float("OrgY"));
        [x, y, x + self.canvas_float("ClipX"), y + self.canvas_float("ClipY")]
    }

    /// `DrawTileClipped`: a tile cut down to the canvas's clipping region, its texture cut
    /// with it so what is left keeps its scale. The game's windows are drawn this way: a list
    /// header stretched wider than its window is only seen as far as the window goes.
    pub fn canvas_draw_tile_clipped(&mut self, texture: Option<ObjectKey>, size: [f32; 2], uv: [f32; 4]) -> VmResult<()> {
        let at = self.canvas_pos();
        let clip = self.canvas_clip();
        let Some((rect, uv)) = clip_quad([at[0], at[1], size[0], size[1]], uv, clip) else { return Ok(()) };
        let (color, style) = (self.canvas_color(), self.canvas_style());
        self.canvas_quads.push(CanvasQuad { rect, uv, texture, color, masked: style == STY_MASKED, style });
        Ok(())
    }

    pub fn canvas_draw_tile(&mut self, texture: Option<ObjectKey>, size: [f32; 2], uv: [f32; 4]) -> VmResult<()> {
        let at = self.canvas_pos();
        let color = self.canvas_color();
        let style = self.canvas_style();
        if self.switches.trace_quads {
            let name = texture.map(|t| {
                let pkg = self.world.package(t.package).clone();
                pkg.object_name(grim_package::ObjectRef::Export(t.export)).to_string()
            });
            eprintln!("tile at {at:?} size {size:?} uv {uv:?} style {style} texture {name:?}");
        }
        self.canvas_quads.push(CanvasQuad {
            rect: [at[0], at[1], size[0], size[1]],
            uv,
            texture,
            color,
            // A normal tile covers what is under it; the other styles blend.
            masked: style == STY_MASKED,
            style,
        });
        Ok(())
    }

    /// `Canvas.Style`: how what is drawn goes over what is already there.
    fn canvas_style(&mut self) -> u8 {
        let Some(canvas) = self.canvas else { return STY_NORMAL };
        match self.get_prop(canvas, "Style") {
            Ok(Value::Byte(b)) if b > 0 => b,
            _ => STY_NORMAL,
        }
    }

    /// Lays out a string with the canvas font and draws every glyph. What does not fit in the
    /// canvas's clipping width wraps onto the next line, as Unreal's own text drawing does.
    /// `DrawTextClipped`: one line, not wrapped, cut at the canvas's clipping region.
    pub fn canvas_draw_text_clipped(&mut self, text: &str) -> VmResult<()> {
        let Some(font) = self.canvas_font() else { return Ok(()) };
        let Some(metrics) = self.font_metrics(font).cloned() else { return Ok(()) };
        let (color, style) = (self.canvas_color(), self.canvas_style());
        let at = self.canvas_pos();
        let clip = self.canvas_clip();
        let mut x = at[0];
        for c in text.chars() {
            let Some((page, r)) = metrics.glyph(c) else { continue };
            let texture = metrics.pages.get(page).copied().flatten();
            if let Some((rect, uv)) = clip_quad([x, at[1], r[2], r[3]], r, clip) {
                self.canvas_quads.push(CanvasQuad { rect, uv, texture, color, masked: true, style });
            }
            x += r[2];
        }
        let org = [self.canvas_float("OrgX"), self.canvas_float("OrgY")];
        let Some(canvas) = self.canvas else { return Ok(()) };
        self.set_prop(canvas, "CurX", Value::Float(x - org[0]))?;
        Ok(())
    }

    pub fn canvas_draw_text(&mut self, text: &str, newline: bool) -> VmResult<()> {
        let Some(font) = self.canvas_font() else { return Ok(()) };
        let Some(metrics) = self.font_metrics(font).cloned() else { return Ok(()) };
        let color = self.canvas_color();
        let style = self.canvas_style();
        let at = self.canvas_pos();
        let org = [self.canvas_float("OrgX"), self.canvas_float("OrgY")];
        let clip = self.canvas_float("ClipX");
        let center = matches!(self.canvas.map(|c| self.get_prop(c, "bCenter")), Some(Ok(Value::Bool(true))));
        // Centred text uses the whole width; text that starts somewhere else has what is left.
        let width = if center { clip } else { (clip - (at[0] - org[0])).max(1.0) };

        let mut y = at[1];
        let (mut last_x, mut line_height) = (at[0], 0.0f32);
        for line in wrap(&metrics, text, width) {
            let size = metrics.size(&line);
            let mut x = if center { (clip - size[0]) / 2.0 } else { at[0] };
            for c in line.chars() {
                let Some((page, r)) = metrics.glyph(c) else { continue };
                let texture = metrics.pages.get(page).copied().flatten();
                self.canvas_quads.push(CanvasQuad {
                    rect: [x, y, r[2], r[3]],
                    uv: r,
                    texture,
                    color,
                    // Font pages keep their background transparent.
                    masked: true,
                    style,
                });
                x += r[2];
            }
            line_height = size[1];
            (last_x, y) = (x, y + size[1]);
        }
        // The last line drawn leaves the pen where it ended, unless a new line was asked for.
        let Some(canvas) = self.canvas else { return Ok(()) };
        self.set_prop(canvas, "CurX", Value::Float(if newline { at[0] - org[0] } else { last_x - org[0] }))?;
        self.set_prop(canvas, "CurY", Value::Float(if newline { y - org[1] } else { y - line_height - org[1] }))?;
        self.set_prop(canvas, "CurYL", Value::Float(line_height))?;
        Ok(())
    }

    /// Size of a string in the canvas font.
    pub fn canvas_text_size(&mut self, text: &str) -> [f32; 2] {
        let Some(font) = self.canvas_font() else { return [0.0; 2] };
        self.font_metrics(font).map(|m| m.size(text)).unwrap_or([0.0; 2])
    }
}

/// A rectangle `[x, y, w, h]` with its texture rectangle `[u, v, ul, vl]`, cut to a region
/// `[x0, y0, x1, y1]`; nothing when none of it is inside.
fn clip_quad(rect: [f32; 4], uv: [f32; 4], clip: [f32; 4]) -> Option<([f32; 4], [f32; 4])> {
    let (x0, y0) = (rect[0].max(clip[0]), rect[1].max(clip[1]));
    let (x1, y1) = ((rect[0] + rect[2]).min(clip[2]), (rect[1] + rect[3]).min(clip[3]));
    if x1 <= x0 || y1 <= y0 || rect[2] <= 0.0 || rect[3] <= 0.0 {
        return None;
    }
    let (su, sv) = (uv[2] / rect[2], uv[3] / rect[3]);
    Some((
        [x0, y0, x1 - x0, y1 - y0],
        [uv[0] + (x0 - rect[0]) * su, uv[1] + (y0 - rect[1]) * sv, (x1 - x0) * su, (y1 - y0) * sv],
    ))
}

/// Breaks a string into the lines it needs so none is wider than `width`, splitting at spaces
/// and letting a single word that does not fit take a line of its own.
fn wrap(metrics: &FontMetrics, text: &str, width: f32) -> Vec<String> {
    let mut lines = Vec::new();
    for paragraph in text.split('\n') {
        let mut line = String::new();
        for word in paragraph.split(' ') {
            let candidate = if line.is_empty() { word.to_string() } else { format!("{line} {word}") };
            if !line.is_empty() && metrics.size(&candidate)[0] > width {
                lines.push(std::mem::take(&mut line));
                line = word.to_string();
            } else {
                line = candidate;
            }
        }
        lines.push(line);
    }
    lines
}

fn build_metrics(pkg: &std::sync::Arc<grim_package::Package>, font: &Font, package: grim_object::PkgId) -> FontMetrics {
    let mut metrics = FontMetrics::default();
    for page in &font.pages {
        let texture = match page.texture {
            grim_package::ObjectRef::Export(e) => Some(ObjectKey { package, export: e }),
            // A page in another package is resolved when it is drawn.
            _ => None,
        };
        let index = metrics.pages.len();
        metrics.pages.push(texture);
        for c in &page.characters {
            metrics.glyphs.push((index, [c.start_u as f32, c.start_v as f32, c.u_size as f32, c.v_size as f32]));
        }
    }
    let _ = pkg;
    metrics
}

/// Vectors the scripts pass as sizes, where only two components matter.
pub fn xy(v: &Value) -> VmResult<[f32; 2]> {
    let v = floats3(v)?;
    Ok([v[0], v[1]])
}

/// A picture the engine draws into on the processor: a `ScriptedTexture`'s own.
#[derive(Debug, Clone, Default)]
pub struct Picture {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
    /// Set when something was drawn into it since the renderer last took it.
    pub dirty: bool,
}

impl Picture {
    pub fn new(width: u32, height: u32) -> Self {
        Self { width, height, rgba: vec![0; (width * height * 4) as usize], dirty: true }
    }

    fn put(&mut self, x: i32, y: i32, colour: [u8; 4]) {
        if x < 0 || y < 0 || x as u32 >= self.width || y as u32 >= self.height {
            return;
        }
        let at = ((y as u32 * self.width + x as u32) * 4) as usize;
        self.rgba[at..at + 4].copy_from_slice(&colour);
    }
}

impl Vm {
    /// A texture's picture, decoded once. `None` for the ones the engine cannot decode.
    fn picture_of(&mut self, key: ObjectKey) -> Option<&grim_assets::texture::Rgba> {
        if !self.pictures.contains_key(&key) {
            let pkg = self.world.package(key.package).clone();
            let decoded = (|| {
                let loaded = grim_assets::load_export(&pkg, key.export).ok()?;
                let Asset::Texture(t) = &loaded.asset else { return None };
                let palette = match loaded.base.property(&pkg, "Palette", 0) {
                    Some(grim_package::property::PropertyValue::Object(r)) => {
                        let handle = self.world.lib.resolve(&pkg, *r).ok()?;
                        match grim_assets::load_export(&handle.package, handle.export).ok()?.asset {
                            Asset::Palette(p) => Some(p),
                            _ => None,
                        }
                    }
                    _ => None,
                };
                let mip = t.mips.first()?;
                grim_assets::texture::decode_mip(mip, t.format, palette.as_ref(), true).ok()
            })();
            self.pictures.insert(key, decoded);
        }
        self.pictures.get(&key).and_then(|p| p.as_ref())
    }

    /// The picture a `ScriptedTexture` draws into, made the size the texture says.
    fn scripted_picture(&mut self, texture: ObjectKey) -> Option<&mut Picture> {
        if !self.scripted.contains_key(&texture) {
            let size = |vm: &mut Self, name: &str| match vm.get_prop(texture, name) {
                Ok(Value::Int(v)) if v > 0 => Some(v as u32),
                _ => None,
            };
            let (w, h) = (size(self, "USize")?, size(self, "VSize")?);
            self.scripted.insert(texture, Picture::new(w, h));
        }
        self.scripted.get_mut(&texture)
    }

    /// `ScriptedTexture.DrawColoredText`: writes text into the texture's own picture, glyph by
    /// glyph out of the font's pages. The engine draws this one itself, so it is the only text
    /// in the game that is not handed to the renderer as rectangles.
    pub fn scripted_draw_text(&mut self, texture: ObjectKey, at: [f32; 2], text: &str, font: ObjectKey, colour: [u8; 4]) {
        let Some(metrics) = self.font_metrics(font).cloned() else { return };
        let mut x = at[0] as i32;
        let y = at[1] as i32;
        for c in text.chars() {
            let Some((page, rect)) = metrics.glyphs.get(c as usize).copied().filter(|g| g.1[2] > 0.0) else { continue };
            let Some(Some(page)) = metrics.pages.get(page).copied() else { continue };
            let Some(source) = self.picture_of(page) else { continue };
            let (sw, sh) = (source.width as i32, source.height as i32);
            let pixels = source.pixels.clone();
            let (u, v, w, h) = (rect[0] as i32, rect[1] as i32, rect[2] as i32, rect[3] as i32);
            let Some(picture) = self.scripted_picture(texture) else { return };
            for row in 0..h {
                for col in 0..w {
                    let (sx, sy) = (u + col, v + row);
                    if sx < 0 || sy < 0 || sx >= sw || sy >= sh {
                        continue;
                    }
                    let at = ((sy * sw + sx) * 4) as usize;
                    let texel = &pixels[at..at + 4];
                    // A font page is masked: what the artist left at palette zero is the gap
                    // between the letters, and the rest takes the colour it was asked for.
                    if texel[3] == 0 {
                        continue;
                    }
                    let shade = texel[0].max(texel[1]).max(texel[2]) as u32;
                    let mix = |c: u8| ((c as u32 * shade) / 255) as u8;
                    picture.put(x + col, y + row, [mix(colour[0]), mix(colour[1]), mix(colour[2]), 255]);
                }
            }
            picture.dirty = true;
            x += w;
        }
    }

    /// Empties a scripted texture's picture, which the engine does before its actor draws it.
    pub fn scripted_clear(&mut self, texture: ObjectKey) {
        if let Some(picture) = self.scripted_picture(texture) {
            picture.rgba.fill(0);
            picture.dirty = true;
        }
    }
}
