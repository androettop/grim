//! Serialization shared by every `UObject`: optional state frame + tagged properties.

use crate::error::Result;
use crate::flags::RF_HAS_STACK;
use crate::package::Package;
use crate::property::{read_object_ref, read_properties, Property};
use crate::reader::Reader;
use crate::tables::ObjectRef;

#[derive(Debug, Clone, PartialEq)]
pub struct StateFrame {
    pub node: ObjectRef,
    pub state_node: ObjectRef,
    pub probe_mask: u64,
    pub latent_action: u32,
    /// Present only when `node` is not null.
    pub code_offset: Option<i32>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ObjectBase {
    pub state_frame: Option<StateFrame>,
    /// `None` for `UClass` exports, which serialize their defaults later.
    pub properties: Option<Vec<Property>>,
}

pub fn read_object_base(pkg: &Package, export: u32, r: &mut Reader) -> Result<ObjectBase> {
    let e = pkg.export(export);
    let state_frame = if e.flags & RF_HAS_STACK != 0 {
        let node = read_object_ref(pkg, r)?;
        let state_node = read_object_ref(pkg, r)?;
        let probe_mask = r.u64()?;
        let latent_action = r.u32()?;
        let code_offset = if node.is_null() { None } else { Some(r.compact()?) };
        Some(StateFrame { node, state_node, probe_mask, latent_action, code_offset })
    } else {
        None
    };
    let properties = if e.class.is_null() { None } else { Some(read_properties(pkg, r)?) };
    Ok(ObjectBase { state_frame, properties })
}

impl ObjectBase {
    pub fn property<'a>(&'a self, pkg: &Package, name: &str, index: u32) -> Option<&'a crate::property::PropertyValue> {
        self.properties
            .iter()
            .flatten()
            .find(|p| p.array_index == index && pkg.name(p.name).eq_ignore_ascii_case(name))
            .map(|p| &p.value)
    }

    pub fn bool_property(&self, pkg: &Package, name: &str) -> Option<bool> {
        match self.property(pkg, name, 0)? {
            crate::property::PropertyValue::Bool(b) => Some(*b),
            _ => None,
        }
    }

    pub fn byte_property(&self, pkg: &Package, name: &str) -> Option<u8> {
        match self.property(pkg, name, 0)? {
            crate::property::PropertyValue::Byte(b) => Some(*b),
            _ => None,
        }
    }

    pub fn int_property(&self, pkg: &Package, name: &str) -> Option<i32> {
        match self.property(pkg, name, 0)? {
            crate::property::PropertyValue::Int(b) => Some(*b),
            _ => None,
        }
    }
}
