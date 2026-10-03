//! Small matrix helpers. Unreal space: X forward, Y right, Z up, angles in 1/65536 turns.

pub type Vec3 = [f32; 3];
/// Column-major 4x4 matrix, as graphics APIs expect it.
pub type Mat4 = [[f32; 4]; 4];

pub const IDENTITY: Mat4 = [
    [1.0, 0.0, 0.0, 0.0],
    [0.0, 1.0, 0.0, 0.0],
    [0.0, 0.0, 1.0, 0.0],
    [0.0, 0.0, 0.0, 1.0],
];

pub fn mul(a: Mat4, b: Mat4) -> Mat4 {
    let mut out = [[0.0; 4]; 4];
    for (c, col) in out.iter_mut().enumerate() {
        for (r, v) in col.iter_mut().enumerate() {
            *v = (0..4).map(|k| a[k][r] * b[c][k]).sum();
        }
    }
    out
}

pub fn dot(a: Vec3, b: Vec3) -> f32 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

pub fn sub(a: Vec3, b: Vec3) -> Vec3 {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}

pub fn add(a: Vec3, b: Vec3) -> Vec3 {
    [a[0] + b[0], a[1] + b[1], a[2] + b[2]]
}

pub fn scale(a: Vec3, k: f32) -> Vec3 {
    [a[0] * k, a[1] * k, a[2] * k]
}

/// Forward, right and up axes of an Unreal rotator (pitch, yaw, roll).
pub fn axes(rot: [i32; 3]) -> [Vec3; 3] {
    let a = |u: i32| u as f32 * std::f32::consts::PI / 32768.0;
    let (sp, cp) = a(rot[0]).sin_cos();
    let (sy, cy) = a(rot[1]).sin_cos();
    let (sr, cr) = a(rot[2]).sin_cos();
    [
        [cp * cy, cp * sy, sp],
        [sr * sp * cy - cr * sy, sr * sp * sy + cr * cy, -sr * cp],
        [-(cr * sp * cy + sr * sy), cy * sr - cr * sp * sy, cr * cp],
    ]
}

/// World to view: x right, y up, z forward.
pub fn view(position: Vec3, rotation: [i32; 3]) -> Mat4 {
    let [f, r, u] = axes(rotation);
    [
        [r[0], u[0], f[0], 0.0],
        [r[1], u[1], f[1], 0.0],
        [r[2], u[2], f[2], 0.0],
        [-dot(r, position), -dot(u, position), -dot(f, position), 1.0],
    ]
}

/// Perspective with depth in `[0, 1]` (wgpu convention) looking down +z.
pub fn perspective(fov_y_deg: f32, aspect: f32, near: f32, far: f32) -> Mat4 {
    let f = 1.0 / (fov_y_deg.to_radians() / 2.0).tan();
    [
        [f / aspect, 0.0, 0.0, 0.0],
        [0.0, f, 0.0, 0.0],
        [0.0, 0.0, far / (far - near), 1.0],
        [0.0, 0.0, -far * near / (far - near), 0.0],
    ]
}

/// Model matrix for geometry already in world axes, such as a brush actor's polygons: only
/// rotated and moved, with no mirroring.
pub fn place(position: Vec3, rotation: [i32; 3]) -> Mat4 {
    let [f, r, u] = axes(rotation);
    [
        [f[0], f[1], f[2], 0.0],
        [r[0], r[1], r[2], 0.0],
        [u[0], u[1], u[2], 0.0],
        [position[0], position[1], position[2], 1.0],
    ]
}

/// Model matrix from an Unreal placement.
///
/// Mesh vertices are stored mirrored along Y with respect to world space, so the matrix flips
/// that axis back. Checked on light shafts (which then enter through their window) and on
/// closed props such as chests, whose faces then wind like the BSP's.
pub fn transform(position: Vec3, rotation: [i32; 3], scale3: Vec3) -> Mat4 {
    let [f, r, u] = axes(rotation);
    [
        [f[0] * scale3[0], f[1] * scale3[0], f[2] * scale3[0], 0.0],
        [-r[0] * scale3[1], -r[1] * scale3[1], -r[2] * scale3[1], 0.0],
        [u[0] * scale3[2], u[1] * scale3[2], u[2] * scale3[2], 0.0],
        [position[0], position[1], position[2], 1.0],
    ]
}

/// Mirrors world space about a plane `(normal, distance)`: `p - 2 (n·p - d) n`.
pub fn reflection(plane: [f32; 4]) -> Mat4 {
    let n = [plane[0], plane[1], plane[2]];
    let d = plane[3];
    let e = |i: usize, j: usize| if i == j { 1.0 } else { 0.0 } - 2.0 * n[i] * n[j];
    [
        [e(0, 0), e(1, 0), e(2, 0), 0.0],
        [e(0, 1), e(1, 1), e(2, 1), 0.0],
        [e(0, 2), e(1, 2), e(2, 2), 0.0],
        [2.0 * d * n[0], 2.0 * d * n[1], 2.0 * d * n[2], 1.0],
    ]
}
