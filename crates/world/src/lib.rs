//! Level geometry: the BSP of a `Engine.Model`, point queries and line traces.

pub mod anim;
pub mod lightmap;
pub mod procedural;
pub mod scene;

use grim_assets::model::Model;
 

pub const INDEX_NONE: i32 = -1;

pub type Vec3 = [f32; 3];

pub fn dot(a: Vec3, b: Vec3) -> f32 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

pub fn add(a: Vec3, b: Vec3) -> Vec3 {
    [a[0] + b[0], a[1] + b[1], a[2] + b[2]]
}

pub fn sub(a: Vec3, b: Vec3) -> Vec3 {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}

pub fn scale(a: Vec3, k: f32) -> Vec3 {
    [a[0] * k, a[1] * k, a[2] * k]
}

pub fn lerp(a: Vec3, b: Vec3, t: f32) -> Vec3 {
    add(a, scale(sub(b, a), t))
}

#[derive(Debug, Clone, Copy)]
pub struct Node {
    pub normal: Vec3,
    pub dist: f32,
    pub front: i32,
    pub back: i32,
    /// Leaf index per side (see `FRONT`/`BACK`); `INDEX_NONE` when that side is not a leaf.
    pub leaf: [i32; 2],
    /// Zone number per side.
    pub zone: [u8; 2],
    pub surf: i32,
    pub flags: u8,
}

/// Side indices of a node's per-side arrays (`leaf`, `zone`): 1 is front, 0 is back.
/// Verified against the `Region` the editor stored in every actor.
pub const FRONT: usize = 1;
pub const BACK: usize = 0;

pub struct Bsp {
    pub nodes: Vec<Node>,
    /// Whether the model was built with leaves, which says how solidity is read.
    leaves: bool,
    /// Poly flags per surface, which is what a trace looks up about what it hit.
    pub surf_flags: Vec<u32>,
}

impl Bsp {
    pub fn new(model: &Model) -> Self {
        let nodes = model
            .nodes
            .iter()
            .map(|n| Node {
                normal: [n.plane.x, n.plane.y, n.plane.z],
                dist: n.plane.w,
                front: n.i_front,
                back: n.i_back,
                leaf: n.i_leaf,
                zone: n.i_zone,
                surf: n.i_surf,
                flags: n.node_flags,
            })
            .collect();
        Self { nodes, leaves: !model.leaves.is_empty(), surf_flags: model.surfs.iter().map(|s| s.poly_flags).collect() }
    }

    /// The flags of the surface a hit is against, if it is one of the level's own.
    pub fn flags_of(&self, hit: &Hit) -> u32 {
        usize::try_from(hit.surf).ok().and_then(|i| self.surf_flags.get(i)).copied().unwrap_or(0)
    }

    pub fn is_empty(&self) -> bool {
        self.nodes.is_empty()
    }

    /// Leaf and zone containing `p`, following the BSP down to a missing child.
    pub fn point_region(&self, p: Vec3) -> (i32, u8) {
        let mut i = 0i32;
        let mut guard = 0;
        while i != INDEX_NONE {
            let n = &self.nodes[i as usize];
            let side = if dot(n.normal, p) - n.dist >= 0.0 { FRONT } else { BACK };
            let child = if side == FRONT { n.front } else { n.back };
            if child == INDEX_NONE {
                return (n.leaf[side], n.zone[side]);
            }
            i = child;
            guard += 1;
            if guard > self.nodes.len() {
                break;
            }
        }
        (INDEX_NONE, 0)
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Hit {
    /// Fraction of the segment at which the hit happens.
    pub time: f32,
    pub location: Vec3,
    /// Surface normal, pointing back towards the segment's start side.
    pub normal: Vec3,
    pub node: i32,
    pub surf: i32,
}

/// A half-space with no child and no leaf is solid; with a leaf it is open space. The brushes
/// the movers carry have no leaves at all — leaves belong to a built level — so for those the
/// rule is the one the tree is cut with: what lies behind a face with nothing behind it is the
/// inside of the brush.
fn solid(node: &Node, side: usize, leaves: bool) -> bool {
    if leaves {
        node.leaf[side] == INDEX_NONE
    } else {
        side == BACK
    }
}

impl Bsp {
    /// Whether `p` is inside solid geometry.
    pub fn point_solid(&self, p: Vec3) -> bool {
        let mut i = 0i32;
        let mut guard = 0;
        while let Some(n) = usize::try_from(i).ok().and_then(|k| self.nodes.get(k)) {
            let side = if dot(n.normal, p) - n.dist >= 0.0 { FRONT } else { BACK };
            let child = if side == FRONT { n.front } else { n.back };
            if child == INDEX_NONE {
                return solid(n, side, self.leaves);
            }
            i = child;
            guard += 1;
            if guard > self.nodes.len() {
                break;
            }
        }
        false
    }

    /// First crossing from open space into solid geometry along `start`..`end`.
    /// The leaf a point falls in, and the zone that leaf belongs to. Walking down the tree is
    /// how the engine knows where the eye is, and from the leaf comes what it can see.
    pub fn leaf_at(&self, point: Vec3) -> Option<(i32, u8)> {
        if self.nodes.is_empty() {
            return None;
        }
        let mut index = 0i32;
        for _ in 0..4096 {
            let node = self.nodes[index as usize];
            let side = if dot(node.normal, point) - node.dist >= 0.0 { FRONT } else { BACK };
            let next = if side == FRONT { node.front } else { node.back };
            if next < 0 {
                return Some((node.leaf[side], node.zone[side]));
            }
            index = next;
        }
        None
    }

    pub fn line_check(&self, start: Vec3, end: Vec3) -> Option<Hit> {
        if self.nodes.is_empty() {
            return None;
        }
        self.check(0, start, end, 0.0, 1.0, 0)
    }

    fn check(&self, index: i32, p0: Vec3, p1: Vec3, t0: f32, t1: f32, depth: u32) -> Option<Hit> {
        if depth > 4096 {
            return None;
        }
        let n = self.nodes[index as usize];
        let d0 = dot(n.normal, p0) - n.dist;
        let d1 = dot(n.normal, p1) - n.dist;
        let side = |d: f32| if d >= 0.0 { FRONT } else { BACK };
        let child = |s: usize| if s == FRONT { n.front } else { n.back };
        let hit = |t: f32, p: Vec3, s: usize| Hit {
            time: t,
            location: p,
            normal: if s == FRONT { n.normal } else { scale(n.normal, -1.0) },
            node: index,
            surf: n.surf,
        };

        // A segment that runs along a plane never crosses it, however the rounding falls: what
        // it meets is whatever is on the side it is on. Without this an actor standing where a
        // plane passes through it has every trace stopped by that plane — a shadow thrown down
        // a staircase catches the face of a step instead of reaching the ground.
        const PARALLEL: f32 = 1e-3;
        if (d0 >= 0.0) == (d1 >= 0.0) || (d0 - d1).abs() < PARALLEL {
            let s = side(d0);
            return match child(s) {
                INDEX_NONE if solid(&n, s, self.leaves) => Some(hit(t0, p0, s)),
                INDEX_NONE => None,
                c => self.check(c, p0, p1, t0, t1, depth + 1),
            };
        }

        // The segment crosses the plane: near half first, then the far half.
        let f = d0 / (d0 - d1);
        let tm = t0 + (t1 - t0) * f;
        let mid = lerp(p0, p1, f);
        let (near, far) = (side(d0), side(d1));
        match child(near) {
            INDEX_NONE if solid(&n, near, self.leaves) => return Some(hit(t0, p0, near)),
            INDEX_NONE => {}
            c => {
                if let Some(h) = self.check(c, p0, mid, t0, tm, depth + 1) {
                    return Some(h);
                }
            }
        }
        match child(far) {
            // Entering solid space: the hit is on this node's plane.
            INDEX_NONE if solid(&n, far, self.leaves) => Some(hit(tm, mid, near)),
            INDEX_NONE => None,
            // What the far half answers is kept, except when it says the segment was already
            // in solid space where it started: that start is this plane's crossing, so the
            // surface met is this plane and not whatever node the far half stopped at.
            c => self.check(c, mid, p1, tm, t1, depth + 1).map(|h| if h.time <= tm { hit(tm, mid, near) } else { h }),
        }
    }
}

impl Bsp {
    /// Whether an axis-aligned box centred at `at` runs into solid geometry, which is what a
    /// teleport asks before it drops an actor somewhere.
    ///
    /// A box that straddles a node's plane is on both sides of it, so both halves are walked;
    /// pushing the plane out and picking one side, the way a trace does, would follow the tree
    /// down the wrong branch.
    pub fn box_solid(&self, at: Vec3, extent: Vec3) -> bool {
        !self.nodes.is_empty() && self.solid_around(0, at, extent, 0)
    }

    fn solid_around(&self, index: i32, p: Vec3, ext: Vec3, depth: u32) -> bool {
        if depth > 4096 {
            return false;
        }
        let n = self.nodes[index as usize];
        // A hair less than the extent, so an actor resting exactly on the floor still fits.
        let push = ext[0] * n.normal[0].abs() + ext[1] * n.normal[1].abs() + ext[2] * n.normal[2].abs() - 0.1;
        let d = dot(n.normal, p) - n.dist;
        let mut sides = [(FRONT, d >= -push, n.front), (BACK, d <= push, n.back)];
        sides.sort_by_key(|s| !s.1);
        for (side, reached, child) in sides {
            if !reached {
                continue;
            }
            let hit = match child {
                INDEX_NONE => solid(&n, side, self.leaves),
                c => self.solid_around(c, p, ext, depth + 1),
            };
            if hit {
                return true;
            }
        }
        false
    }
}

/// Trace of an axis-aligned box: every plane is pushed out by the box's extent, which is the
/// standard Minkowski expansion for axis-aligned boxes.
impl Bsp {
    pub fn box_check(&self, start: Vec3, end: Vec3, extent: Vec3) -> Option<Hit> {
        if self.nodes.is_empty() {
            return None;
        }
        if extent == [0.0; 3] {
            return self.line_check(start, end);
        }
        // Nothing has been crossed yet: a box that is already in solid space reports the
        // surface it would have had to come through, which is the one facing its travel.
        let back = sub(start, end);
        let length = (back[0] * back[0] + back[1] * back[1] + back[2] * back[2]).sqrt();
        let entry = if length > 1e-6 { scale(back, 1.0 / length) } else { [0.0, 0.0, 1.0] };
        self.check_extent(0, start, end, 0.0, 1.0, extent, 0, entry)
    }

    /// The box crosses a plane over a stretch of the segment, not at a point: it starts leaving
    /// one side `extent` before the plane and has fully entered the other `extent` after it, so
    /// both halves are walked over the stretch where the box still reaches into them.
    /// `entry` is the surface the box came through to reach this part of the tree. A solid
    /// leaf found without crossing this node's plane was entered somewhere further up, so that
    /// is the surface the hit is against: the plane the tree happens to be split on here says
    /// nothing about what was run into.
    fn check_extent(&self, index: i32, p0: Vec3, p1: Vec3, t0: f32, t1: f32, ext: Vec3, depth: u32, entry: Vec3) -> Option<Hit> {
        if depth > 4096 {
            return None;
        }
        let n = self.nodes[index as usize];
        // The same hair `solid_around` leaves, so both queries agree about a box that is only
        // touching a surface. Without it a box resting against a wall is reported as hitting
        // it before it has moved, while the other query says it has room: nothing then gets it
        // away from the wall, because the move is refused and there is nothing to escape from.
        let off = (ext[0] * n.normal[0].abs() + ext[1] * n.normal[1].abs() + ext[2] * n.normal[2].abs() - 0.1).max(0.0);
        let d0 = dot(n.normal, p0) - n.dist;
        let d1 = dot(n.normal, p1) - n.dist;
        let child = |s: usize| if s == FRONT { n.front } else { n.back };
        let half = |s: usize, a: Vec3, b: Vec3, ta: f32, tb: f32, normal: Vec3| match child(s) {
            INDEX_NONE if solid(&n, s, self.leaves) => {
                Some(Hit { time: ta, location: a, normal, node: index, surf: n.surf })
            }
            INDEX_NONE => None,
            c => self.check_extent(c, a, b, ta, tb, ext, depth + 1, normal),
        };
        let front_normal = n.normal;
        let back_normal = scale(n.normal, -1.0);

        // Clear of the plane for the whole segment: only that side matters, and it is still on
        // the side it came in on, so what it would run into there is what it came through.
        if d0 >= off && d1 >= off {
            return half(FRONT, p0, p1, t0, t1, entry);
        }
        if d0 <= -off && d1 <= -off {
            return half(BACK, p0, p1, t0, t1, entry);
        }

        let (near, far) = if d0 >= d1 { (FRONT, BACK) } else { (BACK, FRONT) };
        let denom = d0 - d1;
        let (enter, leave) = if denom.abs() < 1e-8 {
            (0.0, 1.0)
        } else if near == FRONT {
            (((d0 - off) / denom).clamp(0.0, 1.0), ((d0 + off) / denom).clamp(0.0, 1.0))
        } else {
            (((d0 + off) / denom).clamp(0.0, 1.0), ((d0 - off) / denom).clamp(0.0, 1.0))
        };
        let at = |f: f32| (lerp(p0, p1, f), t0 + (t1 - t0) * f);
        let (p_leave, t_leave) = at(leave);
        let (p_enter, t_enter) = at(enter);
        let normals = |s: usize| if s == FRONT { front_normal } else { back_normal };
        // A box that lies across a plane and travels along it never comes through that plane,
        // so it is not what it runs into: what stopped it is whatever it did come through.
        // Without this a box falling alongside a wall is answered by the wall, and a fall that
        // only wanted to go down is refused by a surface it is not moving towards at all.
        let delta = sub(p1, p0);
        let reach = (delta[0] * delta[0] + delta[1] * delta[1] + delta[2] * delta[2]).sqrt();
        let crossing = dot(delta, normals(near)) < -1e-3 * reach;
        let plane = if crossing { normals(near) } else { entry };

        // The half it is leaving, for as long as the box still reaches into it. It has not
        // crossed anything to be there, so a hit is against whatever let it in.
        if let Some(h) = half(near, p0, p_leave, t0, t_leave, entry) {
            return h.into();
        }
        // Then the half it enters, from where the box first touches it.
        half(far, p_enter, p1, t_enter, t1, plane)
    }
}

/// Segment against an actor's collision cylinder (radius on X/Y, half height on Z), optionally
/// expanded by `extent`. Returns the entry time and the surface normal.
/// A moving box against a still one, both axis aligned: the mover is turned into a point by
/// growing the target by its own half extents, and the segment is clipped against the slabs.
/// Gives how far along the segment it meets it and the face it meets.
pub fn box_check(start: Vec3, end: Vec3, center: Vec3, half: Vec3, extent: Vec3) -> Option<(f32, Vec3)> {
    let grown = [0, 1, 2].map(|i| half[i] + extent[i]);
    let d = sub(end, start);
    let o = sub(start, center);
    let (mut enter, mut exit) = (0.0f32, 1.0f32);
    let mut axis = 0usize;
    for i in 0..3 {
        if d[i].abs() < 1e-6 {
            if o[i].abs() > grown[i] {
                return None;
            }
            continue;
        }
        let (mut t1, mut t2) = ((-grown[i] - o[i]) / d[i], (grown[i] - o[i]) / d[i]);
        if t1 > t2 {
            std::mem::swap(&mut t1, &mut t2);
        }
        if t1 > enter {
            enter = t1;
            axis = i;
        }
        exit = exit.min(t2);
        if enter > exit {
            return None;
        }
    }
    if enter > exit || enter >= 1.0 {
        return None;
    }
    // Already inside it at the start: nothing to report, or the actor would be thrown out.
    if enter <= 0.0 && (0..3).all(|i| o[i].abs() <= grown[i]) {
        return None;
    }
    let mut normal = [0.0; 3];
    normal[axis] = if o[axis] < 0.0 { -1.0 } else { 1.0 };
    Some((enter.max(0.0), normal))
}

/// A moving box against one that has been turned about its up axis, which is how the game
/// places the long thin boxes it blocks the way with. The segment is taken into the box's own
/// frame, tested there, and what it met is turned back.
///
/// The mover keeps its own axis-aligned extent in that frame, which is a little larger than it
/// really is when the two are at an angle; a box that fits through a gap by less than that
/// will be stopped. Turning it properly means an oriented-box sweep, which nothing here needs
/// yet.
pub fn box_check_turned(start: Vec3, end: Vec3, center: Vec3, half: Vec3, extent: Vec3, yaw: f32) -> Option<(f32, Vec3)> {
    if yaw == 0.0 {
        return box_check(start, end, center, half, extent);
    }
    let (sin, cos) = (-yaw).sin_cos();
    let into = |p: Vec3| {
        let d = sub(p, center);
        [center[0] + d[0] * cos - d[1] * sin, center[1] + d[0] * sin + d[1] * cos, p[2]]
    };
    let (time, normal) = box_check(into(start), into(end), center, half, extent)?;
    let (sin, cos) = yaw.sin_cos();
    Some((time, [normal[0] * cos - normal[1] * sin, normal[0] * sin + normal[1] * cos, normal[2]]))
}

pub fn cylinder_check(start: Vec3, end: Vec3, center: Vec3, radius: f32, half_height: f32, extent: Vec3) -> Option<(f32, Vec3)> {
    let (r, h) = (radius + extent[0].max(extent[1]), half_height + extent[2]);
    let d = sub(end, start);
    let o = sub(start, center);

    // Z slab.
    let (mut enter, mut exit, mut enter_axis) = (0.0f32, 1.0f32, false);
    if d[2].abs() < 1e-6 {
        if o[2].abs() > h {
            return None;
        }
    } else {
        let (mut t1, mut t2) = ((-h - o[2]) / d[2], (h - o[2]) / d[2]);
        if t1 > t2 {
            std::mem::swap(&mut t1, &mut t2);
        }
        if t1 > enter {
            enter = t1;
            enter_axis = true;
        }
        exit = exit.min(t2);
    }

    // Radial quadratic.
    let (a, b, c) = (d[0] * d[0] + d[1] * d[1], 2.0 * (o[0] * d[0] + o[1] * d[1]), o[0] * o[0] + o[1] * o[1] - r * r);
    if a < 1e-9 {
        if c > 0.0 {
            return None;
        }
    } else {
        let disc = b * b - 4.0 * a * c;
        if disc < 0.0 {
            return None;
        }
        let sq = disc.sqrt();
        let (t1, t2) = ((-b - sq) / (2.0 * a), (-b + sq) / (2.0 * a));
        if t1 > enter {
            enter = t1;
            enter_axis = false;
        }
        exit = exit.min(t2);
    }
    if enter > exit || enter > 1.0 || exit < 0.0 {
        return None;
    }
    let p = lerp(start, end, enter);
    let normal = if enter_axis {
        [0.0, 0.0, if p[2] > center[2] { 1.0 } else { -1.0 }]
    } else {
        let n = [p[0] - center[0], p[1] - center[1], 0.0];
        let l = dot(n, n).sqrt();
        if l > 1e-6 { scale(n, 1.0 / l) } else { [1.0, 0.0, 0.0] }
    };
    Some((enter.max(0.0), normal))
}
