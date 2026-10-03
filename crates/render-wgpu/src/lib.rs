//! wgpu backend. Renders either to a window surface or offscreen into an image.

use std::collections::HashMap;

use grim_render::math::{self, Mat4};
use grim_render::{Blend, Image, MeshData, MeshId, MeshLight, Quad, Renderer, Scene, Section, TextureData, TextureId, MESH_LIGHTS};
use wgpu::util::DeviceExt;

const DEPTH_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Depth32Float;
/// The scene is drawn in the art's own colour space: Unreal lit and blended 8 bit textures
/// directly, with no linear conversion, and the gamma copy is what reaches the screen.
const SCENE_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8Unorm;
/// Bytes of one overlay rectangle's instance data.
const QUAD_SIZE: u64 = 64;
/// Bytes of one particle: where it is and its turn, its size, its colour.
const SPRITE_SIZE: u64 = 64;

/// Bytes of one `Globals` block, padded to the uniform offset alignment.
const GLOBALS_STRIDE: u64 = 512;

struct Mesh {
    vertices: wgpu::Buffer,
    indices: wgpu::Buffer,
    sections: Vec<Section>,
}

struct Texture {
    bind_group: wgpu::BindGroup,
    /// Kept so the engine can rewrite the pixels of the textures it draws itself.
    texture: wgpu::Texture,
    width: u32,
    height: u32,
}

enum Target {
    #[cfg_attr(target_arch = "wasm32", allow(dead_code))]
    Offscreen { color: wgpu::Texture, width: u32, height: u32 },
    Surface { surface: wgpu::Surface<'static>, config: wgpu::SurfaceConfiguration },
}

pub struct WgpuRenderer {
    device: wgpu::Device,
    queue: wgpu::Queue,
    target: Target,
    format: wgpu::TextureFormat,
    depth: wgpu::TextureView,
    /// Pipelines by (blend, two sided).
    pipelines: HashMap<(Blend, bool), wgpu::RenderPipeline>,
    /// The same, for the backdrop: no depth, so the world always draws over it.
    sky_pipelines: HashMap<(Blend, bool), wgpu::RenderPipeline>,
    backdrop_pipeline: wgpu::RenderPipeline,
    /// The sky and the windows onto it as a mirror shows them: reflected, so turned inside out.
    mirrored_sky_pipelines: HashMap<(Blend, bool), wgpu::RenderPipeline>,
    mirrored_backdrop_pipeline: wgpu::RenderPipeline,
    mirrored_pipelines: HashMap<(Blend, bool), wgpu::RenderPipeline>,
    mirror_pipeline: wgpu::RenderPipeline,
    /// What a mirror shows: the scene drawn from the reflected point of view.
    reflection: wgpu::TextureView,
    reflection_group: wgpu::BindGroup,
    reflection_depth: wgpu::TextureView,
    globals: wgpu::Buffer,
    globals_group: wgpu::BindGroup,
    texture_layout: wgpu::BindGroupLayout,
    /// Overlay pipelines by how a rectangle blends.
    overlay: HashMap<Blend, wgpu::RenderPipeline>,
    overlay_instances: wgpu::Buffer,
    /// Particle pipelines by how a sprite blends, and the buffer of the frame's sprites.
    /// How much a lightmap brightens what it lights: two is the usual, with half the neutral.
    lightmap_scale: f32,
    sprites: HashMap<Blend, wgpu::RenderPipeline>,
    /// The same, for the sprites drawn over what is in front of them.
    sprites_through: HashMap<Blend, wgpu::RenderPipeline>,
    sprite_instances: wgpu::Buffer,
    /// The scene is drawn here first, then copied out applying the display gamma.
    scene_color: wgpu::Texture,
    scene_group: wgpu::BindGroup,
    post: wgpu::RenderPipeline,
    /// Scales the world and adds a colour over it, for the screen flash.
    flash: wgpu::RenderPipeline,
    flash_globals: wgpu::Buffer,
    flash_group: wgpu::BindGroup,
    post_globals: wgpu::Buffer,
    post_group: wgpu::BindGroup,
    white: TextureId,
    textures: Vec<Texture>,
    meshes: Vec<Mesh>,
    /// What the last frame drew with.
    drawn_textures: std::collections::HashSet<TextureId>,
    sampler: wgpu::Sampler,
    /// Last frame read back from an offscreen target.
    last_frame: Option<Image>,
    capture_cache: HashMap<(u32, u32), wgpu::Buffer>,
}

const SHADER: &str = r#"
struct Globals {
    view_proj: mat4x4<f32>,
    model: mat4x4<f32>,
    view: mat4x4<f32>,
    params: vec4<f32>, // x: alpha test, y: lightmap modulation, z: environment mapping
    tint: vec4<f32>,
    camera: vec4<f32>,
    light_dir: array<vec4<f32>, 3>,
    light_color: array<vec4<f32>, 3>,
    // Plane to keep the drawing on the front side of, for what a mirror shows.
    clip: vec4<f32>,
    // How far the texture has slid over the surface by now, for `PF_AutoUPan`/`PF_AutoVPan`.
    pan: vec4<f32>,
};

@group(0) @binding(0) var<uniform> globals: Globals;
@group(1) @binding(0) var tex: texture_2d<f32>;
@group(1) @binding(1) var samp: sampler;
@group(2) @binding(0) var light_map: texture_2d<f32>;
@group(2) @binding(1) var light_samp: sampler;

struct VsOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) uv: vec2<f32>,
    @location(1) normal: vec3<f32>,
    @location(2) light: vec3<f32>,
    @location(3) uv2: vec2<f32>,
    @location(4) world: vec3<f32>,
};

@vertex
fn vs(
    @location(0) position: vec3<f32>,
    @location(1) uv: vec2<f32>,
    @location(2) normal: vec3<f32>,
    @location(3) color: vec3<f32>,
    @location(4) uv2: vec2<f32>,
) -> VsOut {
    var out: VsOut;
    let world = globals.model * vec4<f32>(position, 1.0);
    out.clip = globals.view_proj * world;
    out.uv = uv + globals.pan.xy;
    let n = normalize((globals.model * vec4<f32>(normal, 0.0)).xyz);
    out.normal = n;
    var lit = globals.tint.rgb;
    for (var i = 0u; i < 3u; i = i + 1u) {
        // Half of each light reaches the whole mesh, so the shading stays soft.
        lit = lit + globals.light_color[i].rgb * (0.5 + 0.5 * dot(n, globals.light_dir[i].xyz));
    }
    out.light = color * min(lit, vec3<f32>(1.0, 1.0, 1.0));
    out.uv2 = uv2;
    out.world = world.xyz;
    return out;
}

@fragment
fn fs(in: VsOut) -> @location(0) vec4<f32> {
    if (dot(globals.clip.xyz, in.world) - globals.clip.w < 0.0) {
        discard;
    }
    var uv = in.uv;
    if (globals.params.z > 0.5) {
        // The texture is wrapped around the mesh by its normals as the camera sees them.
        let n = normalize((globals.view * vec4<f32>(in.normal, 0.0)).xyz);
        uv = vec2<f32>(0.5 + n.x * 0.5, 0.5 - n.y * 0.5);
    }
    let texel = textureSample(tex, samp, uv);
    if (globals.params.x > 0.5 && texel.a < 0.5) {
        discard;
    }
    // Meshes without a lightmap sample a white texel, so only their tint lights them.
    let baked = textureSample(light_map, light_samp, in.uv2).rgb * globals.params.y;
    return vec4<f32>(texel.rgb * in.light * baked, 1.0);
}

// A mirror shows what was drawn from the reflected point of view, so it samples that picture
// where the surface lands on the screen rather than a texture of its own.
@fragment
fn fs_mirror(in: VsOut) -> @location(0) vec4<f32> {
    let size = vec2<f32>(textureDimensions(tex));
    let texel = textureSample(tex, samp, in.clip.xy / size);
    return vec4<f32>(texel.rgb, 1.0);
}
"#;

/// Draws the game's menus, HUD and text over the scene: one textured rectangle at a time, in
/// pixels, the way the scripts ask for them through `Canvas`.
/// Particles: a rectangle standing where the particle is, turned to face the camera.
const SPRITE_SHADER: &str = r#"
struct Globals {
    view_proj: mat4x4<f32>,
    model: mat4x4<f32>,
    view: mat4x4<f32>,
    params: vec4<f32>,
    tint: vec4<f32>,
    camera: vec4<f32>,
    light_dir: array<vec4<f32>, 3>,
    light_color: array<vec4<f32>, 3>,
    clip: vec4<f32>,
};

@group(0) @binding(0) var<uniform> globals: Globals;
@group(1) @binding(0) var tex: texture_2d<f32>;
@group(1) @binding(1) var samp: sampler;

struct VsOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) uv: vec2<f32>,
    @location(1) color: vec4<f32>,
    @location(2) lying: f32,
};

@vertex
fn vs(
    @builtin(vertex_index) index: u32,
    @location(0) place: vec4<f32>,   // xyz: where it is, w: its turn
    @location(1) size: vec4<f32>,    // xy: width and height
    @location(2) color: vec4<f32>,
    @location(3) facing: vec4<f32>,  // xyz: surface normal, w: 1 when it lies on it
) -> VsOut {
    let corner = vec2<f32>(f32(index & 1u), f32((index >> 1u) & 1u));
    // The view matrix holds the camera's own axes; a sprite lies in the plane they span.
    var right = vec3<f32>(globals.view[0][0], globals.view[1][0], globals.view[2][0]);
    var up = vec3<f32>(globals.view[0][1], globals.view[1][1], globals.view[2][1]);
    // A decal spans the surface it was thrown at instead: any two axes across its normal do,
    // since the picture is turned around that normal by the sprite's own spin.
    if (facing.w > 0.5) {
        let n = normalize(facing.xyz);
        let guess = select(vec3<f32>(0.0, 0.0, 1.0), vec3<f32>(1.0, 0.0, 0.0), abs(n.z) > 0.9);
        right = normalize(cross(guess, n));
        up = cross(n, right);
    }
    let turn = place.w * 6.2831855;
    let c = cos(turn);
    let sn = sin(turn);
    let r = right * c + up * sn;
    let u = up * c - right * sn;
    let offset = (corner - vec2<f32>(0.5, 0.5)) * size.xy;
    let world = place.xyz + r * offset.x + u * offset.y;
    var out: VsOut;
    out.clip = globals.view_proj * vec4<f32>(world, 1.0);
    out.uv = vec2<f32>(corner.x, 1.0 - corner.y);
    out.color = color;
    out.lying = facing.w;
    return out;
}

@fragment
fn fs(in: VsOut) -> @location(0) vec4<f32> {
    let texel = textureSample(tex, samp, in.uv);
    // A decal modulates what is already there, so its picture goes out as it is: mid grey
    // leaves the floor alone and the dark middle of a shadow darkens it.
    if (in.lying > 0.5) {
        // A decal is taken from the top level of its picture. Its quad lies along the surface,
        // so the slope of the texture across the screen is steep and the automatic choice falls
        // to a copy small enough to have lost the shape, which for a shadow means nothing is
        // drawn at all.
        let flat = textureSampleLevel(tex, samp, in.uv, 0.0);
        return vec4<f32>(flat.rgb * in.color.rgb, 1.0);
    }
    // Translucent adds what it draws, so the alpha is how bright the particle is.
    return vec4<f32>(texel.rgb * in.color.rgb * in.color.a * texel.a, 1.0);
}
"#;

const OVERLAY_SHADER: &str = r#"
@group(0) @binding(0) var tex: texture_2d<f32>;
@group(0) @binding(1) var samp: sampler;

struct VsOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) uv: vec2<f32>,
    @location(1) color: vec4<f32>,
    @location(2) alpha_test: f32,
    @location(3) lo: vec2<f32>,
    @location(4) hi: vec2<f32>,
};

@vertex
fn vs(
    @builtin(vertex_index) index: u32,
    @location(0) rect: vec4<f32>,
    @location(1) uv: vec4<f32>,
    @location(2) color: vec4<f32>,
    @location(3) flags: vec4<f32>,
) -> VsOut {
    let corner = vec2<f32>(f32(index & 1u), f32((index >> 1u) & 1u));
    var out: VsOut;
    out.uv = uv.xy + corner * uv.zw;
    out.lo = min(uv.xy, uv.xy + uv.zw);
    out.hi = max(uv.xy, uv.xy + uv.zw);
    out.color = color;
    out.alpha_test = flags.x;
    let pixel = rect.xy + corner * rect.zw;
    out.clip = vec4<f32>(pixel.x * 2.0 - 1.0, 1.0 - pixel.y * 2.0, 0.0, 1.0);
    return out;
}

@fragment
fn fs(in: VsOut) -> @location(0) vec4<f32> {
    // A tile is a rectangle out of a bigger picture (a font page, a sheet of window parts):
    // the filter must not reach the texels around it. Samples stay between the centres of
    // its outer texels; a tile one texel across comes out as that texel's flat colour.
    let half = 0.5 / vec2<f32>(textureDimensions(tex));
    let lo = in.lo + half;
    let hi = max(in.hi - half, lo);
    let texel = textureSample(tex, samp, clamp(in.uv, lo, hi)) * in.color;
    if (in.alpha_test > 0.5 && texel.a < 0.5) {
        discard;
    }
    return texel;
}
"#;

/// The screen flash: the fog colour, added over the world after the blend has scaled it.
const FLASH_SHADER: &str = r#"
@group(0) @binding(0) var<uniform> fog: vec4<f32>;

@vertex
fn vs(@builtin(vertex_index) index: u32) -> @builtin(position) vec4<f32> {
    let uv = vec2<f32>(f32((index << 1u) & 2u), f32(index & 2u));
    return vec4<f32>(uv * vec2<f32>(2.0, -2.0) + vec2<f32>(-1.0, 1.0), 0.0, 1.0);
}

@fragment
fn fs() -> @location(0) vec4<f32> {
    return vec4<f32>(fog.rgb, 1.0);
}
"#;

/// Copies the frame applying Unreal's display gamma, which the game sets through the
/// `Brightness` of its render device.
const POST_SHADER: &str = r#"
@group(0) @binding(0) var<uniform> gamma: vec4<f32>;
@group(1) @binding(0) var frame: texture_2d<f32>;
@group(1) @binding(1) var samp: sampler;

struct VsOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

@vertex
fn vs(@builtin(vertex_index) index: u32) -> VsOut {
    // One triangle covering the target.
    let uv = vec2<f32>(f32((index << 1u) & 2u), f32(index & 2u));
    var out: VsOut;
    out.uv = uv;
    out.clip = vec4<f32>(uv * vec2<f32>(2.0, -2.0) + vec2<f32>(-1.0, 1.0), 0.0, 1.0);
    return out;
}

@fragment
fn fs(in: VsOut) -> @location(0) vec4<f32> {
    let c = textureSample(frame, samp, in.uv).rgb;
    return vec4<f32>(pow(c, vec3<f32>(1.0 / gamma.x)), 1.0);
}
"#;

impl WgpuRenderer {
    /// Renderer without a window, drawing into an image of the given size.
    #[cfg(not(target_arch = "wasm32"))]
    pub fn offscreen(width: u32, height: u32) -> Result<Self, String> {
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env());
        let (device, queue, adapter) = Self::device(&instance, None)?;
        let _ = adapter;
        let format = wgpu::TextureFormat::Rgba8Unorm;
        let color = Self::color_texture(&device, width, height, format);
        let depth = Self::depth_view(&device, width, height);
        Self::build(device, queue, Target::Offscreen { color, width, height }, format, depth)
    }

    /// Renderer drawing into a window surface. The window is shared so the surface can
    /// outlive this call, and its display handle is what Wayland needs to create the instance.
    #[cfg(not(target_arch = "wasm32"))]
    pub fn for_window<W>(window: std::sync::Arc<W>, width: u32, height: u32) -> Result<Self, String>
    where
        W: wgpu::rwh::HasWindowHandle + wgpu::rwh::HasDisplayHandle + std::fmt::Debug + Send + Sync + 'static,
    {
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_with_display_handle_from_env(Box::new(window.clone())));
        pollster::block_on(Self::for_surface(instance, window, width, height))
    }

    /// The same in a browser, where nothing may block: the window is a canvas, and the GPU is
    /// WebGPU where the browser has it and WebGL 2 where it does not.
    #[cfg(target_arch = "wasm32")]
    pub async fn for_window<W>(window: std::sync::Arc<W>, width: u32, height: u32) -> Result<Self, String>
    where
        W: wgpu::rwh::HasWindowHandle + wgpu::rwh::HasDisplayHandle + std::fmt::Debug + Send + Sync + 'static,
    {
        let instance = wgpu::util::new_instance_with_webgpu_detection(wgpu::InstanceDescriptor::new_with_display_handle(Box::new(window.clone()))).await;
        Self::for_surface(instance, window, width, height).await
    }

    async fn for_surface<W>(instance: wgpu::Instance, window: std::sync::Arc<W>, width: u32, height: u32) -> Result<Self, String>
    where
        W: wgpu::rwh::HasWindowHandle + wgpu::rwh::HasDisplayHandle + std::fmt::Debug + Send + Sync + 'static,
    {
        let surface = instance.create_surface(window).map_err(|e| e.to_string())?;
        let (device, queue, adapter) = Self::request_device(&instance, Some(&surface)).await?;
        let caps = surface.get_capabilities(&adapter);
        let format = caps
            .formats
            .iter()
            .copied()
            .find(|f| f.remove_srgb_suffix() == *f && f.target_pixel_byte_cost() == Some(4))
            .or_else(|| caps.formats.iter().map(|f| f.remove_srgb_suffix()).find(|f| caps.formats.contains(&f.add_srgb_suffix())))
            .unwrap_or(caps.formats[0]);
        let max = device.limits().max_texture_dimension_2d;
        let config = wgpu::SurfaceConfiguration {
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            format,
            width: width.clamp(1, max),
            height: height.clamp(1, max),
            color_space: wgpu::SurfaceColorSpace::Srgb,
            // Mailbox where the surface has it: still no tearing, but a frame that runs a
            // little long is shown late instead of dragging the rate down to half, which is
            // what the plain wait for the vertical blank does to a game hovering near it.
            present_mode: if caps.present_modes.contains(&wgpu::PresentMode::Mailbox) {
                wgpu::PresentMode::Mailbox
            } else {
                wgpu::PresentMode::AutoVsync
            },
            alpha_mode: caps.alpha_modes[0],
            view_formats: vec![],
            desired_maximum_frame_latency: 2,
        };
        surface.configure(&device, &config);
        let depth = Self::depth_view(&device, config.width, config.height);
        Self::build(device, queue, Target::Surface { surface, config }, format, depth)
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn device(
        instance: &wgpu::Instance,
        surface: Option<&wgpu::Surface<'static>>,
    ) -> Result<(wgpu::Device, wgpu::Queue, wgpu::Adapter), String> {
        pollster::block_on(Self::request_device(instance, surface))
    }

    async fn request_device(
        instance: &wgpu::Instance,
        surface: Option<&wgpu::Surface<'static>>,
    ) -> Result<(wgpu::Device, wgpu::Queue, wgpu::Adapter), String> {
        let adapter = instance
            .request_adapter(&wgpu::RequestAdapterOptions {
                power_preference: wgpu::PowerPreference::HighPerformance,
                compatible_surface: surface,
                force_fallback_adapter: false,
                apply_limit_buckets: false,
            })
            .await
            .map_err(|e| format!("no GPU adapter: {e}"))?;
        // The adapter's own limits: the conservative defaults cap textures at 2048, which is
        // smaller than a common desktop window.
        let limits = adapter.limits();
        let (device, queue) = adapter
            .request_device(&wgpu::DeviceDescriptor {
                label: Some("grim"),
                required_features: wgpu::Features::empty(),
                required_limits: limits,
                experimental_features: Default::default(),
                memory_hints: wgpu::MemoryHints::Performance,
                trace: wgpu::Trace::Off,
            })
            .await
            .map_err(|e| format!("no GPU device: {e}"))?;
        Ok((device, queue, adapter))
    }

    fn color_texture(device: &wgpu::Device, width: u32, height: u32, format: wgpu::TextureFormat) -> wgpu::Texture {
        device.create_texture(&wgpu::TextureDescriptor {
            label: Some("color"),
            size: wgpu::Extent3d { width: width.max(1), height: height.max(1), depth_or_array_layers: 1 },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        })
    }

    /// Target the scene is drawn into, before the gamma copy. It keeps the art's own colour
    /// space: Unreal blended and lit 8 bit textures directly, without a linear conversion.
    fn scene_texture(device: &wgpu::Device, width: u32, height: u32) -> wgpu::Texture {
        device.create_texture(&wgpu::TextureDescriptor {
            label: Some("scene"),
            size: wgpu::Extent3d { width: width.max(1), height: height.max(1), depth_or_array_layers: 1 },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: SCENE_FORMAT,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        })
    }

    fn scene_bind_group(
        device: &wgpu::Device,
        layout: &wgpu::BindGroupLayout,
        texture: &wgpu::Texture,
        sampler: &wgpu::Sampler,
    ) -> wgpu::BindGroup {
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("scene"),
            layout,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::TextureView(&view) },
                wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::Sampler(sampler) },
            ],
        })
    }

    fn depth_view(device: &wgpu::Device, width: u32, height: u32) -> wgpu::TextureView {
        device
            .create_texture(&wgpu::TextureDescriptor {
                label: Some("depth"),
                size: wgpu::Extent3d { width: width.max(1), height: height.max(1), depth_or_array_layers: 1 },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: DEPTH_FORMAT,
                usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
                view_formats: &[],
            })
            .create_view(&wgpu::TextureViewDescriptor::default())
    }

    fn build(
        device: wgpu::Device,
        queue: wgpu::Queue,
        target: Target,
        format: wgpu::TextureFormat,
        depth: wgpu::TextureView,
    ) -> Result<Self, String> {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("grim"),
            source: wgpu::ShaderSource::Wgsl(SHADER.into()),
        });
        let globals_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("globals"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: true,
                    min_binding_size: wgpu::BufferSize::new(GLOBALS_STRIDE),
                },
                count: None,
            }],
        });
        let texture_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("texture"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
            ],
        });
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("grim"),
            bind_group_layouts: &[Some(&globals_layout), Some(&texture_layout), Some(&texture_layout)],
            immediate_size: 0,
        });
        let make_pipeline = |cull: Option<wgpu::Face>, blend: Blend, depth: bool, write: wgpu::ColorWrites| device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("grim"),
            layout: Some(&layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs"),
                compilation_options: Default::default(),
                buffers: &[Some(wgpu::VertexBufferLayout {
                    array_stride: 13 * 4,
                    step_mode: wgpu::VertexStepMode::Vertex,
                    attributes: &[
                        wgpu::VertexAttribute { format: wgpu::VertexFormat::Float32x3, offset: 0, shader_location: 0 },
                        wgpu::VertexAttribute { format: wgpu::VertexFormat::Float32x2, offset: 12, shader_location: 1 },
                        wgpu::VertexAttribute { format: wgpu::VertexFormat::Float32x3, offset: 20, shader_location: 2 },
                        wgpu::VertexAttribute { format: wgpu::VertexFormat::Float32x3, offset: 32, shader_location: 3 },
                        wgpu::VertexAttribute { format: wgpu::VertexFormat::Float32x2, offset: 44, shader_location: 4 },
                    ],
                })],
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format: SCENE_FORMAT,
                    blend: match blend {
                        Blend::Opaque => None,
                        // Additive: black texels leave the background untouched.
                        Blend::Translucent => Some(wgpu::BlendState {
                            color: wgpu::BlendComponent {
                                src_factor: wgpu::BlendFactor::One,
                                dst_factor: wgpu::BlendFactor::One,
                                operation: wgpu::BlendOperation::Add,
                            },
                            alpha: wgpu::BlendComponent::REPLACE,
                        }),
                        // Modulate 2x: src*dst + dst*src.
                        Blend::Modulated => Some(wgpu::BlendState {
                            color: wgpu::BlendComponent {
                                src_factor: wgpu::BlendFactor::Dst,
                                dst_factor: wgpu::BlendFactor::Src,
                                operation: wgpu::BlendOperation::Add,
                            },
                            alpha: wgpu::BlendComponent::REPLACE,
                        }),
                    },
                    write_mask: write,
                })],
            }),
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleList,
                front_face: wgpu::FrontFace::Cw,
                cull_mode: cull,
                ..Default::default()
            },
            depth_stencil: Some(wgpu::DepthStencilState {
                format: DEPTH_FORMAT,
                // Blended surfaces test depth but do not write it, and the backdrop neither:
                // it sits behind everything whatever its own distance says.
                depth_write_enabled: Some(depth && (blend == Blend::Opaque || write.is_empty())),
                depth_compare: Some(if depth { wgpu::CompareFunction::Less } else { wgpu::CompareFunction::Always }),
                stencil: Default::default(),
                bias: Default::default(),
            }),
            multisample: Default::default(),
            multiview_mask: None,
            cache: None,
        });
        // The windows onto the sky only fill the depth buffer, so what the level keeps beyond
        // them is thrown away while the backdrop drawn before them stays.
        // Only the faces turned towards the camera mask: one the camera has passed is not
        // between it and the level.
        let backdrop_pipeline = make_pipeline(Some(wgpu::Face::Back), Blend::Opaque, true, wgpu::ColorWrites::empty());
        let mirrored_backdrop_pipeline = make_pipeline(Some(wgpu::Face::Front), Blend::Opaque, true, wgpu::ColorWrites::empty());
        let mut mirrored_sky_pipelines = HashMap::new();
        let mut pipelines = HashMap::new();
        let mut sky_pipelines = HashMap::new();
        // Reflecting the world turns it inside out, so the faces to keep are the other ones.
        let mut mirrored_pipelines = HashMap::new();
        for blend in [Blend::Opaque, Blend::Translucent, Blend::Modulated] {
            for two_sided in [false, true] {
                let cull = (!two_sided).then_some(wgpu::Face::Back);
                pipelines.insert((blend, two_sided), make_pipeline(cull, blend, true, wgpu::ColorWrites::ALL));
                sky_pipelines.insert((blend, two_sided), make_pipeline(cull, blend, false, wgpu::ColorWrites::ALL));
                let flipped = (!two_sided).then_some(wgpu::Face::Front);
                mirrored_pipelines.insert((blend, two_sided), make_pipeline(flipped, blend, true, wgpu::ColorWrites::ALL));
                mirrored_sky_pipelines.insert((blend, two_sided), make_pipeline(flipped, blend, false, wgpu::ColorWrites::ALL));
            }
        }
        let mirror_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("mirror"),
            layout: Some(&layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs"),
                compilation_options: Default::default(),
                buffers: &[Some(wgpu::VertexBufferLayout {
                    array_stride: 13 * 4,
                    step_mode: wgpu::VertexStepMode::Vertex,
                    attributes: &[
                        wgpu::VertexAttribute { format: wgpu::VertexFormat::Float32x3, offset: 0, shader_location: 0 },
                        wgpu::VertexAttribute { format: wgpu::VertexFormat::Float32x2, offset: 12, shader_location: 1 },
                        wgpu::VertexAttribute { format: wgpu::VertexFormat::Float32x3, offset: 20, shader_location: 2 },
                        wgpu::VertexAttribute { format: wgpu::VertexFormat::Float32x3, offset: 32, shader_location: 3 },
                        wgpu::VertexAttribute { format: wgpu::VertexFormat::Float32x2, offset: 44, shader_location: 4 },
                    ],
                })],
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_mirror"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format: SCENE_FORMAT,
                    blend: None,
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleList,
                front_face: wgpu::FrontFace::Cw,
                cull_mode: Some(wgpu::Face::Back),
                ..Default::default()
            },
            depth_stencil: Some(wgpu::DepthStencilState {
                format: DEPTH_FORMAT,
                depth_write_enabled: Some(true),
                depth_compare: Some(wgpu::CompareFunction::Less),
                stencil: Default::default(),
                bias: Default::default(),
            }),
            multisample: Default::default(),
            multiview_mask: None,
            cache: None,
        });
        let globals = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("globals"),
            size: GLOBALS_STRIDE * 4096,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let globals_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("globals"),
            layout: &globals_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::Buffer(wgpu::BufferBinding {
                    buffer: &globals,
                    offset: 0,
                    size: wgpu::BufferSize::new(GLOBALS_STRIDE),
                }),
            }],
        });
        let post_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("grim post"),
            source: wgpu::ShaderSource::Wgsl(POST_SHADER.into()),
        });
        let post_globals_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("post globals"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: wgpu::BufferSize::new(16),
                },
                count: None,
            }],
        });
        let post_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("grim post"),
            bind_group_layouts: &[Some(&post_globals_layout), Some(&texture_layout)],
            immediate_size: 0,
        });
        let post = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("grim post"),
            layout: Some(&post_layout),
            vertex: wgpu::VertexState {
                module: &post_shader,
                entry_point: Some("vs"),
                compilation_options: Default::default(),
                buffers: &[],
            },
            fragment: Some(wgpu::FragmentState {
                module: &post_shader,
                entry_point: Some("fs"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState { format, blend: None, write_mask: wgpu::ColorWrites::ALL })],
            }),
            primitive: Default::default(),
            depth_stencil: None,
            multisample: Default::default(),
            multiview_mask: None,
            cache: None,
        });
        let flash_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("grim flash"),
            source: wgpu::ShaderSource::Wgsl(FLASH_SHADER.into()),
        });
        let flash_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("grim flash"),
            bind_group_layouts: &[Some(&post_globals_layout)],
            immediate_size: 0,
        });
        let flash = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("grim flash"),
            layout: Some(&flash_layout),
            vertex: wgpu::VertexState {
                module: &flash_shader,
                entry_point: Some("vs"),
                compilation_options: Default::default(),
                buffers: &[],
            },
            fragment: Some(wgpu::FragmentState {
                module: &flash_shader,
                entry_point: Some("fs"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format: SCENE_FORMAT,
                    // What is there is scaled by the blend constant, the fog is added to it.
                    blend: Some(wgpu::BlendState {
                        color: wgpu::BlendComponent {
                            src_factor: wgpu::BlendFactor::One,
                            dst_factor: wgpu::BlendFactor::Constant,
                            operation: wgpu::BlendOperation::Add,
                        },
                        alpha: wgpu::BlendComponent::REPLACE,
                    }),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            primitive: Default::default(),
            depth_stencil: None,
            multisample: Default::default(),
            multiview_mask: None,
            cache: None,
        });
        let flash_globals = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("flash globals"),
            size: 16,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let flash_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("flash globals"),
            layout: &post_globals_layout,
            entries: &[wgpu::BindGroupEntry { binding: 0, resource: flash_globals.as_entire_binding() }],
        });
        let sprite_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("grim sprites"),
            source: wgpu::ShaderSource::Wgsl(SPRITE_SHADER.into()),
        });
        let sprite_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("grim sprites"),
            bind_group_layouts: &[Some(&globals_layout), Some(&texture_layout)],
            immediate_size: 0,
        });
        let make_sprite = |blend: Blend, through: bool| device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("grim sprites"),
            layout: Some(&sprite_layout),
            vertex: wgpu::VertexState {
                module: &sprite_shader,
                entry_point: Some("vs"),
                compilation_options: Default::default(),
                buffers: &[Some(wgpu::VertexBufferLayout {
                    array_stride: SPRITE_SIZE,
                    step_mode: wgpu::VertexStepMode::Instance,
                    attributes: &[
                        wgpu::VertexAttribute { format: wgpu::VertexFormat::Float32x4, offset: 0, shader_location: 0 },
                        wgpu::VertexAttribute { format: wgpu::VertexFormat::Float32x4, offset: 16, shader_location: 1 },
                        wgpu::VertexAttribute { format: wgpu::VertexFormat::Float32x4, offset: 32, shader_location: 2 },
                        wgpu::VertexAttribute { format: wgpu::VertexFormat::Float32x4, offset: 48, shader_location: 3 },
                    ],
                })],
            },
            fragment: Some(wgpu::FragmentState {
                module: &sprite_shader,
                entry_point: Some("fs"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format: SCENE_FORMAT,
                    blend: match blend {
                        Blend::Opaque => None,
                        Blend::Translucent => Some(wgpu::BlendState {
                            color: wgpu::BlendComponent {
                                src_factor: wgpu::BlendFactor::One,
                                dst_factor: wgpu::BlendFactor::One,
                                operation: wgpu::BlendOperation::Add,
                            },
                            alpha: wgpu::BlendComponent::REPLACE,
                        }),
                        Blend::Modulated => Some(wgpu::BlendState {
                            color: wgpu::BlendComponent {
                                src_factor: wgpu::BlendFactor::Dst,
                                dst_factor: wgpu::BlendFactor::Src,
                                operation: wgpu::BlendOperation::Add,
                            },
                            alpha: wgpu::BlendComponent::REPLACE,
                        }),
                    },
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            primitive: wgpu::PrimitiveState { topology: wgpu::PrimitiveTopology::TriangleStrip, ..Default::default() },
            // Particles are hidden by the world but never hide each other.
            depth_stencil: Some(wgpu::DepthStencilState {
                format: DEPTH_FORMAT,
                depth_write_enabled: Some(false),
                depth_compare: Some(if through {
                    wgpu::CompareFunction::Always
                } else {
                    wgpu::CompareFunction::LessEqual
                }),
                stencil: Default::default(),
                bias: Default::default(),
            }),
            multisample: Default::default(),
            multiview_mask: None,
            cache: None,
        });
        let sprites: HashMap<Blend, wgpu::RenderPipeline> = [Blend::Opaque, Blend::Translucent, Blend::Modulated]
            .into_iter()
            .map(|b| (b, make_sprite(b, false)))
            .collect();
        let sprites_through: HashMap<Blend, wgpu::RenderPipeline> = [Blend::Opaque, Blend::Translucent, Blend::Modulated]
            .into_iter()
            .map(|b| (b, make_sprite(b, true)))
            .collect();
        let sprite_instances = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("sprites"),
            size: SPRITE_SIZE * 4096,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let overlay_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("grim overlay"),
            source: wgpu::ShaderSource::Wgsl(OVERLAY_SHADER.into()),
        });
        let overlay_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("grim overlay"),
            bind_group_layouts: &[Some(&texture_layout)],
            immediate_size: 0,
        });
        let make_overlay = |blend: Blend| device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("grim overlay"),
            layout: Some(&overlay_layout),
            vertex: wgpu::VertexState {
                module: &overlay_shader,
                entry_point: Some("vs"),
                compilation_options: Default::default(),
                buffers: &[Some(wgpu::VertexBufferLayout {
                    array_stride: QUAD_SIZE,
                    step_mode: wgpu::VertexStepMode::Instance,
                    attributes: &[
                        wgpu::VertexAttribute { format: wgpu::VertexFormat::Float32x4, offset: 0, shader_location: 0 },
                        wgpu::VertexAttribute { format: wgpu::VertexFormat::Float32x4, offset: 16, shader_location: 1 },
                        wgpu::VertexAttribute { format: wgpu::VertexFormat::Float32x4, offset: 32, shader_location: 2 },
                        wgpu::VertexAttribute { format: wgpu::VertexFormat::Float32x4, offset: 48, shader_location: 3 },
                    ],
                })],
            },
            fragment: Some(wgpu::FragmentState {
                module: &overlay_shader,
                entry_point: Some("fs"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format: SCENE_FORMAT,
                    blend: match blend {
                        // A normal tile covers what is under it, whatever its own alpha.
                        Blend::Opaque => None,
                        Blend::Translucent => Some(wgpu::BlendState {
                            color: wgpu::BlendComponent {
                                src_factor: wgpu::BlendFactor::One,
                                dst_factor: wgpu::BlendFactor::One,
                                operation: wgpu::BlendOperation::Add,
                            },
                            alpha: wgpu::BlendComponent::REPLACE,
                        }),
                        Blend::Modulated => Some(wgpu::BlendState {
                            color: wgpu::BlendComponent {
                                src_factor: wgpu::BlendFactor::Dst,
                                dst_factor: wgpu::BlendFactor::Src,
                                operation: wgpu::BlendOperation::Add,
                            },
                            alpha: wgpu::BlendComponent::REPLACE,
                        }),
                    },
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            primitive: wgpu::PrimitiveState { topology: wgpu::PrimitiveTopology::TriangleStrip, ..Default::default() },
            // It shares the scene's pass, so it declares the same depth buffer but ignores it.
            depth_stencil: Some(wgpu::DepthStencilState {
                format: DEPTH_FORMAT,
                depth_write_enabled: Some(false),
                depth_compare: Some(wgpu::CompareFunction::Always),
                stencil: Default::default(),
                bias: Default::default(),
            }),
            multisample: Default::default(),
            multiview_mask: None,
            cache: None,
        });
        let overlay: HashMap<Blend, wgpu::RenderPipeline> = [Blend::Opaque, Blend::Translucent, Blend::Modulated]
            .into_iter()
            .map(|b| (b, make_overlay(b)))
            .collect();
        let overlay_instances = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("overlay"),
            size: QUAD_SIZE * 4096,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let post_globals = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("post globals"),
            size: 16,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let post_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("post globals"),
            layout: &post_globals_layout,
            entries: &[wgpu::BindGroupEntry { binding: 0, resource: post_globals.as_entire_binding() }],
        });
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("grim"),
            address_mode_u: wgpu::AddressMode::Repeat,
            address_mode_v: wgpu::AddressMode::Repeat,
            address_mode_w: wgpu::AddressMode::Repeat,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            mipmap_filter: wgpu::MipmapFilterMode::Linear,
            ..Default::default()
        });
        let (w, h) = match &target {
            Target::Offscreen { width, height, .. } => (*width, *height),
            Target::Surface { config, .. } => (config.width, config.height),
        };
        let scene_color = Self::scene_texture(&device, w, h);
        let scene_group = Self::scene_bind_group(&device, &texture_layout, &scene_color, &sampler);
        let reflection_texture = Self::scene_texture(&device, w, h);
        let reflection = reflection_texture.create_view(&wgpu::TextureViewDescriptor::default());
        let reflection_group = Self::scene_bind_group(&device, &texture_layout, &reflection_texture, &sampler);
        let reflection_depth = Self::depth_view(&device, w, h);
        let mut r = Self {
            device,
            queue,
            target,
            format,
            depth,
            pipelines,
            sky_pipelines,
            backdrop_pipeline,
            mirrored_sky_pipelines,
            mirrored_backdrop_pipeline,
            mirrored_pipelines,
            mirror_pipeline,
            reflection,
            reflection_group,
            reflection_depth,
            overlay,
            overlay_instances,
            lightmap_scale: 2.0,
            sprites,
            sprites_through,
            sprite_instances,
            globals,
            globals_group,
            texture_layout,
            scene_color,
            scene_group,
            post,
            flash,
            flash_globals,
            flash_group,
            post_globals,
            post_group,
            white: TextureId(0),
            textures: Vec::new(),
            meshes: Vec::new(),
            drawn_textures: std::collections::HashSet::new(),
            sampler,
            last_frame: None,
            capture_cache: HashMap::new(),
        };
        r.white = r.create_texture(TextureData { width: 1, height: 1, rgba: &[255, 255, 255, 255], mips: &[] });
        Ok(r)
    }

    fn size(&self) -> (u32, u32) {
        match &self.target {
            Target::Offscreen { width, height, .. } => (*width, *height),
            Target::Surface { config, .. } => (config.width, config.height),
        }
    }
}

fn globals_bytes(
    view_proj: Mat4,
    model: Mat4,
    view: Mat4,
    alpha_test: bool,
    tint: [f32; 3],
    lightmap_scale: f32,
    env_map: bool,
    camera: [f32; 3],
    lights: &[MeshLight; MESH_LIGHTS],
    clip: [f32; 4],
    pan: [f32; 2],
) -> Vec<u8> {
    let mut out = Vec::with_capacity(GLOBALS_STRIDE as usize);
    for m in [view_proj, model, view] {
        for col in m {
            for v in col {
                out.extend_from_slice(&v.to_le_bytes());
            }
        }
    }
    let mut push = |v: [f32; 4]| out.extend(v.iter().flat_map(|f| f.to_le_bytes()));
    push([if alpha_test { 1.0 } else { 0.0 }, lightmap_scale, if env_map { 1.0 } else { 0.0 }, 0.0]);
    push([tint[0], tint[1], tint[2], 1.0]);
    push([camera[0], camera[1], camera[2], 1.0]);
    for l in lights {
        push([l.direction[0], l.direction[1], l.direction[2], 0.0]);
    }
    for l in lights {
        push([l.color[0], l.color[1], l.color[2], 0.0]);
    }
    push(clip);
    push([pan[0], pan[1], 0.0, 0.0]);
    out
}

/// A plane nothing is clipped against: everything is on its front side.
const NO_CLIP: [f32; 4] = [0.0, 0.0, 0.0, -1.0];

/// One overlay rectangle's uniforms, with the screen rectangle in 0..1 of the target.
fn quad_bytes(q: &Quad, width: u32, height: u32) -> Vec<u8> {
    let (w, h) = (width.max(1) as f32, height.max(1) as f32);
    let values = [
        q.rect[0] / w,
        q.rect[1] / h,
        q.rect[2] / w,
        q.rect[3] / h,
        q.uv[0],
        q.uv[1],
        q.uv[2],
        q.uv[3],
        q.color[0],
        q.color[1],
        q.color[2],
        q.color[3],
        if q.masked { 1.0 } else { 0.0 },
        0.0,
        0.0,
        0.0,
    ];
    values.iter().flat_map(|f| f.to_le_bytes()).collect()
}

fn vertex_bytes(data: &MeshData) -> Vec<u8> {
    let mut out = Vec::with_capacity(data.vertices.len() * 44);
    for v in &data.vertices {
        let f = [
            v.position[0], v.position[1], v.position[2],
            v.uv[0], v.uv[1],
            v.normal[0], v.normal[1], v.normal[2],
            v.color[0], v.color[1], v.color[2],
            v.uv2[0], v.uv2[1],
        ];
        for f in f {
            out.extend_from_slice(&f.to_le_bytes());
        }
    }
    out
}

impl WgpuRenderer {
    /// Every texture a scene draws with: what its meshes' sections carry, or the skins over
    /// them, its particles and what is drawn over it.
    fn note_drawn_textures(&mut self, scene: &Scene<'_>) {
        let mut used = std::mem::take(&mut self.drawn_textures);
        used.clear();
        let item = |it: &grim_render::DrawItem<'_>, used: &mut std::collections::HashSet<TextureId>| {
            if let Some(mesh) = self.meshes.get(it.mesh.0 as usize) {
                for s in &mesh.sections {
                    let skin = it.skins.get(s.material as usize).copied().flatten();
                    if let Some(t) = skin.or(s.texture) {
                        used.insert(t);
                    }
                }
            }
        };
        for it in scene.items.iter().chain(scene.foreground.iter()).chain(scene.backdrop.iter()) {
            item(it, &mut used);
        }
        for (it, _) in &scene.mirrors {
            item(it, &mut used);
        }
        if let Some((it, _)) = &scene.sky {
            item(it, &mut used);
        }
        used.extend(scene.sprites.iter().filter_map(|s| s.texture));
        used.extend(scene.overlay.iter().filter_map(|q| q.texture));
        self.drawn_textures = used;
    }
}

impl Renderer for WgpuRenderer {
    fn create_texture(&mut self, data: TextureData<'_>) -> TextureId {
        let size = wgpu::Extent3d { width: data.width.max(1), height: data.height.max(1), depth_or_array_layers: 1 };
        // The picture and every smaller copy the game ships with it. Each level is half the
        // one before, down to a single texel, and a level is only used if it is the right
        // size: a chain that stops short leaves the rest undrawn.
        let mut levels: Vec<&[u8]> = vec![data.rgba];
        let (mut w, mut h) = (size.width, size.height);
        for mip in data.mips {
            let (nw, nh) = ((w / 2).max(1), (h / 2).max(1));
            if w == 1 && h == 1 {
                break;
            }
            if mip.len() as u32 != nw * nh * 4 {
                break;
            }
            levels.push(mip);
            (w, h) = (nw, nh);
        }
        let texture = self.device.create_texture(&wgpu::TextureDescriptor {
            label: None,
            size,
            mip_level_count: levels.len() as u32,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        let (mut w, mut h) = (size.width, size.height);
        for (level, pixels) in levels.iter().enumerate() {
            self.queue.write_texture(
                wgpu::TexelCopyTextureInfo {
                    texture: &texture,
                    mip_level: level as u32,
                    origin: wgpu::Origin3d::ZERO,
                    aspect: wgpu::TextureAspect::All,
                },
                pixels,
                wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(4 * w), rows_per_image: Some(h) },
                wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
            );
            (w, h) = ((w / 2).max(1), (h / 2).max(1));
        }
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        let bind_group = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: None,
            layout: &self.texture_layout,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::TextureView(&view) },
                wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::Sampler(&self.sampler) },
            ],
        });
        self.textures.push(Texture { bind_group, texture, width: size.width, height: size.height });
        TextureId(self.textures.len() as u32 - 1)
    }

    fn update_texture(&mut self, texture: TextureId, data: TextureData<'_>) {
        let Some(t) = self.textures.get(texture.0 as usize) else { return };
        let size = wgpu::Extent3d { width: data.width.max(1), height: data.height.max(1), depth_or_array_layers: 1 };
        if (t.width, t.height) != (size.width, size.height) {
            return;
        }
        self.queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &t.texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            data.rgba,
            wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(4 * size.width), rows_per_image: Some(size.height) },
            size,
        );
    }

    fn create_mesh(&mut self, data: &MeshData) -> MeshId {
        let vertices = self.device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: None,
            contents: &vertex_bytes(data),
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
        });
        let index_bytes: Vec<u8> = data.indices.iter().flat_map(|i| i.to_le_bytes()).collect();
        let indices = self.device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: None,
            contents: &index_bytes,
            usage: wgpu::BufferUsages::INDEX,
        });
        self.meshes.push(Mesh { vertices, indices, sections: data.sections.clone() });
        MeshId(self.meshes.len() as u32 - 1)
    }

    fn update_mesh(&mut self, mesh: MeshId, data: &MeshData) {
        let bytes = vertex_bytes(data);
        let Some(m) = self.meshes.get_mut(mesh.0 as usize) else { return };
        // A pose keeps the vertex count, so the buffer is only rewritten.
        if bytes.len() as u64 <= m.vertices.size() {
            self.queue.write_buffer(&m.vertices, 0, &bytes);
            return;
        }
        m.vertices = self.device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: None,
            contents: &bytes,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
        });
    }

    fn clear_meshes(&mut self) {
        self.meshes.clear();
    }

    fn resize(&mut self, width: u32, height: u32) {
        let max = self.device.limits().max_texture_dimension_2d;
        let (w, h) = (width.clamp(1, max), height.clamp(1, max));
        match &mut self.target {
            Target::Offscreen { color, width, height } => {
                *color = Self::color_texture(&self.device, w, h, self.format);
                (*width, *height) = (w, h);
            }
            Target::Surface { surface, config } => {
                config.width = w;
                config.height = h;
                surface.configure(&self.device, config);
            }
        }
        self.depth = Self::depth_view(&self.device, w, h);
        self.scene_color = Self::scene_texture(&self.device, w, h);
        self.scene_group = Self::scene_bind_group(&self.device, &self.texture_layout, &self.scene_color, &self.sampler);
        let reflection = Self::scene_texture(&self.device, w, h);
        self.reflection = reflection.create_view(&wgpu::TextureViewDescriptor::default());
        self.reflection_group = Self::scene_bind_group(&self.device, &self.texture_layout, &reflection, &self.sampler);
        self.reflection_depth = Self::depth_view(&self.device, w, h);
    }

    fn render(&mut self, scene: &Scene<'_>) -> Result<(), String> {
        self.note_drawn_textures(scene);
        let (width, height) = self.size();
        let aspect = width as f32 / height.max(1) as f32;
        let view = math::view(scene.camera.position, scene.camera.rotation);
        let view_proj = math::mul(math::perspective(scene.camera.fov_y_deg, aspect, scene.camera.near, scene.camera.far), view);

        // One uniform slot per section drawn, the sky's first.
        let mut offsets = Vec::new();
        let mut uniforms = Vec::new();
        let mut sky_slots = 0;
        if let Some((item, at)) = &scene.sky {
            let view = math::view(*at, scene.camera.rotation);
            let view_proj = math::mul(math::perspective(scene.camera.fov_y_deg, aspect, scene.camera.near, scene.camera.far), view);
            if let Some(mesh) = self.meshes.get(item.mesh.0 as usize) {
                for s in &mesh.sections {
                    offsets.push(uniforms.len() as u32);
                    let scale = if item.lightmap.is_some() { self.lightmap_scale } else { 1.0 };
                    uniforms.extend(globals_bytes(
                        view_proj,
                        item.transform,
                        view,
                        item.alpha_test.unwrap_or(s.masked),
                        item.tint,
                        scale,
                        item.env_map,
                        *at,
                        &item.lights,
                        NO_CLIP,
                        [s.pan[0] * scene.time, s.pan[1] * scene.time],
                    ));
                    uniforms.resize(uniforms.len().div_ceil(GLOBALS_STRIDE as usize) * GLOBALS_STRIDE as usize, 0);
                    sky_slots += 1;
                }
            }
        }
        let mut backdrop_slots = 0;
        if let Some(item) = &scene.backdrop {
            if let Some(mesh) = self.meshes.get(item.mesh.0 as usize) {
                for s in &mesh.sections {
                    offsets.push(uniforms.len() as u32);
                    uniforms.extend(globals_bytes(
                        view_proj,
                        item.transform,
                        view,
                        item.alpha_test.unwrap_or(s.masked),
                        item.tint,
                        1.0,
                        false,
                        scene.camera.position,
                        &item.lights,
                        NO_CLIP,
                        [s.pan[0] * scene.time, s.pan[1] * scene.time],
                    ));
                    uniforms.resize(uniforms.len().div_ceil(GLOBALS_STRIDE as usize) * GLOBALS_STRIDE as usize, 0);
                    backdrop_slots += 1;
                }
            }
        }
        for item in &scene.items {
            let Some(mesh) = self.meshes.get(item.mesh.0 as usize) else { continue };
            for s in &mesh.sections {
                offsets.push(uniforms.len() as u32);
                // Lightmaps modulate at twice their value; a mesh's own light does not.
                let scale = if item.lightmap.is_some() { self.lightmap_scale } else { 1.0 };
                uniforms.extend(globals_bytes(
                    view_proj,
                    item.transform,
                    view,
                    item.alpha_test.unwrap_or(s.masked),
                    item.tint,
                    scale,
                    item.env_map,
                    scene.camera.position,
                    &item.lights,
                    NO_CLIP,
                    [s.pan[0] * scene.time, s.pan[1] * scene.time],
                ));
                uniforms.resize(uniforms.len().div_ceil(GLOBALS_STRIDE as usize) * GLOBALS_STRIDE as usize, 0);
            }
        }
        // The meshes drawn over the world share the scene's camera; only the depth test is
        // dropped, so they never disappear into what stands in front of them.
        let foreground_first = offsets.len();
        for item in &scene.foreground {
            let Some(mesh) = self.meshes.get(item.mesh.0 as usize) else { continue };
            for s in &mesh.sections {
                offsets.push(uniforms.len() as u32);
                let scale = if item.lightmap.is_some() { self.lightmap_scale } else { 1.0 };
                uniforms.extend(globals_bytes(
                    view_proj,
                    item.transform,
                    view,
                    item.alpha_test.unwrap_or(s.masked),
                    item.tint,
                    scale,
                    item.env_map,
                    scene.camera.position,
                    &item.lights,
                    NO_CLIP,
                    [s.pan[0] * scene.time, s.pan[1] * scene.time],
                ));
                uniforms.resize(uniforms.len().div_ceil(GLOBALS_STRIDE as usize) * GLOBALS_STRIDE as usize, 0);
            }
        }
        // Each mirror shows the world from its own reflected point of view, which needs a set
        // of slots of its own: one per section drawn reflected, then one for its own surface.
        struct MirrorSlots {
            sky: usize,
            backdrop: usize,
            reflected: usize,
            sprites: usize,
            surface: usize,
        }
        let mut mirror_slots: Vec<MirrorSlots> = Vec::new();
        for (item, plane) in &scene.mirrors {
            // Only what is on the camera's side of the mirror is in front of it to be shown.
            let side = math::dot([plane[0], plane[1], plane[2]], scene.camera.position) - plane[3];
            let clip = if side < 0.0 { [-plane[0], -plane[1], -plane[2], -plane[3]] } else { *plane };
            let reflect = math::reflection(*plane);
            let view_r = math::mul(view, reflect);
            let view_proj_r = math::mul(math::perspective(scene.camera.fov_y_deg, aspect, scene.camera.near, scene.camera.far), view_r);
            // The sky, reflected as the main view draws it: from the sky zone's own point of
            // view, about the mirror's plane carried along with the eye into the sky zone.
            let sky = offsets.len();
            if let Some((item, at)) = &scene.sky {
                let n = [plane[0], plane[1], plane[2]];
                let moved = plane[3] + math::dot(n, [at[0] - scene.camera.position[0], at[1] - scene.camera.position[1], at[2] - scene.camera.position[2]]);
                let sky_view = math::mul(math::view(*at, scene.camera.rotation), math::reflection([n[0], n[1], n[2], moved]));
                let sky_view_proj = math::mul(math::perspective(scene.camera.fov_y_deg, aspect, scene.camera.near, scene.camera.far), sky_view);
                if let Some(mesh) = self.meshes.get(item.mesh.0 as usize) {
                    for s in &mesh.sections {
                        offsets.push(uniforms.len() as u32);
                        let scale = if item.lightmap.is_some() { self.lightmap_scale } else { 1.0 };
                        uniforms.extend(globals_bytes(
                            sky_view_proj,
                            item.transform,
                            sky_view,
                            item.alpha_test.unwrap_or(s.masked),
                            item.tint,
                            scale,
                            item.env_map,
                            *at,
                            &item.lights,
                            NO_CLIP,
                            [s.pan[0] * scene.time, s.pan[1] * scene.time],
                        ));
                        uniforms.resize(uniforms.len().div_ceil(GLOBALS_STRIDE as usize) * GLOBALS_STRIDE as usize, 0);
                    }
                }
            }
            // The windows onto it, reflected with the rest of the world.
            let backdrop = offsets.len();
            if let Some(item) = &scene.backdrop {
                if let Some(mesh) = self.meshes.get(item.mesh.0 as usize) {
                    for s in &mesh.sections {
                        offsets.push(uniforms.len() as u32);
                        uniforms.extend(globals_bytes(
                            view_proj_r,
                            item.transform,
                            view_r,
                            item.alpha_test.unwrap_or(s.masked),
                            item.tint,
                            1.0,
                            false,
                            scene.camera.position,
                            &item.lights,
                            clip,
                            [s.pan[0] * scene.time, s.pan[1] * scene.time],
                        ));
                        uniforms.resize(uniforms.len().div_ceil(GLOBALS_STRIDE as usize) * GLOBALS_STRIDE as usize, 0);
                    }
                }
            }
            let reflected = offsets.len();
            for item in &scene.items {
                let Some(mesh) = self.meshes.get(item.mesh.0 as usize) else { continue };
                for s in &mesh.sections {
                    offsets.push(uniforms.len() as u32);
                    let scale = if item.lightmap.is_some() { self.lightmap_scale } else { 1.0 };
                    uniforms.extend(globals_bytes(
                        view_proj_r,
                        item.transform,
                        view_r,
                        item.alpha_test.unwrap_or(s.masked),
                        item.tint,
                        scale,
                        item.env_map,
                        scene.camera.position,
                        &item.lights,
                        clip,
                        [s.pan[0] * scene.time, s.pan[1] * scene.time],
                    ));
                    uniforms.resize(uniforms.len().div_ceil(GLOBALS_STRIDE as usize) * GLOBALS_STRIDE as usize, 0);
                }
            }
            // The particles reflected, which is one slot of its own: a mirror has to show
            // them as much as it shows the world.
            let sprites = offsets.len();
            offsets.push(uniforms.len() as u32);
            uniforms.extend(globals_bytes(
                view_proj_r,
                math::IDENTITY,
                view_r,
                false,
                [1.0; 3],
                1.0,
                false,
                scene.camera.position,
                &[MeshLight::default(); MESH_LIGHTS],
                clip,
                [0.0; 2],
            ));
            uniforms.resize(uniforms.len().div_ceil(GLOBALS_STRIDE as usize) * GLOBALS_STRIDE as usize, 0);
            // And one more per section of the mirror surface, drawn with the ordinary camera.
            let surface = offsets.len();
            if let Some(mesh) = self.meshes.get(item.mesh.0 as usize) {
                for s in &mesh.sections {
                    offsets.push(uniforms.len() as u32);
                    uniforms.extend(globals_bytes(
                        view_proj,
                        item.transform,
                        view,
                        false,
                        item.tint,
                        1.0,
                        false,
                        scene.camera.position,
                        &item.lights,
                        NO_CLIP,
                        [s.pan[0] * scene.time, s.pan[1] * scene.time],
                    ));
                    uniforms.resize(uniforms.len().div_ceil(GLOBALS_STRIDE as usize) * GLOBALS_STRIDE as usize, 0);
                }
            }
            mirror_slots.push(MirrorSlots { sky, backdrop, reflected, sprites, surface });
        }
        // The particles all share one slot: the ordinary camera, and no transform of their own.
        let sprite_slot = offsets.len();
        if !scene.sprites.is_empty() {
            offsets.push(uniforms.len() as u32);
            uniforms.extend(globals_bytes(
                view_proj,
                math::IDENTITY,
                view,
                false,
                [1.0; 3],
                1.0,
                false,
                scene.camera.position,
                &[MeshLight::default(); MESH_LIGHTS],
                NO_CLIP,
                [0.0; 2],
            ));
            uniforms.resize(uniforms.len().div_ceil(GLOBALS_STRIDE as usize) * GLOBALS_STRIDE as usize, 0);
        }
        let mut sprites = Vec::with_capacity(scene.sprites.len() * SPRITE_SIZE as usize);
        for sp in &scene.sprites {
            sprites.extend(sp.position.iter().chain(&[sp.spin]).flat_map(|v| v.to_le_bytes()));
            sprites.extend([sp.size[0], sp.size[1], 0.0, 0.0].iter().flat_map(|v| v.to_le_bytes()));
            sprites.extend(sp.color.iter().flat_map(|v| v.to_le_bytes()));
            let facing = sp.facing.unwrap_or([0.0, 0.0, 1.0]);
            let lying = sp.facing.is_some() as u32 as f32;
            sprites.extend(facing.iter().chain(&[lying]).flat_map(|v| v.to_le_bytes()));
        }
        if sprites.len() as u64 > self.sprite_instances.size() {
            self.sprite_instances = self.device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("sprites"),
                size: (sprites.len() as u64).next_power_of_two(),
                usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
        }
        if !sprites.is_empty() {
            self.queue.write_buffer(&self.sprite_instances, 0, &sprites);
        }
        let mut overlay = Vec::with_capacity(scene.overlay.len() * QUAD_SIZE as usize);
        for q in &scene.overlay {
            overlay.extend(quad_bytes(q, width, height));
        }
        if overlay.len() as u64 > self.overlay_instances.size() {
            // Text makes a rectangle per glyph, so the buffer grows with whatever is on screen.
            self.overlay_instances = self.device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("overlay"),
                size: (overlay.len() as u64).next_power_of_two(),
                usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
        }
        if !overlay.is_empty() {
            self.queue.write_buffer(&self.overlay_instances, 0, &overlay);
        }
        if uniforms.len() as u64 > self.globals.size() {
            return Err(format!("too many draws for the uniform buffer ({} sections)", offsets.len()));
        }
        self.queue.write_buffer(&self.globals, 0, &uniforms);

        let frame = match &self.target {
            Target::Offscreen { color, .. } => None.or(Some(color.create_view(&wgpu::TextureViewDescriptor::default()))),
            Target::Surface { .. } => None,
        };
        let surface_frame = match &self.target {
            Target::Surface { surface, .. } => match surface.get_current_texture() {
                wgpu::CurrentSurfaceTexture::Success(t) | wgpu::CurrentSurfaceTexture::Suboptimal(t) => Some(t),
                other => return Err(format!("surface texture unavailable: {other:?}")),
            },
            Target::Offscreen { .. } => None,
        };
        let view = match (&frame, &surface_frame) {
            (Some(v), _) => v.clone(),
            (_, Some(f)) => f.texture.create_view(&wgpu::TextureViewDescriptor::default()),
            _ => return Err("no render target".into()),
        };

        self.queue.write_buffer(&self.post_globals, 0, &[scene.gamma.max(0.1), 0.0, 0.0, 0.0].map(f32::to_le_bytes).concat());
        let scene_view = self.scene_color.create_view(&wgpu::TextureViewDescriptor::default());
        let mut encoder = self.device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("grim") });
        {
            let c = scene.clear_color;
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("grim"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &scene_view,
                    resolve_target: None,
                    depth_slice: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color {
                            r: c[0] as f64,
                            g: c[1] as f64,
                            b: c[2] as f64,
                            a: c[3] as f64,
                        }),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                    view: &self.depth,
                    depth_ops: Some(wgpu::Operations { load: wgpu::LoadOp::Clear(1.0), store: wgpu::StoreOp::Store }),
                    stencil_ops: None,
                }),
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            // The backdrop goes first, from the sky zone's own point of view.
            let mut current = None;
            if let Some((item, _)) = &scene.sky {
                if let Some(mesh) = self.meshes.get(item.mesh.0 as usize) {
                    pass.set_vertex_buffer(0, mesh.vertices.slice(..));
                    pass.set_index_buffer(mesh.indices.slice(..), wgpu::IndexFormat::Uint32);
                    for (i, s) in mesh.sections.iter().enumerate() {
                        let key = (item.blend.unwrap_or(s.blend), s.two_sided);
                        pass.set_pipeline(&self.sky_pipelines[&key]);
                        let tex = s.texture.unwrap_or(self.white);
                        let baked = item.lightmap.unwrap_or(self.white);
                        if let (Some(t), Some(l)) = (self.textures.get(tex.0 as usize), self.textures.get(baked.0 as usize)) {
                            pass.set_bind_group(0, &self.globals_group, &[offsets[i]]);
                            pass.set_bind_group(1, &t.bind_group, &[]);
                            pass.set_bind_group(2, &l.bind_group, &[]);
                            pass.draw_indexed(s.first_index..s.first_index + s.index_count, 0, 0..1);
                        }
                    }
                }
            }
            // Then the windows onto it, which write depth only: the sky stays, the level's own
            // geometry behind them goes.
            if let Some(item) = &scene.backdrop {
                if let Some(mesh) = self.meshes.get(item.mesh.0 as usize) {
                    pass.set_vertex_buffer(0, mesh.vertices.slice(..));
                    pass.set_index_buffer(mesh.indices.slice(..), wgpu::IndexFormat::Uint32);
                    pass.set_pipeline(&self.backdrop_pipeline);
                    for (i, s) in mesh.sections.iter().enumerate() {
                        let tex = s.texture.unwrap_or(self.white);
                        let baked = item.lightmap.unwrap_or(self.white);
                        if let (Some(t), Some(l)) = (self.textures.get(tex.0 as usize), self.textures.get(baked.0 as usize)) {
                            pass.set_bind_group(0, &self.globals_group, &[offsets[sky_slots + i]]);
                            pass.set_bind_group(1, &t.bind_group, &[]);
                            pass.set_bind_group(2, &l.bind_group, &[]);
                            pass.draw_indexed(s.first_index..s.first_index + s.index_count, 0, 0..1);
                        }
                    }
                }
            }
            // Opaque geometry first, then the blended surfaces over it.
            for pass_blend in [true, false] {
                let mut slot = sky_slots + backdrop_slots;
                for item in &scene.items {
                    let Some(mesh) = self.meshes.get(item.mesh.0 as usize) else { continue };
                    let mut bound = false;
                    for s in &mesh.sections {
                        let blend = item.blend.unwrap_or(s.blend);
                        let opaque = blend == Blend::Opaque;
                        let draw = opaque == pass_blend;
                        if draw {
                            if !bound {
                                pass.set_vertex_buffer(0, mesh.vertices.slice(..));
                                pass.set_index_buffer(mesh.indices.slice(..), wgpu::IndexFormat::Uint32);
                                bound = true;
                            }
                            let key = (blend, s.two_sided);
                            if current != Some(key) {
                                pass.set_pipeline(&self.pipelines[&key]);
                                current = Some(key);
                            }
                            let skin = item.skins.get(s.material as usize).copied().flatten();
                            let tex = skin.or(s.texture).unwrap_or(self.white);
                            let baked = item.lightmap.unwrap_or(self.white);
                            if let (Some(t), Some(l)) = (self.textures.get(tex.0 as usize), self.textures.get(baked.0 as usize)) {
                                pass.set_bind_group(0, &self.globals_group, &[offsets[slot]]);
                                pass.set_bind_group(1, &t.bind_group, &[]);
                                pass.set_bind_group(2, &l.bind_group, &[]);
                                pass.draw_indexed(s.first_index..s.first_index + s.index_count, 0, 0..1);
                            }
                        }
                        slot += 1;
                    }
                }
            }
            // Over the finished world, ignoring what stands in front: the sky pipelines are the
            // ones built without a depth test, which is exactly what this wants.
            if !scene.foreground.is_empty() {
                let mut slot = foreground_first;
                for item in &scene.foreground {
                    let Some(mesh) = self.meshes.get(item.mesh.0 as usize) else { continue };
                    pass.set_vertex_buffer(0, mesh.vertices.slice(..));
                    pass.set_index_buffer(mesh.indices.slice(..), wgpu::IndexFormat::Uint32);
                    for s in &mesh.sections {
                        let blend = item.blend.unwrap_or(s.blend);
                        pass.set_pipeline(&self.sky_pipelines[&(blend, s.two_sided)]);
                        let skin = item.skins.get(s.material as usize).copied().flatten();
                        let tex = skin.or(s.texture).unwrap_or(self.white);
                        let baked = item.lightmap.unwrap_or(self.white);
                        if let (Some(t), Some(l)) = (self.textures.get(tex.0 as usize), self.textures.get(baked.0 as usize)) {
                            pass.set_bind_group(0, &self.globals_group, &[offsets[slot]]);
                            pass.set_bind_group(1, &t.bind_group, &[]);
                            pass.set_bind_group(2, &l.bind_group, &[]);
                            pass.draw_indexed(s.first_index..s.first_index + s.index_count, 0, 0..1);
                        }
                        slot += 1;
                    }
                }
            }

            // Particles go over the finished world, grouped by the texture they share.
            if !scene.sprites.is_empty() {
                pass.set_vertex_buffer(0, self.sprite_instances.slice(..));
                pass.set_bind_group(0, &self.globals_group, &[offsets[sprite_slot]]);
                let key = |sp: &grim_render::Sprite| (sp.texture.unwrap_or(self.white), sp.blend, sp.through);
                let mut run = 0u32;
                while (run as usize) < scene.sprites.len() {
                    let (texture, blend, through) = key(&scene.sprites[run as usize]);
                    let mut end = run + 1;
                    while (end as usize) < scene.sprites.len() && key(&scene.sprites[end as usize]) == (texture, blend, through) {
                        end += 1;
                    }
                    let set = if through { &self.sprites_through } else { &self.sprites };
                    if let (Some(t), Some(pipeline)) = (self.textures.get(texture.0 as usize), set.get(&blend)) {
                        pass.set_pipeline(pipeline);
                        pass.set_bind_group(1, &t.bind_group, &[]);
                        pass.draw(0..4, run..end);
                    }
                    run = end;
                }
            }
            // The game's own drawing goes over the scene, and over the mirrors, so it waits
            // for its own pass further down.
            if false {
                pass.set_vertex_buffer(0, self.overlay_instances.slice(..));
                let key = |q: &Quad| (q.texture.unwrap_or(self.white), q.blend);
                let mut run = 0u32;
                while (run as usize) < scene.overlay.len() {
                    let (texture, blend) = key(&scene.overlay[run as usize]);
                    let mut end = run + 1;
                    while (end as usize) < scene.overlay.len() && key(&scene.overlay[end as usize]) == (texture, blend) {
                        end += 1;
                    }
                    if let (Some(t), Some(pipeline)) = (self.textures.get(texture.0 as usize), self.overlay.get(&blend)) {
                        pass.set_pipeline(pipeline);
                        pass.set_bind_group(0, &t.bind_group, &[]);
                        pass.draw(0..4, run..end);
                    }
                    run = end;
                }
            }
        }

        // Each mirror is drawn after the world: first the world from its own reflected point
        // of view, into a picture of its own, then the surface sampling that picture where it
        // lands on the screen. One picture is reused, so the pair runs once per mirror.
        for (i, (item, _)) in scene.mirrors.iter().enumerate() {
            let Some(slots) = mirror_slots.get(i) else { continue };
            let Some(mesh) = self.meshes.get(item.mesh.0 as usize) else { continue };
            {
                let c = scene.clear_color;
                let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                    label: Some("reflection"),
                    color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                        view: &self.reflection,
                        resolve_target: None,
                        depth_slice: None,
                        ops: wgpu::Operations {
                            load: wgpu::LoadOp::Clear(wgpu::Color { r: c[0] as f64, g: c[1] as f64, b: c[2] as f64, a: c[3] as f64 }),
                            store: wgpu::StoreOp::Store,
                        },
                    })],
                    depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                        view: &self.reflection_depth,
                        depth_ops: Some(wgpu::Operations { load: wgpu::LoadOp::Clear(1.0), store: wgpu::StoreOp::Store }),
                        stencil_ops: None,
                    }),
                    timestamp_writes: None,
                    occlusion_query_set: None,
                    multiview_mask: None,
                });
                // The backdrop first, then the windows onto it, as in the main view.
                if let Some((item, _)) = &scene.sky {
                    if let Some(mesh) = self.meshes.get(item.mesh.0 as usize) {
                        pass.set_vertex_buffer(0, mesh.vertices.slice(..));
                        pass.set_index_buffer(mesh.indices.slice(..), wgpu::IndexFormat::Uint32);
                        for (i, s) in mesh.sections.iter().enumerate() {
                            pass.set_pipeline(&self.mirrored_sky_pipelines[&(item.blend.unwrap_or(s.blend), s.two_sided)]);
                            let tex = s.texture.unwrap_or(self.white);
                            let baked = item.lightmap.unwrap_or(self.white);
                            if let (Some(t), Some(l)) = (self.textures.get(tex.0 as usize), self.textures.get(baked.0 as usize)) {
                                pass.set_bind_group(0, &self.globals_group, &[offsets[slots.sky + i]]);
                                pass.set_bind_group(1, &t.bind_group, &[]);
                                pass.set_bind_group(2, &l.bind_group, &[]);
                                pass.draw_indexed(s.first_index..s.first_index + s.index_count, 0, 0..1);
                            }
                        }
                    }
                }
                if let Some(item) = &scene.backdrop {
                    if let Some(mesh) = self.meshes.get(item.mesh.0 as usize) {
                        pass.set_vertex_buffer(0, mesh.vertices.slice(..));
                        pass.set_index_buffer(mesh.indices.slice(..), wgpu::IndexFormat::Uint32);
                        pass.set_pipeline(&self.mirrored_backdrop_pipeline);
                        for (i, s) in mesh.sections.iter().enumerate() {
                            let tex = s.texture.unwrap_or(self.white);
                            let baked = item.lightmap.unwrap_or(self.white);
                            if let (Some(t), Some(l)) = (self.textures.get(tex.0 as usize), self.textures.get(baked.0 as usize)) {
                                pass.set_bind_group(0, &self.globals_group, &[offsets[slots.backdrop + i]]);
                                pass.set_bind_group(1, &t.bind_group, &[]);
                                pass.set_bind_group(2, &l.bind_group, &[]);
                                pass.draw_indexed(s.first_index..s.first_index + s.index_count, 0, 0..1);
                            }
                        }
                    }
                }
                for pass_blend in [true, false] {
                    let mut at = slots.reflected;
                    for item in &scene.items {
                        let Some(mesh) = self.meshes.get(item.mesh.0 as usize) else { continue };
                        let mut bound = false;
                        for s in &mesh.sections {
                            let blend = item.blend.unwrap_or(s.blend);
                            if (blend == Blend::Opaque) == pass_blend {
                                if !bound {
                                    pass.set_vertex_buffer(0, mesh.vertices.slice(..));
                                    pass.set_index_buffer(mesh.indices.slice(..), wgpu::IndexFormat::Uint32);
                                    bound = true;
                                }
                                pass.set_pipeline(&self.mirrored_pipelines[&(blend, s.two_sided)]);
                                let skin = item.skins.get(s.material as usize).copied().flatten();
                                let tex = skin.or(s.texture).unwrap_or(self.white);
                                let baked = item.lightmap.unwrap_or(self.white);
                                if let (Some(t), Some(l)) = (self.textures.get(tex.0 as usize), self.textures.get(baked.0 as usize)) {
                                    pass.set_bind_group(0, &self.globals_group, &[offsets[at]]);
                                    pass.set_bind_group(1, &t.bind_group, &[]);
                                    pass.set_bind_group(2, &l.bind_group, &[]);
                                    pass.draw_indexed(s.first_index..s.first_index + s.index_count, 0, 0..1);
                                }
                            }
                            at += 1;
                        }
                    }
                }
                // The particles go in the reflection too, over the world it shows.
                if !scene.sprites.is_empty() {
                    pass.set_vertex_buffer(0, self.sprite_instances.slice(..));
                    pass.set_bind_group(0, &self.globals_group, &[offsets[slots.sprites]]);
                    let key = |sp: &grim_render::Sprite| (sp.texture.unwrap_or(self.white), sp.blend);
                    let mut run = 0u32;
                    while (run as usize) < scene.sprites.len() {
                        let (texture, blend) = key(&scene.sprites[run as usize]);
                        let mut end = run + 1;
                        while (end as usize) < scene.sprites.len() && key(&scene.sprites[end as usize]) == (texture, blend) {
                            end += 1;
                        }
                        if let (Some(t), Some(pipeline)) = (self.textures.get(texture.0 as usize), self.sprites.get(&blend)) {
                            pass.set_pipeline(pipeline);
                            pass.set_bind_group(1, &t.bind_group, &[]);
                            pass.draw(0..4, run..end);
                        }
                        run = end;
                    }
                }
            }
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("grim mirror"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &scene_view,
                    resolve_target: None,
                    depth_slice: None,
                    ops: wgpu::Operations { load: wgpu::LoadOp::Load, store: wgpu::StoreOp::Store },
                })],
                depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                    view: &self.depth,
                    depth_ops: Some(wgpu::Operations { load: wgpu::LoadOp::Load, store: wgpu::StoreOp::Store }),
                    stencil_ops: None,
                }),
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            pass.set_vertex_buffer(0, mesh.vertices.slice(..));
            pass.set_index_buffer(mesh.indices.slice(..), wgpu::IndexFormat::Uint32);
            pass.set_pipeline(&self.mirror_pipeline);
            for (n, s) in mesh.sections.iter().enumerate() {
                let baked = item.lightmap.unwrap_or(self.white);
                if let Some(l) = self.textures.get(baked.0 as usize) {
                    pass.set_bind_group(0, &self.globals_group, &[offsets[slots.surface + n]]);
                    pass.set_bind_group(1, &self.reflection_group, &[]);
                    pass.set_bind_group(2, &l.bind_group, &[]);
                    pass.draw_indexed(s.first_index..s.first_index + s.index_count, 0, 0..1);
                }
            }
        }
        // The screen flash takes the world, and only the world: what the game draws over it
        // (the hourglass of a skipped cutscene among it) stays as it is.
        if scene.flash != [1.0, 0.0, 0.0, 0.0] {
            let fog = [scene.flash[1], scene.flash[2], scene.flash[3], 0.0];
            self.queue.write_buffer(&self.flash_globals, 0, &fog.map(f32::to_le_bytes).concat());
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("grim flash"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &scene_view,
                    resolve_target: None,
                    depth_slice: None,
                    ops: wgpu::Operations { load: wgpu::LoadOp::Load, store: wgpu::StoreOp::Store },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            let scale = scene.flash[0].max(0.0) as f64;
            pass.set_blend_constant(wgpu::Color { r: scale, g: scale, b: scale, a: 1.0 });
            pass.set_pipeline(&self.flash);
            pass.set_bind_group(0, &self.flash_group, &[]);
            pass.draw(0..3, 0..1);
        }
        // The game's own drawing goes over everything the world put down.
        if !scene.overlay.is_empty() {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("grim overlay"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &scene_view,
                    resolve_target: None,
                    depth_slice: None,
                    ops: wgpu::Operations { load: wgpu::LoadOp::Load, store: wgpu::StoreOp::Store },
                })],
                depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                    view: &self.depth,
                    depth_ops: Some(wgpu::Operations { load: wgpu::LoadOp::Load, store: wgpu::StoreOp::Store }),
                    stencil_ops: None,
                }),
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            pass.set_vertex_buffer(0, self.overlay_instances.slice(..));
            let key = |q: &Quad| (q.texture.unwrap_or(self.white), q.blend);
            let mut run = 0u32;
            while (run as usize) < scene.overlay.len() {
                let (texture, blend) = key(&scene.overlay[run as usize]);
                let mut end = run + 1;
                while (end as usize) < scene.overlay.len() && key(&scene.overlay[end as usize]) == (texture, blend) {
                    end += 1;
                }
                if let (Some(t), Some(pipeline)) = (self.textures.get(texture.0 as usize), self.overlay.get(&blend)) {
                    pass.set_pipeline(pipeline);
                    pass.set_bind_group(0, &t.bind_group, &[]);
                    pass.draw(0..4, run..end);
                }
                run = end;
            }
        }
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("grim gamma"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    resolve_target: None,
                    depth_slice: None,
                    ops: wgpu::Operations { load: wgpu::LoadOp::Clear(wgpu::Color::BLACK), store: wgpu::StoreOp::Store },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            pass.set_pipeline(&self.post);
            pass.set_bind_group(0, &self.post_group, &[]);
            pass.set_bind_group(1, &self.scene_group, &[]);
            pass.draw(0..3, 0..1);
        }

        // Offscreen: copy the frame into a buffer so `capture` can read it.
        if let Target::Offscreen { color, width, height } = &self.target {
            let padded = (4 * *width).div_ceil(256) * 256;
            let buffer = self.capture_cache.entry((*width, *height)).or_insert_with(|| {
                self.device.create_buffer(&wgpu::BufferDescriptor {
                    label: Some("capture"),
                    size: padded as u64 * *height as u64,
                    usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
                    mapped_at_creation: false,
                })
            });
            encoder.copy_texture_to_buffer(
                wgpu::TexelCopyTextureInfo {
                    texture: color,
                    mip_level: 0,
                    origin: wgpu::Origin3d::ZERO,
                    aspect: wgpu::TextureAspect::All,
                },
                wgpu::TexelCopyBufferInfo {
                    buffer,
                    layout: wgpu::TexelCopyBufferLayout {
                        offset: 0,
                        bytes_per_row: Some(padded),
                        rows_per_image: Some(*height),
                    },
                },
                wgpu::Extent3d { width: *width, height: *height, depth_or_array_layers: 1 },
            );
        }

        self.queue.submit([encoder.finish()]);

        if let Target::Offscreen { width, height, .. } = &self.target {
            let (width, height) = (*width, *height);
            let padded = (4 * width).div_ceil(256) * 256;
            let buffer = &self.capture_cache[&(width, height)];
            let slice = buffer.slice(..);
            slice.map_async(wgpu::MapMode::Read, |_| {});
            self.device.poll(wgpu::PollType::wait_indefinitely()).map_err(|e| e.to_string())?;
            let mapped = slice.get_mapped_range().map_err(|e| e.to_string())?;
            let mut rgba = Vec::with_capacity((width * height * 4) as usize);
            for row in 0..height {
                let start = (row * padded) as usize;
                rgba.extend_from_slice(&mapped[start..start + (width * 4) as usize]);
            }
            drop(mapped);
            buffer.unmap();
            self.last_frame = Some(Image { width, height, rgba });
        }
        if let Some(f) = surface_frame {
            self.queue.present(f);
        }
        Ok(())
    }

    fn textures_drawn(&self) -> Option<&std::collections::HashSet<TextureId>> {
        Some(&self.drawn_textures)
    }

    fn capture(&mut self) -> Option<Image> {
        self.last_frame.take()
    }
}
