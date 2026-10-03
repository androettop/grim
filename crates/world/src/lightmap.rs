//! Baking the level's light data into a texture atlas.
//!
//! Each BSP surface carries a lightmap grid (`u_clamp` × `v_clamp` texels) and, per light that
//! reaches it, one visibility bit per texel. Verified on the game's maps: a surface's block is
//! `lights × ceil(u_clamp / 8) × v_clamp` bytes, rows padded to a byte, and a texel's position
//! along the surface is `(dot(P - Base, TextureU) - Pan.x) / UScale`. Texel centres sit at whole
//! coordinates, so a polygon's corners land on the first and last texel of its grid (checked on
//! every map: no vertex falls outside `0 ..= clamp - 1`).
//!
//! How bright each light leaves a texel is engine behaviour rather than file format. A light
//! holds full brightness up to `LightRadiusInner` and fades to nothing at `LightRadius`, times
//! the angle to the surface; lightmaps then modulate a texture at twice their value.

use grim_assets::model::Model;
use grim_package::ObjectRef;

use crate::{dot, scale, sub, Vec3};

/// A light that reaches surfaces, as the renderer needs it.
#[derive(Debug, Clone, Copy)]
pub struct LightSample {
    pub position: Vec3,
    pub color: [f32; 3],
    /// World units the light reaches.
    pub radius: f32,
    /// World units it stays at full brightness, from `LightRadiusInner`: a property this
    /// game's engine adds to `Actor`, holding a fraction of the radius in 0..255.
    pub inner: f32,
    /// Spotlights: the axis they point along and the cosine of their half angle.
    pub cone: Option<(Vec3, f32)>,
    /// `LE_NonIncidence` lights ignore how the surface faces them.
    pub ignores_angle: bool,
    /// `bDarkLight`: the light takes its colour away from the surface instead of adding it.
    pub dark: bool,
    /// `bSpecialLit`: the light only reaches surfaces marked `PF_SpecialLit`, and those
    /// surfaces take no light from any other. It is how a map lights a sky or a backdrop apart
    /// from the room below it.
    pub special: bool,
}

impl LightSample {
    /// How much of the light survives `distance`, before the angle to the surface.
    pub fn falloff(&self, distance: f32) -> f32 {
        if distance >= self.radius {
            return 0.0;
        }
        if distance <= self.inner {
            return 1.0;
        }
        let t = (distance - self.inner) / (self.radius - self.inner).max(1e-3);
        1.0 - t * t
    }

    /// How much of the light leaves it towards `to_light` (already pointing at the light).
    pub fn spread(&self, to_light: Vec3, distance: f32) -> f32 {
        let Some((axis, cos_half)) = self.cone else { return 1.0 };
        if distance <= 1e-3 {
            return 1.0;
        }
        let along = -dot(axis, scale(to_light, 1.0 / distance));
        if along <= cos_half {
            return 0.0;
        }
        // Fade over the outer part of the cone instead of cutting it off.
        ((along - cos_half) / (1.0 - cos_half)).min(1.0)
    }
}

/// Renderers modulate a texture by twice its light, so a half lit surface is drawn as it is.
pub const LIGHT_SCALE: f32 = 1.0;

/// Where a surface's lightmap sits inside the atlas.
#[derive(Debug, Clone, Copy, Default)]
pub struct Placement {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
    pub valid: bool,
}

pub struct Lightmaps {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
    /// Indexed like `Model::surfs`.
    pub placements: Vec<Placement>,
}

impl Lightmaps {
    /// Atlas coordinates of a point on a surface, given its texel coordinates. A texel's centre
    /// is at a whole coordinate, so the first one covers the edge of the polygon.
    pub fn atlas_uv(&self, surf: usize, texel: [f32; 2]) -> [f32; 2] {
        match self.placements.get(surf) {
            Some(p) if p.valid => [
                (p.x as f32 + 0.5 + texel[0].clamp(0.0, p.width as f32 - 1.0)) / self.width as f32,
                (p.y as f32 + 0.5 + texel[1].clamp(0.0, p.height as f32 - 1.0)) / self.height as f32,
            ],
            // Surfaces without a lightmap sample the fully lit texel kept at the origin.
            _ => self.white_uv(),
        }
    }

    /// The fully lit texel, for surfaces the engine draws at face value.
    pub fn white_uv(&self) -> [f32; 2] {
        [0.5 / self.width as f32, 0.5 / self.height as f32]
    }
}

/// What each baked lightmap belongs to. The level names its own from the surfaces
/// (`FBspSurf::iLightMap`); the brush a mover carries leaves that at -1 and numbers its
/// **editor polygons** instead, in `iBrushPoly`. Checked over the game: every one of the 1122
/// mover brushes that keeps light has exactly as many entries as it has numbered polygons,
/// numbered 0 upwards. It fits what the polygons are: the front of a secret door in
/// `EntryHall_hub` is one surface (its plane) but three polygons — stone, portrait, stone —
/// and each of the three carries its own picture and its own piece of baked light.
pub enum Pieces<'a> {
    /// The level: one piece per surface.
    Surfs(&'a Model),
    /// A brush: one piece per numbered polygon.
    Polys(&'a Model, &'a [grim_assets::polys::Poly]),
}

impl<'a> Pieces<'a> {
    /// A brush's polygons when it has them (a mover), its surfaces otherwise.
    pub fn of(model: &'a Model, polys: Option<&'a [grim_assets::polys::Poly]>) -> Self {
        match polys {
            Some(polys) if model.surfs.iter().all(|s| s.i_light_map < 0) => Self::Polys(model, polys),
            _ => Self::Surfs(model),
        }
    }

    pub fn model(&self) -> &Model {
        match self {
            Self::Surfs(m) | Self::Polys(m, _) => m,
        }
    }

    pub fn count(&self) -> usize {
        match self {
            Self::Surfs(m) => m.surfs.len(),
            Self::Polys(m, _) => m.light_map.len(),
        }
    }

    /// The piece a node draws with: its surface, or the polygon it was cut from.
    pub fn of_node(&self, node: &grim_assets::model::BspNode, poly: Option<&grim_assets::polys::Poly>) -> Option<usize> {
        match self {
            Self::Surfs(_) => (node.i_surf >= 0).then(|| node.i_surf as usize),
            Self::Polys(..) => {
                let brush_poly = poly?.i_brush_poly;
                (brush_poly >= 0).then_some(brush_poly as usize)
            }
        }
    }

    /// Whether a piece is lit only by the lights kept for it (`PF_SpecialLit`). The cloud
    /// layers over Privet Drive carry it: lit by the street lamps below them as well, they come
    /// out in circles instead of an even sheet.
    pub fn special_lit(&self, piece: usize) -> bool {
        const PF_SPECIAL_LIT: u32 = 0x0010_0000;
        match self {
            Self::Surfs(m) => m.surfs.get(piece).is_some_and(|s| s.poly_flags & PF_SPECIAL_LIT != 0),
            Self::Polys(_, polys) => polys.get(piece).is_some_and(|p| p.poly_flags & PF_SPECIAL_LIT != 0),
        }
    }

    /// Whether a piece is drawn from both sides. Its stored normal then says nothing about
    /// which side faces the room: `PrivetDr`'s drawing pinned to Harry's wall has a normal
    /// pointing into the wall, and lighting it only from that side left it black while the
    /// lights the map itself gave it stood in front of it.
    pub fn two_sided(&self, piece: usize) -> bool {
        const PF_TWO_SIDED: u32 = 0x0000_0100;
        match self {
            Self::Surfs(m) => m.surfs.get(piece).is_some_and(|s| s.poly_flags & PF_TWO_SIDED != 0),
            Self::Polys(_, polys) => polys.get(piece).is_some_and(|p| p.poly_flags & PF_TWO_SIDED != 0),
        }
    }

    /// The lightmap of a piece.
    pub fn map(&self, piece: usize) -> Option<&grim_assets::model::LightMapIndex> {
        match self {
            Self::Surfs(m) => {
                let index = m.surfs.get(piece)?.i_light_map;
                if index >= 0 {
                    return m.light_map.get(index as usize);
                }
                // A brush whose polygons were not handed over: one entry per surface, in order.
                (m.light_map.len() == m.surfs.len()).then(|| m.light_map.get(piece)).flatten()
            }
            Self::Polys(m, _) => m.light_map.get(piece),
        }
    }

    /// Where a piece sits and how its lightmap grid lies on it: base point and texture axes,
    /// which the lightmap's pan and scale are given in, and the plane's normal.
    fn frame(&self, piece: usize) -> Option<(Vec3, Vec3, Vec3, Vec3)> {
        let v3 = |v: grim_package::property::Vector| [v.x, v.y, v.z];
        match self {
            Self::Surfs(m) => {
                let surf = m.surfs.get(piece)?;
                Some((
                    v3(*m.points.get(surf.p_base.max(0) as usize)?),
                    v3(*m.vectors.get(surf.v_texture_u.max(0) as usize)?),
                    v3(*m.vectors.get(surf.v_texture_v.max(0) as usize)?),
                    v3(*m.vectors.get(surf.v_normal.max(0) as usize)?),
                ))
            }
            Self::Polys(_, polys) => {
                let poly = polys.iter().find(|p| p.i_brush_poly == piece as i32)?;
                Some((v3(poly.base), v3(poly.texture_u), v3(poly.texture_v), v3(poly.normal)))
            }
        }
    }

    /// Texel coordinates of a world position on a piece's lightmap grid.
    pub fn coords(&self, piece: usize, p: Vec3) -> Option<[f32; 2]> {
        let (base, tu, tv, _) = self.frame(piece)?;
        let lm = self.map(piece)?;
        let d = sub(p, base);
        Some([(dot(d, tu) - lm.pan.x) / lm.u_scale, (dot(d, tv) - lm.pan.y) / lm.v_scale])
    }

    /// World position of a texel of a piece's lightmap grid, the inverse of [`Self::coords`].
    /// The texture axes are not always perpendicular, so both coordinates are solved together.
    pub fn position(&self, piece: usize, texel: [f32; 2]) -> Option<Vec3> {
        let (base, tu, tv, normal) = self.frame(piece)?;
        let lm = self.map(piece)?;
        let (du, dv) = (texel[0] * lm.u_scale + lm.pan.x, texel[1] * lm.v_scale + lm.pan.y);
        // The texture axes are neither perpendicular nor always parallel to the surface, so the
        // offset from Base is whatever has those two coordinates and stays on the plane.
        let offset = solve3([tu, tv, normal], [du, dv, 0.0])?;
        Some([0, 1, 2].map(|i| base[i] + offset[i]))
    }

    pub fn normal(&self, piece: usize) -> Vec3 {
        self.frame(piece).map_or([0.0; 3], |f| f.3)
    }
}

/// Solves `rows * x = b` by Cramer's rule.
fn solve3(rows: [Vec3; 3], b: Vec3) -> Option<Vec3> {
    let det = |m: [Vec3; 3]| {
        m[0][0] * (m[1][1] * m[2][2] - m[1][2] * m[2][1]) - m[0][1] * (m[1][0] * m[2][2] - m[1][2] * m[2][0])
            + m[0][2] * (m[1][0] * m[2][1] - m[1][1] * m[2][0])
    };
    let d = det(rows);
    if d.abs() <= 1e-12 {
        return None;
    }
    let mut out = [0.0; 3];
    for (c, o) in out.iter_mut().enumerate() {
        let mut m = rows;
        for (r, row) in m.iter_mut().enumerate() {
            row[c] = b[r];
        }
        *o = det(m) / d;
    }
    Some(out)
}

fn hemisphere(n: Vec3, to_light: Vec3, distance: f32) -> f32 {
    if distance <= 1e-3 {
        return 1.0;
    }
    dot(n, scale(to_light, 1.0 / distance)).max(0.0)
}

/// Bakes every surface's lightmap into one atlas. `light` resolves the model's light references
/// and `ambient` holds each surface's zone ambient light.
pub fn bake(
    pieces: &Pieces<'_>,
    ambient: &[[f32; 3]],
    light_count: usize,
    light: &dyn Fn(ObjectRef) -> Option<LightSample>,
) -> Lightmaps {
    let model = pieces.model();
    // Pieces in atlas order: tallest first keeps the shelves tight.
    let mut order: Vec<usize> = (0..pieces.count()).filter(|&i| pieces.map(i).is_some()).collect();
    let size_of = |i: usize| -> (u32, u32) {
        match pieces.map(i) {
            Some(lm) => (lm.u_clamp.max(0) as u32, lm.v_clamp.max(0) as u32),
            None => (0, 0),
        }
    };
    order.sort_by_key(|&i| std::cmp::Reverse(size_of(i).1));

    // Shelf packing, with a one texel border so filtering does not bleed between surfaces.
    let total: u32 = order.iter().map(|&i| (size_of(i).0 + 2) * (size_of(i).1 + 2)).sum();
    let mut width = 64u32;
    while width * width < total.max(64) * 5 / 4 {
        width *= 2;
    }
    let width = width.min(4096);
    let mut placements = vec![Placement::default(); pieces.count()];
    let (mut pen_x, mut pen_y, mut shelf) = (1u32, 1u32, 0u32);
    for &i in &order {
        let (w, h) = size_of(i);
        if w == 0 || h == 0 {
            continue;
        }
        if pen_x + w + 1 > width {
            pen_x = 1;
            pen_y += shelf + 1;
            shelf = 0;
        }
        placements[i] = Placement { x: pen_x, y: pen_y, width: w, height: h, valid: true };
        pen_x += w + 1;
        shelf = shelf.max(h);
    }
    let height = (pen_y + shelf + 1).next_power_of_two().clamp(64, 4096);

    let mut rgba = vec![0u8; (width * height * 4) as usize];

    for piece in 0..pieces.count() {
        let place = placements[piece];
        if !place.valid {
            continue;
        }
        let Some(lm) = pieces.map(piece) else { continue };
        let normal = pieces.normal(piece);
        let both_sides = pieces.two_sided(piece);
        let only_special = pieces.special_lit(piece);

        // Lights reaching this surface, with their visibility bitmap.
        let stride = place.width.div_ceil(8) as usize;
        let mut lights = Vec::new();
        let mut k = lm.i_light_actors.max(0) as usize;
        if lm.i_light_actors >= 0 {
            while let Some(r) = model.lights.get(k) {
                if r.is_null() {
                    break;
                }
                let offset = lm.data_offset.max(0) as usize + lights.len() * stride * place.height as usize;
                lights.push((light(*r), offset));
                k += 1;
            }
        }

        for y in 0..place.height {
            for x in 0..place.width {
                let Some(p) = pieces.position(piece, [x as f32, y as f32]) else { continue };
                let mut reaching: Vec<(f32, [f32; 3])> = Vec::new();
                for (sample, offset) in &lights {
                    let Some(s) = sample else { continue };
                    // A surface kept for the special lights takes light from those alone, and
                    // every other surface takes none of theirs.
                    if s.special != only_special {
                        continue;
                    }
                    let byte = offset + y as usize * stride + (x / 8) as usize;
                    if model.light_bits.get(byte).is_none_or(|b| b >> (x % 8) & 1 == 0) {
                        continue;
                    }
                    let to_light = sub(s.position, p);
                    let distance = dot(to_light, to_light).sqrt();
                    if distance >= s.radius {
                        continue;
                    }
                    let spread = s.spread(to_light, distance);
                    if spread <= 0.0 {
                        continue;
                    }
                    let angle = if s.ignores_angle {
                        1.0
                    } else if both_sides {
                        // A two-sided surface takes light from whichever side it comes.
                        hemisphere(normal, to_light, distance).max(hemisphere([-normal[0], -normal[1], -normal[2]], to_light, distance))
                    } else {
                        hemisphere(normal, to_light, distance)
                    };
                    let intensity = s.falloff(distance) * spread * angle;
                    let color = s.color.map(|c| if s.dark { -c * intensity } else { c * intensity });
                    reaching.push((color[0].abs() + color[1].abs() + color[2].abs(), color));
                }
                // A zone applies only its strongest few lights to a surface, which is what
                // `ZoneInfo.MaxLightCount` counts; the rest never reach the lightmap.
                if reaching.len() > light_count {
                    reaching.sort_by(|a, b| b.0.total_cmp(&a.0));
                    reaching.truncate(light_count);
                }
                let mut acc = ambient.get(piece).copied().unwrap_or_else(|| ambient.first().copied().unwrap_or([0.0; 3]));
                for (_, color) in &reaching {
                    for (a, c) in acc.iter_mut().zip(color) {
                        *a += c;
                    }
                }
                let at = ((place.y + y) * width + place.x + x) as usize * 4;
                for c in 0..3 {
                    rgba[at + c] = (acc[c].clamp(0.0, 1.0) * 255.0) as u8;
                }
            }
        }
        // Border: repeat the edge texels so bilinear filtering stays inside the surface.
        for y in 0..place.height + 2 {
            for x in 0..place.width + 2 {
                if x != 0 && y != 0 && x <= place.width && y <= place.height {
                    continue;
                }
                let sx = place.x + x.clamp(1, place.width) - 1;
                let sy = place.y + y.clamp(1, place.height) - 1;
                let (dx, dy) = (place.x + x - 1, place.y + y - 1);
                if dx >= width || dy >= height {
                    continue;
                }
                let (src, dst) = (((sy * width + sx) * 4) as usize, ((dy * width + dx) * 4) as usize);
                let texel = [rgba[src], rgba[src + 1], rgba[src + 2], rgba[src + 3]];
                rgba[dst..dst + 4].copy_from_slice(&texel);
            }
        }
    }

    // Texel (0, 0) leaves a texture as it is: lightmaps modulate at twice their value, so half
    // is the neutral one. Surfaces the engine draws at face value, such as the sky, sample it.
    // It is written last because the borders of the first lightmap reach over it.
    rgba[..4].copy_from_slice(&[128, 128, 128, 255]);
    Lightmaps { width, height, rgba, placements }
}
