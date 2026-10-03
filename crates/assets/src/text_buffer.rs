use grim_package::property::read_string;
use grim_package::{Reader, Result};

use crate::Ctx;

#[derive(Debug, Clone)]
pub struct TextBuffer {
    pub pos: i32,
    pub top: i32,
    pub text: String,
}

impl TextBuffer {
    pub fn parse(_cx: &Ctx, r: &mut Reader) -> Result<Self> {
        Ok(Self { pos: r.i32()?, top: r.i32()?, text: read_string(r)? })
    }
}
