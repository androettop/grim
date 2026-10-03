//! Textures the engine draws itself, frame by frame: the fires, the sparkles and the rippled
//! glass of `Fire.*Texture`. They are stored with no pixels at all — only a palette, the
//! emitters they carry and the settings below — so nothing of them shows until the engine
//! runs them.
//!
//! What comes from the files: the size, the palette, the emitter list (`Sparks` for a fire,
//! `Drops` for water) and every `FX_` setting. What is a hypothesis, marked where it is used:
//! how heat spreads and fades between frames, and how a drop's ripple travels.

use grim_assets::procedural::{ProceduralKind, ProceduralTexture, Spark};
use grim_assets::texture::Rgba;

/// An emitter of a fire as it runs: where it is, where it is heading and how far it has got.
#[derive(Debug, Clone, Copy)]
struct Live {
    home: [f32; 2],
    at: [f32; 2],
    heading: [f32; 2],
    flown: f32,
}

/// One emitter of a water texture: `Fire.WaterTexture.ADrop`, eight bytes like a spark.
#[derive(Debug, Clone, Copy)]
pub struct Drop {
    pub kind: u8,
    pub depth: u8,
    pub x: u8,
    pub y: u8,
    /// The last of the four spare bytes: how fast this drop's own phase turns.
    pub rate: u8,
}

/// The settings a procedural texture is driven by, as the file names them.
#[derive(Debug, Clone, Copy, Default)]
pub struct Settings {
    pub render_heat: u8,
    pub rising: bool,
    /// `bParametric`: the surface is written down from its drops instead of being let go.
    pub parametric: bool,
    pub fx_heat: u8,
    pub fx_size: u8,
    pub fx_area: u8,
    pub fx_frequency: u8,
    pub fx_phase: u8,
    pub fx_horiz_speed: u8,
    pub fx_vert_speed: u8,
    pub fx_amplitude: u8,
    pub fx_speed: u8,
    pub fx_depth: u8,
    pub wave_amp: u8,
}

/// A texture the engine keeps redrawing.
pub struct Procedural {
    pub kind: ProceduralKind,
    pub width: usize,
    pub height: usize,
    settings: Settings,
    palette: Vec<[u8; 4]>,
    /// Fire: how hot each texel is. Water: the two states of the height field it waves in.
    heat: Vec<u8>,
    wave: [Vec<i16>; 2],
    /// Which of the two the surface is in now.
    current: usize,
    sparks: Vec<Spark>,
    /// The fire's emitters as they run, seeded from the ones the file keeps: `TRAILS` of them
    /// per emitter, one after another.
    live: Vec<Live>,
    drops: Vec<Drop>,
    /// What a glass texture bends, already decoded.
    source: Option<Rgba>,
    /// Seconds waited since the last step, so the picture runs at its own pace.
    waited: f32,
    /// Frames run, which the moving emitters take their phase from.
    step: u32,
    rng: u32,
    /// For a parametric water: the part of each drop's wave that never changes, the sine and
    /// the cosine of how far along the wave every cell of the grid lies, drop after drop.
    waves: Option<(Vec<f32>, Vec<f32>)>,
}

/// The engine steps these pictures at a fixed pace rather than once a frame, so they look the
/// same however fast the game is drawing. Hypothesis: 35 steps a second, which is the pace
/// Unreal's own textures were written for.
const STEPS_PER_SECOND: f32 = 35.0;

/// How many sparks an emitter that throws them keeps in the air at once. The file gives an
/// emitter one spark and room for far more: the wand's cursor stores 9 of them with a
/// `SparksLimit` of 256, so the picture is meant to carry several per emitter. How many is a
/// hypothesis, set against the original by eye.
const TRAILS: usize = 3;

impl Procedural {
    /// Builds the state of a procedural texture. `source` is what a glass texture bends, which
    /// only the callers can resolve.
    pub fn new(texture: &ProceduralTexture, settings: Settings, palette: Vec<[u8; 4]>, drops: Vec<Drop>, source: Option<Rgba>) -> Option<Self> {
        let mip = texture.texture.mips.first()?;
        // Sparks thrown by one emitter start spread along its reach, so they leave rays of
        // their own instead of flying as one.
        let stagger = settings.fx_area.max(1) as f32 / TRAILS as f32;
        let (width, height) = (mip.u_size.max(1) as usize, mip.v_size.max(1) as usize);
        Some(Self {
            kind: texture.kind,
            width,
            height,
            settings,
            palette,
            heat: vec![0; width * height],
            wave: [vec![0; width * height], vec![0; width * height]],
            current: 0,
            sparks: texture.sparks.clone(),
            live: texture
                .sparks
                .iter()
                .flat_map(|s| {
                    (0..TRAILS).map(move |k| Live {
                        home: [s.x as f32, s.y as f32],
                        at: [s.x as f32, s.y as f32],
                        heading: [(s.byte_a as f32 - 128.0) / 64.0, (s.byte_b as f32 - 128.0) / 64.0],
                        flown: k as f32 * stagger,
                    })
                })
                .collect(),
            drops,
            source,
            waited: 0.0,
            step: 0,
            rng: 0x2545_f491,
            waves: None,
        })
    }

    /// How many emitters it carries, for the reports.
    pub fn spark_count(&self) -> usize {
        self.sparks.len()
    }

    /// The lowest and highest the surface reaches, for the reports.
    pub fn wave_range(&self) -> (i16, i16) {
        let b = &self.wave[self.current];
        (b.iter().copied().min().unwrap_or(0), b.iter().copied().max().unwrap_or(0))
    }

    /// How many drops it carries, for the reports.
    pub fn drop_count(&self) -> usize {
        self.drops.len()
    }

    /// How many colours its palette holds, for the reports.
    pub fn palette_len(&self) -> usize {
        self.palette.len()
    }

    fn random(&mut self) -> u32 {
        self.rng ^= self.rng << 13;
        self.rng ^= self.rng >> 17;
        self.rng ^= self.rng << 5;
        self.rng
    }

    /// Moves the picture on by that much time and says whether it changed.
    pub fn tick(&mut self, dt: f32) -> bool {
        self.waited += dt;
        let steps = (self.waited * STEPS_PER_SECOND) as u32;
        if steps == 0 {
            return false;
        }
        self.waited -= steps as f32 / STEPS_PER_SECOND;
        for _ in 0..steps.min(4) {
            self.step += 1;
            match self.kind {
                ProceduralKind::Fire => self.step_fire(),
                ProceduralKind::Wave | ProceduralKind::Wet | ProceduralKind::Ice => self.step_water(),
                ProceduralKind::Scripted => {}
            }
        }
        true
    }

    /// One step of a fire: each emitter puts a point of heat where it is, and what is already
    /// there spreads to the texels around it and cools.
    ///
    /// Hypothesis about the emitters, from what the files carry: the `FX_` speeds are signed
    /// about 128, so 128 is standing still — the wand's sparkle carries 130 and 142, a slow
    /// drift — and an emitter's own two spare bytes are where it is between texels, which is
    /// what lets several emitters of one kind spread out instead of sitting on top of each
    /// other. The kinds themselves are `Fire.FireTexture.ESpark`.
    fn step_fire(&mut self) {
        const SPARK_BURN: u8 = 0;
        const SPARK_SPARKLE: u8 = 1;
        const SPARK_PULSE: u8 = 2;
        const SPARK_BLAZE: u8 = 4;
        const SPARK_CYLINDER: u8 = 9;
        const SPARK_CYLINDER_3D: u8 = 10;
        const SPARK_LISSAJOUS: u8 = 11;
        const SPARK_JUGGLERS: u8 = 12;
        const SPARK_FLOCKS: u8 = 15;
        const SPARK_ORGANIC: u8 = 17;
        const SPARK_RANDOM_CLOUD: u8 = 19;
        const SPARK_LOCAL_CLOUD: u8 = 21;
        const SPARK_LINE_LIGHTNING: u8 = 23;
        const SPARK_SPHERE_LIGHTNING: u8 = 25;
        const SPARK_WHEEL: u8 = 26;
        const SPARK_GAMETES: u8 = 27;
        const SPARK_SPRINKLER: u8 = 28;

        let (w, h) = (self.width, self.height);
        let signed = |b: u8| b as f32 - 128.0;
        let drift = (signed(self.settings.fx_horiz_speed) / 16.0, signed(self.settings.fx_vert_speed) / 16.0);
        // How far an emitter gets from where it sits before it starts over: `FX_Area` is that
        // reach in texels, which on the wand's 64 texel picture is a burst across most of it.
        let area = self.settings.fx_area.max(1) as f32;
        for i in 0..self.sparks.len() {
            let spark = self.sparks[i];
            let own = (spark.byte_a as f32 / 256.0, spark.byte_b as f32 / 256.0);
            let turn = (self.step as f32 * self.settings.fx_frequency.max(1) as f32 / 512.0 + own.0 * std::f32::consts::TAU)
                % std::f32::consts::TAU;
            let (mut x, mut y) = (spark.x as f32, spark.y as f32);
            match spark.kind {
                // A point that stays where it is and burns.
                SPARK_BURN | SPARK_PULSE | SPARK_RANDOM_CLOUD | SPARK_LOCAL_CLOUD => {}
                // Ones that are thrown outwards and start again when they have gone far
                // enough, each time in a new direction. That is what makes a handful of them
                // a burst of rays: the file keeps them clustered where they are thrown from,
                // with the heading each one had at the time in its two spare bytes, and
                // `FX_Area` is how far they get. Hypothesis: a new heading is picked at
                // random, which is what fills the burst out evenly.
                SPARK_BLAZE | SPARK_SPARKLE | SPARK_FLOCKS | SPARK_ORGANIC | SPARK_GAMETES => {
                    let pace = 1.0 + drift.0.abs() + drift.1.abs();
                    let heat = spark.heat.max(self.settings.fx_heat);
                    for k in 0..TRAILS {
                        let mut live = self.live[i * TRAILS + k];
                        if live.flown >= area || live.heading == [0.0, 0.0] {
                            let angle = self.random() as f32 / u32::MAX as f32 * std::f32::consts::TAU;
                            let speed = (live.heading[0].powi(2) + live.heading[1].powi(2)).sqrt().max(1.0);
                            let flown = (live.flown - area).max(0.0);
                            live = Live {
                                home: live.home,
                                at: live.home,
                                heading: [angle.cos() * speed, angle.sin() * speed],
                                flown,
                            };
                        }
                        let speed = (live.heading[0].powi(2) + live.heading[1].powi(2)).sqrt();
                        live.at[0] += live.heading[0] * pace / 4.0;
                        live.at[1] += live.heading[1] * pace / 4.0;
                        live.flown += speed * pace / 4.0;
                        self.live[i * TRAILS + k] = live;
                        let at = (live.at[1] as i32).rem_euclid(h as i32) as usize * w
                            + (live.at[0] as i32).rem_euclid(w as i32) as usize;
                        self.heat[at] = self.heat[at].max(heat);
                    }
                    continue;
                }
                // Ones that go round: a ring, a wheel, a spray.
                SPARK_WHEEL | SPARK_CYLINDER | SPARK_CYLINDER_3D | SPARK_SPRINKLER | SPARK_JUGGLERS => {
                    x += turn.cos() * area;
                    y += turn.sin() * area;
                }
                // A figure of eight.
                SPARK_LISSAJOUS => {
                    x += turn.cos() * area;
                    y += (turn * 2.0).sin() * area;
                }
                // Lightning strikes from where it is towards the middle, a step at a time.
                SPARK_LINE_LIGHTNING | SPARK_SPHERE_LIGHTNING => {
                    let reach = (self.step % 32) as f32 / 32.0;
                    x += (w as f32 / 2.0 - x) * reach;
                    y += (h as f32 / 2.0 - y) * reach;
                }
                _ => {
                    x += drift.0 * self.step as f32;
                    y += drift.1 * self.step as f32;
                }
            }
            let heat = spark.heat.max(self.settings.fx_heat);
            let at = (y as i32).rem_euclid(h as i32) as usize * w + (x as i32).rem_euclid(w as i32) as usize;
            self.heat[at] = self.heat[at].max(heat);
        }
        // Hypothesis: heat stays where it was laid and cools, and only a little of it reaches
        // the texels around. That keeps the trail an emitter leaves as a line of its own,
        // which is what a spark looks like; spreading it evenly turns the whole picture into
        // one blob. A rising fire draws from the row below instead of its own.
        let mut next = vec![0u8; w * h];
        let decay = 1 + 255 / self.settings.fx_size.max(1) as u32;
        for y in 0..h {
            for x in 0..w {
                let from = if self.settings.rising { (y + 1) % h } else { y };
                let at = |xx: usize, yy: usize| self.heat[(yy % h) * w + (xx % w)] as u32;
                let around =
                    at(x, (from + 1) % h) + at((x + w - 1) % w, from) + at((x + 1) % w, from) + at(x, (from + h - 1) % h);
                let kept = at(x, from) * 4 + around;
                next[y * w + x] = (kept / 8).saturating_sub(decay) as u8;
            }
        }
        self.heat = next;
    }

    /// One step of water: every drop drives the surface where it sits, and the wave that makes
    /// travels outwards.
    ///
    /// Hypothesis about the drops, from what the files carry: the last of a drop's four spare
    /// bytes is how fast its own phase turns, in 1/256 of a turn per step — the steady sources
    /// of the menu icons carry 8, a rain drop 30 and a whirling one 2 — and the first bytes are
    /// whatever else its kind needs. The kinds themselves are `Fire.WaterTexture.WDrop`.
    fn step_water(&mut self) {
        // A parametric surface is not let go: it is written down whole, every step.
        if self.settings.parametric {
            return self.paint_waves();
        }
        const DROP_FIXED_DEPTH: u8 = 0;
        const DROP_PHASE_SPOT: u8 = 1;
        const DROP_SHALLOW_SPOT: u8 = 2;
        const DROP_HALF_AMPL: u8 = 3;
        const DROP_RANDOM_MOVER: u8 = 4;
        const DROP_FIXED_RANDOM_SPOT: u8 = 5;
        const DROP_WHIRLY_THING: u8 = 6;
        const DROP_BIG_WHIRLY: u8 = 7;
        const DROP_HORIZONTAL_OSC: u8 = 12;
        const DROP_RAIN_DROPS: u8 = 16;

        let (w, h) = (self.width, self.height);
        for i in 0..self.drops.len() {
            let drop = self.drops[i];
            let turn = (self.step.wrapping_mul(drop.rate.max(1) as u32) % 256) as f32 / 256.0 * std::f32::consts::TAU;
            let depth = -(drop.depth as i32);
            let (mut x, mut y) = (drop.x as i32, drop.y as i32);
            // Hypothesis: `FX_Frequency` and `FX_Phase` are what make a surface look like
            // water rather than one ring spreading out. A wet texture of the menu carries a
            // row of some thirty shallow spots across it with a frequency of 8 and a phase of
            // 8; driven together they push out a single front, but given a phase that walks
            // along the row they fluctuate, which is what the original looks like.
            // `FX_Phase` turns of phase across the picture, so a row of sources does not push
            // as one: each is a little further along than the one beside it, which is what
            // makes the surface fluctuate instead of swelling into a single ring.
            let along = (drop.x as f32 + drop.y as f32) / w as f32 * self.settings.fx_phase as f32;
            let turns = self.step as f32 * self.settings.fx_frequency.max(1) as f32 / 256.0 + along;
            let fluctuation = (turns * std::f32::consts::TAU).sin();
            let amount = match drop.kind {
                // Sources that simply hold the surface down where they are.
                DROP_FIXED_DEPTH | DROP_FIXED_RANDOM_SPOT => depth,
                // One that only ripples the surface instead of holding it down.
                DROP_SHALLOW_SPOT => (depth as f32 * fluctuation) as i32,
                // Half of one, which is what the kind is named after.
                DROP_HALF_AMPL => depth / 2,
                // One that rises and falls instead of holding.
                DROP_PHASE_SPOT => (depth as f32 * turn.sin()) as i32,
                // Ones that carry their source around: a small circle, a wide one, a wander,
                // or a run along the row they sit in.
                DROP_WHIRLY_THING | DROP_BIG_WHIRLY => {
                    let reach = if drop.kind == DROP_BIG_WHIRLY { w as f32 / 4.0 } else { w as f32 / 16.0 };
                    x += (turn.cos() * reach) as i32;
                    y += (turn.sin() * reach) as i32;
                    depth
                }
                DROP_RANDOM_MOVER => {
                    let step = (self.random() % 3) as i32 - 1;
                    self.drops[i].x = (drop.x as i32 + step).rem_euclid(w as i32) as u8;
                    x = self.drops[i].x as i32;
                    depth
                }
                DROP_HORIZONTAL_OSC => {
                    x += (turn.sin() * (w as f32 / 4.0)) as i32;
                    depth
                }
                // Rain falls in one spot at a time, and only now and then.
                DROP_RAIN_DROPS => {
                    if self.step % (drop.rate.max(1) as u32) != 0 {
                        continue;
                    }
                    x = (self.random() % w as u32) as i32;
                    y = (self.random() % h as u32) as i32;
                    depth
                }
                _ => depth,
            };
            let at = (y.rem_euclid(h as i32) as usize) * w + x.rem_euclid(w as i32) as usize;
            self.wave[self.current][at] = amount.clamp(i16::MIN as i32, i16::MAX as i32) as i16;
        }
        // The usual step of a wave: where the surface is heading is the average of what is
        // around it, twice over, less where it was, and a little is lost on the way.
        let (now, next) = (self.current, 1 - self.current);
        for y in 0..h {
            for x in 0..w {
                let at = |b: usize, xx: usize, yy: usize| self.wave[b][(yy % h) * w + (xx % w)] as i32;
                let sum = at(now, x, (y + 1) % h) + at(now, (x + w - 1) % w, y) + at(now, (x + 1) % w, y) + at(now, x, (y + h - 1) % h);
                let value = sum / 2 - at(next, x, y);
                self.wave[next][y * w + x] = (value - value / 32).clamp(i16::MIN as i32, i16::MAX as i32) as i16;
            }
        }
        self.current = next;
    }

    /// The surface of a parametric water texture: the waves its drops stand for, written
    /// straight into the height field instead of being let go from them.
    ///
    /// Hypothesis: with `bParametric` a drop is not a place a ripple spreads from over the
    /// frames but a wave already spread over the whole picture, a ring of its own turning in
    /// place. What points at it: the user says the original ripples over its whole area from
    /// the first frame, like a water surface, with nothing travelling out of a centre — which
    /// is what a wave written down does and a wave let go does not.
    ///
    /// The drops sit in a grid half the picture's size, which is what their numbers say: over
    /// the 69 water textures of the game no drop's `Y` ever reaches half the height, while `X`
    /// runs the whole byte and wraps. Read that way the icons carry a row of sources along one
    /// edge, spanning the width. `FX_Size` is the length of the wave, the drop's last byte how
    /// fast it turns and `FX_Phase` how far the phase walks from one drop to the next, so a
    /// row of them does not swell as one.
    fn paint_waves(&mut self) {
        let (w, h) = (self.width, self.height);
        // The picture is smoothed back up from the grid, so a coarse one is enough: the
        // shortest wave any of the game's textures carries still spans eight cells of it, and
        // it keeps the biggest (256x256 with 86 drops) cheap.
        const GRID: usize = 64;
        let (fw, fh) = ((w / 2).max(1), (h / 2).max(1));
        let (gw, gh) = (fw.min(GRID), fh.min(GRID));
        let (to_grid_x, to_grid_y) = (gw as f32 / fw as f32, gh as f32 / fh as f32);
        let length = (self.settings.fx_size.max(1) as f32 / 2.0 * to_grid_x).max(1.0);
        let cells = gw * gh;
        // Where each cell lies along each drop's wave does not change from one step to the
        // next, only how far the wave has turned: sin(a - t) = sin a cos t - cos a sin t, so the
        // sines and cosines of the first are worked out once and each step only weighs them.
        // The acid of `Adv3DungeonQuest` has 120 drops over a 64x64 grid, and working out every
        // sine again every step cost seventeen milliseconds a step.
        if self.waves.is_none() {
            let (mut sines, mut cosines) = (Vec::with_capacity(self.drops.len() * cells), Vec::with_capacity(self.drops.len() * cells));
            for drop in &self.drops {
                let at = [(drop.x as usize % fw) as f32 * to_grid_x, (drop.y as usize % fh) as f32 * to_grid_y];
                for y in 0..gh {
                    for x in 0..gw {
                        let (dx, dy) = (x as f32 - at[0], y as f32 - at[1]);
                        let along = (dx * dx + dy * dy).sqrt() / length * std::f32::consts::TAU;
                        sines.push(along.sin());
                        cosines.push(along.cos());
                    }
                }
            }
            self.waves = Some((sines, cosines));
        }
        let Some((sines, cosines)) = &self.waves else { return };
        let mut grid = vec![0f32; cells];
        for (i, drop) in self.drops.iter().enumerate() {
            let turned = self.step as f32 * drop.rate.max(1) as f32 / 256.0;
            // How far along the phase is where the drop sits: a row of them does not swell as
            // one, which is what leaves the surface looking like water and not like a bellows.
            let walk = (drop.x as f32 + drop.y as f32) * self.settings.fx_phase as f32 / 256.0;
            // The drops add up: what the game's textures carry says they were balanced that
            // way, because however many a picture has, their depths come to much the same
            // total — 18 of 208 in a spell's symbol, 46 of 62 on the rug of `Spongify` and
            // 120 of 32 in the acid of `Adv3DungeonQuest`, all of them about 3800.
            let depth = drop.depth as f32;
            let (sin_t, cos_t) = ((walk - turned) * std::f32::consts::TAU).sin_cos();
            let (a, b) = (depth * cos_t, depth * sin_t);
            let (s, c) = (&sines[i * cells..(i + 1) * cells], &cosines[i * cells..(i + 1) * cells]);
            for ((g, &s), &c) in grid.iter_mut().zip(s).zip(c) {
                *g += a * s + b * c;
            }
        }
        // Back up to the picture's own size, between the cells around each texel.
        let now = self.current;
        for y in 0..h {
            for x in 0..w {
                // The cells are not read round the edge: the waves do not meet where the
                // picture ends, so wrapping there leaves a seam a texel wide.
                let (fx, fy) = (x as f32 * gw as f32 / w as f32, y as f32 * gh as f32 / h as f32);
                let (x0, y0) = ((fx as usize).min(gw - 1), (fy as usize).min(gh - 1));
                let (x1, y1) = ((x0 + 1).min(gw - 1), (y0 + 1).min(gh - 1));
                let (tx, ty) = (fx - fx.floor(), fy - fy.floor());
                let top = grid[y0 * gw + x0] * (1.0 - tx) + grid[y0 * gw + x1] * tx;
                let bottom = grid[y1 * gw + x0] * (1.0 - tx) + grid[y1 * gw + x1] * tx;
                let value = top * (1.0 - ty) + bottom * ty;
                self.wave[now][y * w + x] = value.clamp(i16::MIN as f32, i16::MAX as f32) as i16;
            }
        }
    }

    /// The picture as it stands.
    pub fn rgba(&self) -> Vec<u8> {
        let (w, h) = (self.width, self.height);
        let mut out = vec![0u8; w * h * 4];
        match self.kind {
            // A fire is a picture of heat looked up in its own palette, a colour per level.
            // `RenderHeat` is not part of that: half of the game's fires do not set it at all
            // and the ones that do would be left almost black by scaling with it.
            ProceduralKind::Fire => {
                for (i, &heat) in self.heat.iter().enumerate() {
                    let colour = self.palette.get(heat as usize).copied().unwrap_or([0, 0, 0, 255]);
                    out[i * 4..i * 4 + 4].copy_from_slice(&colour);
                }
            }
            // Glass bends what is behind it by how steep the surface is where it is looked
            // through; with nothing behind it, the surface is shown as light and shade.
            _ => {
                // How far the surface bends what is behind it. Hypothesis: `FX_Amplitude` is
                // that reach, and `WaveAmp` only stands in when a texture leaves it at zero.
                // What points at it: with it the three kinds of surface the game draws — the
                // symbol of a spell (10), the rug of `Spongify` (64) and the acid of the
                // dungeon (63) — all bend by a few hundredths of their picture, while going
                // by `WaveAmp` (128, 96, 128) they would bend by wildly different amounts.
                let amp = match (self.settings.fx_amplitude, self.settings.wave_amp) {
                    (0, wave) => wave as i32,
                    (fx, _) => fx as i32,
                };
                let wave = &self.wave[self.current];
                // How steep the surface is, taken between the texels beside this one and never
                // round the picture: the surface does not meet itself at the edges, so reading
                // round them leaves a line of the far side showing.
                let at = |xx: usize, yy: usize| wave[yy.min(h - 1) * w + xx.min(w - 1)] as i32;
                match &self.source {
                    Some(src) if src.width > 0 && src.height > 0 => {
                        let (sw, sh) = (src.width as usize, src.height as usize);
                        // Where each column and row of the picture falls in what is behind it,
                        // worked out once rather than divided out for every texel.
                        let columns: Vec<usize> = (0..w).map(|x| (x * sw / w).min(sw - 1)).collect();
                        let rows: Vec<usize> = (0..h).map(|y| (y * sh / h).min(sh - 1)).collect();
                        for y in 0..h {
                            for x in 0..w {
                                let dx = (at(x + 1, y) - at(x.saturating_sub(1), y)) * amp / 4096;
                                let dy = (at(x, y + 1) - at(x, y.saturating_sub(1))) * amp / 4096;
                                // Bent within the picture, not round it: what is bent in from
                                // the far edge is a line of the other side showing through.
                                let sx = columns[(x as i32 + dx).clamp(0, w as i32 - 1) as usize];
                                let sy = rows[(y as i32 + dy).clamp(0, h as i32 - 1) as usize];
                                let (i, j) = ((y * w + x) * 4, (sy * sw + sx) * 4);
                                out[i..i + 4].copy_from_slice(&src.pixels[j..j + 4]);
                            }
                        }
                    }
                    // With nothing behind it there is nothing to bend, so the surface itself is
                    // shown as light and shade. Only the diagnostics take this road, so it is
                    // scaled to whatever the surface is doing.
                    _ => {
                        let reach = wave.iter().map(|v| v.unsigned_abs() as i32).max().unwrap_or(1).max(1);
                        for (i, &value) in wave.iter().enumerate() {
                            let shade = (128 + value as i32 * 127 / reach).clamp(0, 255) as u8;
                            out[i * 4..i * 4 + 4].copy_from_slice(&[shade, shade, shade, 255]);
                        }
                    }
                }
            }
        }
        out
    }
}
