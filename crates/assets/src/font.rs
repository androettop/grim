use grim_package::property::read_object_ref;
use grim_package::{Error, ObjectRef, Reader, Result, Unknown};

use crate::Ctx;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FontCharacter {
    pub start_u: i32,
    pub start_v: i32,
    pub u_size: i32,
    pub v_size: i32,
}

#[derive(Debug, Clone)]
pub struct FontPage {
    pub texture: ObjectRef,
    pub characters: Vec<FontCharacter>,
}

#[derive(Debug, Clone)]
pub struct Font {
    pub pages: Vec<FontPage>,
    pub characters_per_page: i32,
    /// v79 only (v68 `UWindowFonts` ends at `characters_per_page`). Always 5 zero bytes; fonts
    /// with no pages (`HGame.ThaiFontSmall`, `JapFont*`, `SystemFont*`, ...) add an FString with
    /// a TrueType font name ("Tahoma", "PMingLiU", ...) and an i32 (12..24, height?).
    /// Unknown what tells both cases apart, so it is kept uninterpreted.
    pub kw_tail: Option<Unknown>,
}

/// First version with the KnowWonder tail (v68 lacks it; no v69..v78 files exist).
pub const FONT_KW_TAIL_VERSION: u16 = 79;

impl Font {
    pub fn parse(cx: &Ctx, r: &mut Reader) -> Result<Self> {
        let n = r.count(2)?;
        let pages = (0..n)
            .map(|_| {
                let texture = read_object_ref(cx.pkg, r)?;
                let chars = r.count(16)?;
                let characters = (0..chars)
                    .map(|_| {
                        Ok(FontCharacter { start_u: r.i32()?, start_v: r.i32()?, u_size: r.i32()?, v_size: r.i32()? })
                    })
                    .collect::<Result<_>>()?;
                Ok(FontPage { texture, characters })
            })
            .collect::<Result<_>>()?;
        let characters_per_page = r.i32()?;
        let kw_tail = if cx.version() >= FONT_KW_TAIL_VERSION {
            if r.is_at_end() {
                return Err(Error::invalid(r.offset(), "v79 Font without the KnowWonder tail"));
            }
            Some(Unknown::new(r.offset(), r.rest()))
        } else {
            None
        };
        Ok(Self { pages, characters_per_page, kw_tail })
    }
}
