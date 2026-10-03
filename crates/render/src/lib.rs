//! Rendering interface. Backends (wgpu, WebGL, ...) implement [`Renderer`]; the engine only
//! ever talks to this crate.

pub mod math;

use math::{Mat4, Vec3};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TextureId(pub u32);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct MeshId(pub u32);

/// RGBA8 image data.
pub struct TextureData<'a> {
    pub width: u32,
    pub height: u32,
    pub rgba: &'a [u8],
    /// The smaller copies the game ships with the picture, each half the size of the one
    /// before. Without them a surface seen at a slant turns to noise, because every pixel of
    /// it reaches for texels far apart.
    pub mips: &'a [Vec<u8>],
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Vertex {
    pub position: Vec3,
    pub uv: [f32; 2],
    pub normal: Vec3,
    /// Baked light reaching this vertex.
    pub color: [f32; 3],
    /// Coordinates in the level's lightmap atlas.
    pub uv2: [f32; 2],
}

/// A light reaching a mesh. Meshes have no lightmap: Unreal shades them from the lights around
/// them, by the angle of each vertex.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct MeshLight {
    /// Unit vector from the mesh towards the light.
    pub direction: Vec3,
    pub color: [f32; 3],
}

/// Lights shading one mesh at a time; the rest of them light it evenly.
pub const MESH_LIGHTS: usize = 3;

/// How a surface blends with what is already drawn, following Unreal's draw styles.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Default)]
pub enum Blend {
    #[default]
    Opaque,
    /// Adds the texture's colour: black is invisible.
    Translucent,
    /// Multiplies by two: mid grey leaves the background unchanged.
    Modulated,
}

/// A run of indices drawn with one texture.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Section {
    pub first_index: u32,
    pub index_count: u32,
    pub texture: Option<TextureId>,
    /// Discard texels whose alpha is below 0.5 (Unreal's masked textures).
    pub masked: bool,
    /// Material index inside the mesh, which an actor's skins can override.
    pub material: u32,
    /// Draw both faces (Unreal's `PF_TwoSided`, used by sheets such as foliage).
    pub two_sided: bool,
    pub blend: Blend,
    /// Unreal's `PF_AutoUPan`/`PF_AutoVPan`: the texture slides over the surface on its own, in
    /// uv per second. The zone says how fast (`ZoneInfo.TexUPanSpeed`, `TexVPanSpeed`).
    pub pan: [f32; 2],
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct MeshData {
    pub vertices: Vec<Vertex>,
    pub indices: Vec<u32>,
    pub sections: Vec<Section>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Camera {
    pub position: Vec3,
    /// Unreal rotator (pitch, yaw, roll).
    pub rotation: [i32; 3],
    pub fov_y_deg: f32,
    pub near: f32,
    pub far: f32,
}

impl Default for Camera {
    fn default() -> Self {
        Self { position: [0.0; 3], rotation: [0; 3], fov_y_deg: 75.0, near: 1.0, far: 65536.0 }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DrawItem<'a> {
    pub mesh: MeshId,
    pub transform: Mat4,
    /// Overrides the per-section alpha test (Unreal decides masking per actor, via `Style`).
    pub alpha_test: Option<bool>,
    /// Per-material texture overrides (Unreal's `Skin` and `MultiSkins`).
    pub skins: &'a [Option<TextureId>],
    /// Overrides the per-section blend (an actor's `Style` applies to its whole mesh).
    pub blend: Option<Blend>,
    /// Light the item receives from every direction.
    pub tint: [f32; 3],
    /// Lights shading the item by the angle of each vertex, softly: a face turned away from a
    /// light still takes half of it, which is how Unreal's meshes look.
    pub lights: [MeshLight; MESH_LIGHTS],
    /// Unreal's `bMeshEnviroMap`: the texture is wrapped around the mesh by its normals,
    /// which is how shiny surfaces such as ectoplasm are drawn.
    pub env_map: bool,
    /// Lightmap atlas sampled with `uv2`. Lightmaps modulate a surface at twice their value;
    /// an item without one is lit by `tint` alone, at face value.
    pub lightmap: Option<TextureId>,
}

/// A rectangle drawn over the finished scene, in pixels from the top left: what the game's
/// menus, HUD and text are made of.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Quad {
    /// Screen rectangle: x, y, width, height.
    pub rect: [f32; 4],
    /// Texture rectangle in 0..1: u, v, width, height.
    pub uv: [f32; 4],
    pub texture: Option<TextureId>,
    pub color: [f32; 4],
    /// Discard the texture's transparent texels instead of blending them.
    pub masked: bool,
    /// How it goes over what is already drawn, from the canvas style.
    pub blend: Blend,
}

/// A sprite standing in the world, always turned to face the viewer: what a particle is.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Sprite {
    /// Drawn over whatever is in front of it, for a glow whose being hidden is decided by a
    /// trace rather than by the depth buffer: a lamp's own glass would hide it otherwise.
    pub through: bool,
    pub position: Vec3,
    /// Width and height in world units.
    pub size: [f32; 2],
    /// Turn around the line of sight, in turns.
    pub spin: f32,
    pub color: [f32; 4],
    pub texture: Option<TextureId>,
    pub blend: Blend,
    /// A decal does not turn to the camera: it lies on the surface it was thrown at, and this
    /// is that surface's normal. `None` for the sprites that face the eye.
    pub facing: Option<Vec3>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Scene<'a> {
    pub camera: Camera,
    pub items: Vec<DrawItem<'a>>,
    pub clear_color: [f32; 4],
    /// Display gamma applied to the finished frame, as Unreal's render devices did.
    pub gamma: f32,
    /// Seconds the level has been running, for the surfaces whose texture slides on its own.
    pub time: f32,
    /// Drawn over the scene, in the order the scripts asked for.
    pub overlay: Vec<Quad>,
    /// The backdrop, drawn before the world from its own point of view: Unreal keeps the sky
    /// as a separate piece of the level and looks at it from the sky zone's own actor.
    pub sky: Option<(DrawItem<'a>, Vec3)>,
    /// The surfaces the sky shows through, drawn into the depth buffer only: they hide what
    /// the level keeps beyond them without covering the backdrop.
    pub backdrop: Option<DrawItem<'a>>,
    /// Mirrors: each one draws the level reflected about its own plane `(normal, distance)`.
    pub mirrors: Vec<(DrawItem<'a>, [f32; 4])>,
    /// Particles, drawn after the world so they blend over it.
    pub sprites: Vec<Sprite>,
    /// Meshes drawn over the finished world, whatever is in front of them: what the game's own
    /// `Canvas.DrawActor` puts on the screen.
    pub foreground: Vec<DrawItem<'a>>,
    /// Unreal's screen flash over the finished world, before what the game draws over it: the
    /// world comes out as `world × flash[0] + flash[1..4]`. `[1, 0, 0, 0]` leaves it alone.
    pub flash: [f32; 4],
}

impl Default for Scene<'_> {
    fn default() -> Self {
        Self {
            camera: Camera::default(),
            items: Vec::new(),
            clear_color: [0.05, 0.05, 0.07, 1.0],
            gamma: 1.0,
            time: 0.0,
            overlay: Vec::new(),
            sky: None,
            backdrop: None,
            mirrors: Vec::new(),
            sprites: Vec::new(),
            foreground: Vec::new(),
            flash: [1.0, 0.0, 0.0, 0.0],
        }
    }
}

/// An image read back from a renderer.
pub struct Image {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

pub trait Renderer {
    fn create_texture(&mut self, data: TextureData<'_>) -> TextureId;
    /// Replaces a texture's pixels, for the ones the engine draws itself every frame.
    fn update_texture(&mut self, texture: TextureId, data: TextureData<'_>);
    fn create_mesh(&mut self, data: &MeshData) -> MeshId;
    /// Replaces a mesh's geometry, for the ones that move every frame.
    fn update_mesh(&mut self, mesh: MeshId, data: &MeshData);
    /// Drops every mesh, for when a whole level is left behind. Ids handed out before this
    /// stop being valid.
    fn clear_meshes(&mut self);
    fn resize(&mut self, width: u32, height: u32);
    fn render(&mut self, scene: &Scene<'_>) -> Result<(), String>;
    /// Reads the last rendered frame back, when the backend supports it.
    fn capture(&mut self) -> Option<Image> {
        None
    }
    /// The textures the last frame drew with, when the backend keeps track: a texture that
    /// draws itself (fire, water) only has to move on while something shows it. `None` means
    /// the backend cannot say, and everything is taken as shown.
    fn textures_drawn(&self) -> Option<&std::collections::HashSet<TextureId>> {
        None
    }
}
