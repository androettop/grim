//! Procedural textures (`Fire.*Texture`, `Engine.ScriptedTexture`): a regular `Texture` whose
//! mips are usually empty (generated at runtime). `FireTexture` adds its sparks.

use grim_package::{Reader, Result};

use crate::texture::{MipData, Texture};
use crate::{check, Ctx};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProceduralKind {
    Fire,
    Wave,
    Wet,
    Ice,
    Scripted,
}

impl ProceduralKind {
    pub fn from_class(class: &str) -> Option<Self> {
        Some(match class {
            "Fire.FireTexture" => Self::Fire,
            "Fire.WaveTexture" => Self::Wave,
            "Fire.WetTexture" => Self::Wet,
            "Fire.IceTexture" => Self::Ice,
            "Engine.ScriptedTexture" => Self::Scripted,
            _ => return None,
        })
    }
}

/// `FSpark`, 8 bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Spark {
    pub kind: u8,
    pub heat: u8,
    pub x: u8,
    pub y: u8,
    pub byte_a: u8,
    pub byte_b: u8,
    pub byte_c: u8,
    pub byte_d: u8,
}

#[derive(Debug, Clone)]
pub struct ProceduralTexture {
    pub kind: ProceduralKind,
    pub texture: Texture,
    /// `FireTexture` only.
    pub sparks: Vec<Spark>,
}

impl ProceduralTexture {
    pub fn parse(cx: &Ctx, class: &str, r: &mut Reader) -> Result<Self> {
        let kind = ProceduralKind::from_class(class).expect("dispatched by from_class");
        let texture = Texture::parse_with(cx, r, MipData::MayBeEmpty)?;
        let sparks = if kind == ProceduralKind::Fire {
            let at = r.offset();
            let n = r.count(8)?;
            let sparks = (0..n)
                .map(|_| {
                    let [kind, heat, x, y, byte_a, byte_b, byte_c, byte_d] = r.array()?;
                    Ok(Spark { kind, heat, x, y, byte_a, byte_b, byte_c, byte_d })
                })
                .collect::<Result<Vec<_>>>()?;
            if let Some(num) = cx.base.int_property(cx.pkg, "NumSparks") {
                check(at, "spark count vs NumSparks", sparks.len() as i32, num)?;
            }
            sparks
        } else {
            Vec::new()
        };
        Ok(Self { kind, texture, sparks })
    }
}
