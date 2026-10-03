use grim_package::property::Color;
use grim_package::{Reader, Result};

use crate::Ctx;

#[derive(Debug, Clone)]
pub struct Palette {
    pub colors: Vec<Color>,
}

impl Palette {
    pub fn parse(_cx: &Ctx, r: &mut Reader) -> Result<Self> {
        let n = r.count(4)?;
        let colors = (0..n)
            .map(|_| {
                let [red, g, b, a] = r.array()?;
                Ok(Color { r: red, g, b, a })
            })
            .collect::<Result<_>>()?;
        Ok(Self { colors })
    }
}
