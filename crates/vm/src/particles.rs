//! `ParticleFX`: the emitters the engine runs itself. Every effect of the game is a subclass
//! that changes nothing but the class defaults, so the behaviour belongs here rather than in
//! the scripts.
//!
//! The numbers an emitter is made of come in pairs, `(Base, Rand)`, which this reads as a
//! value picked uniformly in `[Base, Base + Rand]`. Hypothesis, from the game's own defaults:
//! `SpellTarget`, the sparkle the wand puts where it aims, declares
//! `ColorStart = ((255, 0, 0), (255, 255, 0))`, and is red through yellow on screen.

use grim_object::{ObjectKey, PropType, Value};

use crate::vm::{Vm, VmResult};

/// One live particle.
#[derive(Debug, Clone, Copy)]
pub struct Particle {
    pub position: [f32; 3],
    pub velocity: [f32; 3],
    /// How long it has lived and how long it lives, in seconds.
    pub age: f32,
    pub lifetime: f32,
    pub color_start: [f32; 3],
    pub color_end: [f32; 3],
    pub alpha_start: f32,
    pub alpha_end: f32,
    pub width: f32,
    pub length: f32,
    /// What the size is multiplied by at the end of the particle's life. The game does set
    /// this below zero (`SpellCursor.SetSparklesSeeking` asks for -0.5), so a particle may
    /// shrink through nothing and come back mirrored.
    pub end_scale: f32,
    /// Turn around the line of sight, and how fast it turns, in turns per second.
    pub spin: f32,
    pub spin_rate: f32,
    /// Seconds each of colour, alpha and size holds its start value before it heads for its
    /// end one (`ColorDelay`, `AlphaDelay`, `SizeDelay`).
    pub delays: Fade<f32>,
    /// Fraction of its life a particle spends coming up from nothing to its start alpha and
    /// size (`AlphaGrowPeriod`, `SizeGrowPeriod`).
    pub alpha_grow: f32,
    pub size_grow: f32,
}

/// One value each for a particle's colour, alpha and size.
#[derive(Debug, Default, Clone, Copy)]
pub struct Fade<T> {
    pub color: T,
    pub alpha: T,
    pub size: T,
}

impl Particle {
    /// Colour, alpha and size at the age it has reached.
    ///
    /// What the delays and growth periods mean is what `ParticleFX` says of them: a delay is
    /// "time to wait before beginning transition" from the start value to the end one, and a
    /// grow period the "fraction of lifetime spent growing to" the start value. Hypothesis:
    /// both run linearly, and a transition that waited takes what is left of the life.
    pub fn shade(&self) -> ([f32; 4], [f32; 2]) {
        let life = self.lifetime.max(1e-3);
        let t = |delay: f32| {
            if self.age <= delay || delay >= life {
                0.0
            } else {
                ((self.age - delay) / (life - delay)).clamp(0.0, 1.0)
            }
        };
        let (tc, ta, ts) = (t(self.delays.color), t(self.delays.alpha), t(self.delays.size));
        let mix = |a: f32, b: f32, t: f32| a + (b - a) * t;
        let grown = |period: f32| {
            let lived = self.age / life;
            if period > 0.0 && lived < period { lived / period } else { 1.0 }
        };
        let color = [
            mix(self.color_start[0], self.color_end[0], tc),
            mix(self.color_start[1], self.color_end[1], tc),
            mix(self.color_start[2], self.color_end[2], tc),
            mix(self.alpha_start, self.alpha_end, ta) * grown(self.alpha_grow),
        ];
        let scale = mix(1.0, self.end_scale, ts) * grown(self.size_grow);
        (color, [self.width * scale, self.length * scale])
    }
}

/// The live state of one emitter. `EmissionResidue` and `Age` are the game's own names: its
/// saved defaults carry both, so the engine keeps them per emitter exactly like this.
#[derive(Debug, Default, Clone)]
pub struct Emitter {
    pub particles: Vec<Particle>,
    pub residue: f32,
    pub age: f32,
    /// How many it has let out since it started.
    pub emitted: u32,
    /// Whether it has already been caught up to the state it would be in had it been running
    /// all along (`bSteadyState`).
    pub primed: bool,
}

/// No emitter of this game asks for more than a few hundred at once; the cap only keeps a
/// runaway rate from filling memory.
const MAX_PARTICLES: usize = 4096;

impl Vm {
    /// Reads a `(Base, Rand)` pair and picks a value from it.
    fn param(&mut self, a: ObjectKey, name: &str) -> VmResult<f32> {
        let (base, rand) = self.param_pair(a, name)?;
        let r = self.frand();
        Ok(base + rand * r)
    }

    /// The pair itself, for the callers that need both halves.
    fn param_pair(&mut self, a: ObjectKey, name: &str) -> VmResult<(f32, f32)> {
        let Value::Struct(fields) = self.get_prop(a, name)? else { return Ok((0.0, 0.0)) };
        let slots = self.struct_slots(a, name, &["Base", "Rand"])?;
        let at = |i: usize| match slots[i].and_then(|s| fields.get(s)) {
            Some(Value::Float(v)) => *v,
            Some(Value::Int(v)) => *v as f32,
            _ => 0.0,
        };
        Ok((at(0), at(1)))
    }

    /// A colour pair, whose halves are `Color` structs of their own. A `Color` keeps its
    /// channels in the order the struct declares them, which is not the order they are read
    /// in, so each one is found by name.
    fn color_param(&mut self, a: ObjectKey, name: &str) -> VmResult<[f32; 3]> {
        let Value::Struct(fields) = self.get_prop(a, name)? else { return Ok([1.0; 3]) };
        let halves = self.struct_slots(a, name, &["Base", "Rand"])?;
        let channels = self.color_slots(a, name)?;
        let mut out = [0.0; 3];
        for (i, half) in halves.iter().enumerate() {
            let Some(Value::Struct(color)) = half.and_then(|s| fields.get(s)) else { continue };
            for (c, out) in out.iter_mut().enumerate() {
                let v = match channels[c].and_then(|s| color.get(s)) {
                    Some(Value::Byte(b)) => *b as f32 / 255.0,
                    _ => 0.0,
                };
                // The base is the colour itself; the second half is how far it may stray.
                *out += if i == 0 { v } else { v * self.frand() };
            }
        }
        Ok([out[0].min(1.0), out[1].min(1.0), out[2].min(1.0)])
    }

    /// Slots of red, green and blue inside the `Color` a colour pair is made of.
    fn color_slots(&mut self, a: ObjectKey, prop: &str) -> VmResult<[Option<usize>; 3]> {
        let class = self.class_of(a)?;
        let n = self.world.names.intern(prop);
        let Some(PropType::Struct(sid)) = self.world.class(class).prop(n).map(|p| p.ty.clone()) else {
            return Ok([None; 3]);
        };
        let pair = self.world.structs[sid.0 as usize].clone();
        let base = pair.fields.iter().find(|f| self.world.names.str(f.name).eq_ignore_ascii_case("Base"));
        let Some(PropType::Struct(cid)) = base.map(|f| f.ty.clone()) else { return Ok([None; 3]) };
        let color = self.world.structs[cid.0 as usize].clone();
        let slot = |want: &str| {
            color.fields.iter().find(|f| self.world.names.str(f.name).eq_ignore_ascii_case(want)).map(|f| f.slot as usize)
        };
        Ok([slot("R"), slot("G"), slot("B")])
    }

    /// Slot of each named field of a struct property, looked up through its type.
    fn struct_slots(&mut self, a: ObjectKey, prop: &str, names: &[&str]) -> VmResult<Vec<Option<usize>>> {
        let class = self.class_of(a)?;
        let n = self.world.names.intern(prop);
        let Some(PropType::Struct(sid)) = self.world.class(class).prop(n).map(|p| p.ty.clone()) else {
            return Ok(vec![None; names.len()]);
        };
        let def = self.world.structs[sid.0 as usize].clone();
        Ok(names
            .iter()
            .map(|want| {
                def.fields.iter().find(|f| self.world.names.str(f.name).eq_ignore_ascii_case(want)).map(|f| f.slot as usize)
            })
            .collect())
    }

    /// Class every emitter of the game descends from, found once.
    fn particle_class(&mut self) -> Option<grim_object::ClassId> {
        if self.particle_class.is_none() {
            self.particle_class = (0..self.world.classes.len())
                .map(|i| grim_object::ClassId(i as u32))
                .find(|&c| self.world.names.str(self.world.class(c).name) == "ParticleFX");
        }
        self.particle_class
    }

    /// Whether an actor is one of the emitters.
    pub fn is_particle_fx(&mut self, a: ObjectKey) -> bool {
        let Some(base) = self.particle_class() else { return false };
        matches!(self.class_of(a), Ok(c) if self.world.is_child_of(c, base))
    }

    /// Runs every emitter of the level for one frame.
    pub fn tick_particles(&mut self, dt: f32, report: &mut crate::level::Report) {
        for a in self.actors.clone() {
            if self.is_deleted(a) || !self.is_particle_fx(a) {
                continue;
            }
            if let Err(e) = self.tick_emitter(a, dt) {
                report.record(e);
            }
        }
        let gone: Vec<ObjectKey> = self.particles.keys().copied().filter(|&a| self.is_deleted(a)).collect();
        for a in gone {
            self.particles.remove(&a);
        }
    }

    fn tick_emitter(&mut self, a: ObjectKey, dt: f32) -> VmResult<()> {
        // `bSteadyState` says the thing has been going on since before anyone arrived: the
        // clouds over Privet Drive are already in the sky when the level opens, and at 0.05
        // particles a second the first one would otherwise take twenty seconds to appear. It is
        // caught up by running it for one lifetime, which is exactly how long it takes for the
        // population to settle: what is emitted in that time is what is alive at any moment
        // afterwards. No number of ours comes into it — the rate and the lifetime are the
        // emitter's own.
        // A burst is not caught up: one that lets out a fixed number (`ParticlesMax`) and has
        // them all out in the first moment would have lived its whole life before its first
        // frame. The Horklumps' poison cloud (`Hork01`, 25 particles, steady state) was spent
        // and gone before it was ever seen. Hypothesis: `bSteadyState` only means something to
        // a system that goes on emitting.
        let burst = matches!(self.get_prop(a, "ParticlesMax"), Ok(Value::Int(m)) if m > 0);
        if !burst && !self.particles.get(&a).is_some_and(|e| e.primed) && matches!(self.get_prop(a, "bSteadyState"), Ok(Value::Bool(true))) {
            let (base, rand) = self.param_pair(a, "Lifetime")?;
            let lifetime = base + rand;
            self.particles.entry(a).or_default().primed = true;
            let mut caught = 0.0;
            while caught < lifetime {
                self.tick_emitter(a, dt)?;
                caught += dt;
            }
        }
        let mut state = self.particles.remove(&a).unwrap_or_default();
        state.age += dt;
        let emitting = matches!(self.get_prop(a, "bEmit")?, Value::Bool(true))
            && !matches!(self.get_prop(a, "bHidden")?, Value::Bool(true))
            && state.age >= float_of(self.get_prop(a, "EmitDelay")?);
        if emitting {
            let rate = self.param(a, "ParticlesPerSec")?;
            state.residue += rate * dt;
            // Hypothesis: `ParticlesMax` is how many the system ever lets out, not how many
            // live at once, so a burst empties itself and stops. `DobbyBurst` and `ChessExplo`
            // both ask for 100 and are a puff and an explosion; what goes on for as long as it
            // is wanted, the aiming sparkle and the clouds, asks for no maximum at all.
            let total = match self.get_prop(a, "ParticlesMax")? {
                Value::Int(m) if m > 0 => m as u32,
                _ => u32::MAX,
            };
            while state.residue >= 1.0 {
                state.residue -= 1.0;
                if state.emitted >= total || state.particles.len() >= MAX_PARTICLES {
                    break;
                }
                let p = self.emit(a)?;
                state.particles.push(p);
                state.emitted += 1;
            }
        }
        self.integrate(a, &mut state, dt)?;
        // What the scripts read back: `ParticleFX.Shutdown` stops a system by capping it at
        // what it has let out (`ParticlesMax = ParticlesEmitted`), and destroys it outright if
        // that is nothing; `SpellCursor` looks at what is alive. Left at zero, every spell's
        // trail was destroyed the moment its spell was, sparks and all.
        let (emitted, alive) = (state.emitted.min(i32::MAX as u32) as i32, state.particles.len() as i32);
        if !matches!(self.get_prop(a, "ParticlesEmitted")?, Value::Int(v) if v == emitted) {
            self.set_prop(a, "ParticlesEmitted", Value::Int(emitted))?;
        }
        if !matches!(self.get_prop(a, "ParticlesAlive")?, Value::Int(v) if v == alive) {
            self.set_prop(a, "ParticlesAlive", Value::Int(alive))?;
        }
        // A system made while playing (a spell's trail, the burst where it hit) that has let
        // out all it may and has nothing left alive is over, and goes. Hypothesis: nothing in
        // the scripts destroys one, and a finished burst left standing is what the player sees
        // as sparks that never go; the ones the map places are kept, since a cutscene may turn
        // them on again.
        let capped = matches!(self.get_prop(a, "ParticlesMax")?, Value::Int(m) if m > 0 && state.emitted >= m as u32);
        let spawned = a.package == crate::vm::TRANSIENT;
        let done = capped && alive == 0 && spawned;
        self.particles.insert(a, state);
        if done {
            self.call_event(a, "Destroyed", vec![])?;
            self.set_prop(a, "bDeleteMe", Value::Bool(true))?;
            self.instance(a)?.deleted = true;
        }
        Ok(())
    }

    /// Makes one particle, where the emitter is and pointing the way it faces.
    fn emit(&mut self, a: ObjectKey) -> VmResult<Particle> {
        let origin = crate::vm::floats3(&self.get_prop(a, "Location")?)?;
        let rotation = crate::vm::ints3(&self.get_prop(a, "Rotation")?)?;
        let axes = crate::natives::core::axes(rotation);
        // Hypothesis: the source is a box around the emitter, measured along the way it faces
        // (depth), across (width) and up (height), and the particle starts anywhere inside it.
        let (depth, width, height) =
            (self.param(a, "SourceDepth")?, self.param(a, "SourceWidth")?, self.param(a, "SourceHeight")?);
        let mut position = origin;
        for (i, size) in [depth, width, height].into_iter().enumerate() {
            let r = self.frand() - 0.5;
            for (c, out) in position.iter_mut().enumerate() {
                *out += axes[i][c] * size * r;
            }
        }
        // Hypothesis: the spreads are the whole angle of the cone the emitter fires into, so a
        // particle leaves within half of one to either side. `SpellTarget` spreads 180 or more
        // both ways and throws its sparkles all around itself.
        let (spread_w, spread_h) = (self.param(a, "AngularSpreadWidth")?, self.param(a, "AngularSpreadHeight")?);
        let deg = std::f32::consts::PI / 180.0;
        let (yaw, pitch) = ((self.frand() - 0.5) * spread_w * deg, (self.frand() - 0.5) * spread_h * deg);
        let (sy, cy) = yaw.sin_cos();
        let (sp, cp) = pitch.sin_cos();
        let mut dir = [0.0; 3];
        for c in 0..3 {
            dir[c] = axes[0][c] * cy * cp + axes[1][c] * sy * cp + axes[2][c] * sp;
        }
        let speed = self.param(a, "Speed")?;
        let mut velocity = [dir[0] * speed, dir[1] * speed, dir[2] * speed];
        if matches!(self.get_prop(a, "bVelocityRelative")?, Value::Bool(true)) {
            let own = crate::vm::floats3(&self.get_prop(a, "Velocity")?)?;
            for c in 0..3 {
                velocity[c] += own[c];
            }
        }
        Ok(Particle {
            position,
            velocity,
            age: 0.0,
            lifetime: self.param(a, "Lifetime")?.max(1e-3),
            color_start: self.color_param(a, "ColorStart")?,
            color_end: self.color_param(a, "ColorEnd")?,
            alpha_start: self.param(a, "AlphaStart")?,
            alpha_end: self.param(a, "AlphaEnd")?,
            width: self.param(a, "SizeWidth")?,
            length: self.param(a, "SizeLength")?,
            end_scale: self.param(a, "SizeEndScale")?,
            spin: self.frand(),
            spin_rate: self.param(a, "SpinRate")?,
            delays: Fade {
                color: float_of(self.get_prop(a, "ColorDelay")?),
                alpha: float_of(self.get_prop(a, "AlphaDelay")?),
                size: float_of(self.get_prop(a, "SizeDelay")?),
            },
            alpha_grow: float_of(self.get_prop(a, "AlphaGrowPeriod")?),
            size_grow: float_of(self.get_prop(a, "SizeGrowPeriod")?),
        })
    }

    /// Ages every particle of an emitter and moves it.
    fn integrate(&mut self, a: ObjectKey, state: &mut Emitter, dt: f32) -> VmResult<()> {
        let gravity = match self.get_prop(a, "Gravity") {
            Ok(v) => crate::vm::floats3(&v).unwrap_or([0.0; 3]),
            Err(_) => [0.0; 3],
        };
        let gravity_scale = float_of(self.get_prop(a, "GravityModifier")?);
        let attraction = match self.get_prop(a, "Attraction") {
            Ok(v) => crate::vm::floats3(&v).unwrap_or([0.0; 3]),
            Err(_) => [0.0; 3],
        };
        let damping = float_of(self.get_prop(a, "Damping")?);
        let chaos = float_of(self.get_prop(a, "Chaos")?);
        let chaos_delay = float_of(self.get_prop(a, "ChaosDelay")?);
        // Hypothesis: damping is how fast the speed falls away, so a particle keeps
        // `exp(-Damping * dt)` of it each frame and never turns around.
        let keep = (-damping * dt).exp();
        let mut rolls = Vec::new();
        for _ in 0..state.particles.len() * 3 {
            rolls.push(self.frand() - 0.5);
        }
        let mut roll = rolls.into_iter();
        for p in &mut state.particles {
            p.age += dt;
            p.spin += p.spin_rate * dt;
            for c in 0..3 {
                p.velocity[c] += (gravity[c] * gravity_scale + attraction[c]) * dt;
                if chaos > 0.0 && p.age >= chaos_delay {
                    p.velocity[c] += roll.next().unwrap_or(0.0) * chaos * dt * 100.0;
                }
                p.velocity[c] *= keep;
                p.position[c] += p.velocity[c] * dt;
            }
        }
        state.particles.retain(|p| p.age < p.lifetime);
        Ok(())
    }
}

fn float_of(v: Value) -> f32 {
    match v {
        Value::Float(f) => f,
        Value::Int(i) => i as f32,
        Value::Byte(b) => b as f32,
        _ => 0.0,
    }
}
