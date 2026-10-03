//! Per-class serializers on top of `grim-package`.
//!
//! [`load_export`] dispatches on the export's class and requires the serializer to consume
//! the export exactly; leftovers are an error. Classes without a serializer that leave bytes
//! behind become [`Asset::Unparsed`].

pub mod animation;
pub mod bytecode;
pub mod font;
pub mod lazy;
pub mod level;
pub mod mesh;
pub mod model;
pub mod palette;
pub mod polys;
pub mod primitive;
pub mod procedural;
pub mod script;
pub mod sound;
pub mod text_buffer;
pub mod texture;

use grim_package::object::{read_object_base, ObjectBase};
use grim_package::{Error, ObjectRef, Package, Result, Unknown};

pub use animation::Animation;
pub use font::Font;
pub use level::Level;
pub use mesh::{LodMesh, Mesh, SkeletalMesh};
pub use model::Model;
pub use palette::Palette;
pub use polys::Polys;
pub use procedural::ProceduralTexture;
pub use sound::Sound;
pub use text_buffer::TextBuffer;
pub use texture::Texture;

/// Qualified class name of an export, e.g. `Engine.Texture`.
pub fn class_path(pkg: &Package, export: u32) -> String {
    let class = pkg.export(export).class;
    if class.is_null() {
        return "Core.Class".to_string();
    }
    let mut outer = class;
    let mut top = class;
    while !outer.is_null() {
        top = outer;
        outer = pkg.outer(outer);
    }
    let package = if top == class {
        // No outer: an exported class belongs to this package.
        match class {
            ObjectRef::Export(_) => pkg.name.as_str(),
            _ => "?",
        }
    } else {
        pkg.object_name(top)
    };
    format!("{package}.{}", pkg.object_name(class))
}

#[derive(Debug, Clone)]
pub enum Asset {
    Palette(Palette),
    Texture(Texture),
    ProceduralTexture(ProceduralTexture),
    Sound(Sound),
    Polys(Polys),
    Model(Box<Model>),
    Level(Box<Level>),
    LodMesh(Box<LodMesh>),
    SkeletalMesh(Box<SkeletalMesh>),
    Animation(Box<Animation>),
    Const(script::Const),
    Enum(script::Enum),
    Property(script::Property),
    Struct(Box<script::Struct>),
    Function(Box<script::Function>),
    State(Box<script::State>),
    Class(Box<script::Class>),
    Font(Font),
    TextBuffer(TextBuffer),
    /// Properties only (actors, etc.).
    Plain,
    /// No serializer yet; the bytes after the properties.
    Unparsed(Unknown),
}

#[derive(Debug, Clone)]
pub struct LoadedObject {
    pub class: String,
    pub base: ObjectBase,
    pub asset: Asset,
}

pub struct Ctx<'a> {
    pub pkg: &'a Package,
    pub export: u32,
    pub base: &'a ObjectBase,
}

impl Ctx<'_> {
    pub fn version(&self) -> u16 {
        self.pkg.version()
    }
}

pub fn load_export(pkg: &Package, export: u32) -> Result<LoadedObject> {
    let class = class_path(pkg, export);
    let mut r = pkg.export_reader(export);
    let base = read_object_base(pkg, export, &mut r)?;
    let cx = Ctx { pkg, export, base: &base };
    let asset = match class.as_str() {
        "Engine.Palette" => Asset::Palette(Palette::parse(&cx, &mut r)?),
        "Engine.Texture" => Asset::Texture(Texture::parse(&cx, &mut r)?),
        c if procedural::ProceduralKind::from_class(c).is_some() => {
            Asset::ProceduralTexture(ProceduralTexture::parse(&cx, c, &mut r)?)
        }
        "Engine.Sound" => Asset::Sound(Sound::parse(&cx, &mut r)?),
        "Engine.LodMesh" => Asset::LodMesh(Box::new(LodMesh::parse(&cx, &mut r)?)),
        "Engine.SkeletalMesh" => Asset::SkeletalMesh(Box::new(SkeletalMesh::parse(&cx, &mut r)?)),
        "Engine.Animation" => Asset::Animation(Box::new(Animation::parse(&cx, &mut r)?)),
        "Core.Struct" => Asset::Struct(Box::new(script::Struct::parse(&cx, &mut r)?)),
        "Core.Function" => Asset::Function(Box::new(script::Function::parse(&cx, &mut r)?)),
        "Core.State" => Asset::State(Box::new(script::State::parse(&cx, &mut r)?)),
        "Core.Class" => Asset::Class(Box::new(script::Class::parse(&cx, &mut r)?)),
        "Core.Const" => Asset::Const(script::Const::parse(&cx, &mut r)?),
        "Core.Enum" => Asset::Enum(script::Enum::parse(&cx, &mut r)?),
        c if script::property_class(c).is_some() => Asset::Property(script::Property::parse(&cx, c, &mut r)?),
        "Engine.Level" => Asset::Level(Box::new(Level::parse(&cx, &mut r)?)),
        "Engine.Model" => Asset::Model(Box::new(Model::parse(&cx, &mut r)?)),
        "Engine.Polys" => Asset::Polys(Polys::parse(&cx, &mut r)?),
        "Engine.Font" => Asset::Font(Font::parse(&cx, &mut r)?),
        "Core.TextBuffer" => Asset::TextBuffer(TextBuffer::parse(&cx, &mut r)?),
        _ if r.is_at_end() => Asset::Plain,
        _ => Asset::Unparsed(Unknown::new(r.offset(), r.rest())),
    };
    r.expect_end(&class)?;
    Ok(LoadedObject { class, base, asset })
}

pub(crate) fn check<T: PartialEq + std::fmt::Debug>(at: usize, what: &str, got: T, want: T) -> Result<()> {
    if got == want {
        Ok(())
    } else {
        Err(Error::invalid(at, format!("{what}: read {got:?}, expected {want:?}")))
    }
}

