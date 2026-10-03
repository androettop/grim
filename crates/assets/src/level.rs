//! `Engine.Level`: actor list, URL, BSP and navigation network.

use grim_package::property::{read_object_ref, read_string};
use grim_package::{Error, ObjectRef, Reader, Result};

use crate::model::{read_array, section};
use crate::Ctx;

#[derive(Debug, Clone)]
pub struct Url {
    pub protocol: String,
    pub host: String,
    pub map: String,
    pub portal: String,
    pub options: Vec<String>,
    pub port: i32,
    pub valid: i32,
}

#[derive(Debug, Clone)]
pub struct ReachSpec {
    pub distance: i32,
    pub start: ObjectRef,
    pub end: ObjectRef,
    pub collision_radius: i32,
    pub collision_height: i32,
    pub reach_flags: i32,
    pub pruned: u8,
}

/// UE1 `MAX_TEXT_BLOCKS`.
pub const TEXT_BLOCKS: usize = 16;

#[derive(Debug, Clone)]
pub struct Level {
    /// `DbMax` of the actor list.
    pub actors_max: i32,
    /// Null slots are deleted actors.
    pub actors: Vec<ObjectRef>,
    pub url: Url,
    pub model: ObjectRef,
    pub reach_specs: Vec<ReachSpec>,
    pub time_seconds: f32,
    pub first_deleted: ObjectRef,
    pub text_blocks: [ObjectRef; TEXT_BLOCKS],
    pub travel_info: Vec<(String, String)>,
}

impl Level {
    pub fn parse(cx: &Ctx, r: &mut Reader) -> Result<Self> {
        let pkg = cx.pkg;
        let at = r.offset();
        let num = r.i32()?;
        let actors_max = r.i32()?;
        if num < 0 || num > actors_max || num as usize > r.remaining() {
            return Err(Error::invalid(at, format!("Level.Actors: num {num} / max {actors_max}")));
        }
        let actors = (0..num).map(|_| read_object_ref(pkg, r)).collect::<Result<_>>()?;
        let url = section(
            "url",
            (|| {
                Ok(Url {
                    protocol: read_string(r)?,
                    host: read_string(r)?,
                    map: read_string(r)?,
                    portal: read_string(r)?,
                    options: read_array(r, 1, read_string)?,
                    port: r.i32()?,
                    valid: r.i32()?,
                })
            })(),
        )?;
        let model = read_object_ref(pkg, r)?;
        let reach_specs = section(
            "reach_specs",
            read_array(r, 4 + 1 + 1 + 12 + 1, |r| {
                Ok(ReachSpec {
                    distance: r.i32()?,
                    start: read_object_ref(pkg, r)?,
                    end: read_object_ref(pkg, r)?,
                    collision_radius: r.i32()?,
                    collision_height: r.i32()?,
                    reach_flags: r.i32()?,
                    pruned: r.u8()?,
                })
            }),
        )?;
        let time_seconds = r.f32()?;
        let first_deleted = read_object_ref(pkg, r)?;
        let mut text_blocks = [ObjectRef::Null; TEXT_BLOCKS];
        for t in &mut text_blocks {
            *t = read_object_ref(pkg, r)?;
        }
        let travel_info = section("travel_info", read_array(r, 2, |r| Ok((read_string(r)?, read_string(r)?))))?;
        Ok(Self { actors_max, actors, url, model, reach_specs, time_seconds, first_deleted, text_blocks, travel_info })
    }
}
