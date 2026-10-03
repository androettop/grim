//! Native functions, registered by path (`Package.Class.Function`).

pub mod core;
pub mod engine;

use std::collections::HashMap;

use grim_object::{FuncId, NameId, ObjectKey, Value};

use crate::vm::{fail, floats3, ints3, Vm, VmResult};

pub type NativeFn = fn(&mut Vm, &mut NativeCall) -> VmResult<Value>;

pub struct NativeCall {
    pub target: ObjectKey,
    pub func: FuncId,
    /// Parameter values; out parameters are written back from here.
    pub args: Vec<Value>,
    /// Whether each argument was passed (optional parameters may be omitted).
    pub present: Vec<bool>,
    /// For iterator natives: one row of argument values per iteration.
    pub iterations: Option<Vec<Vec<Value>>>,
}

impl NativeCall {
    pub fn int(&self, i: usize) -> VmResult<i32> {
        match &self.args[i] {
            Value::Int(v) => Ok(*v),
            Value::Byte(v) => Ok(*v as i32),
            v => fail(format!("argument {i}: expected int, got {v:?}")),
        }
    }
    pub fn byte(&self, i: usize) -> VmResult<u8> {
        match &self.args[i] {
            Value::Byte(v) => Ok(*v),
            v => fail(format!("argument {i}: expected byte, got {v:?}")),
        }
    }
    pub fn float(&self, i: usize) -> VmResult<f32> {
        match &self.args[i] {
            Value::Float(v) => Ok(*v),
            v => fail(format!("argument {i}: expected float, got {v:?}")),
        }
    }
    pub fn bool(&self, i: usize) -> VmResult<bool> {
        match &self.args[i] {
            Value::Bool(v) => Ok(*v),
            v => fail(format!("argument {i}: expected bool, got {v:?}")),
        }
    }
    pub fn str(&self, i: usize) -> VmResult<&str> {
        match &self.args[i] {
            Value::Str(v) => Ok(v),
            v => fail(format!("argument {i}: expected string, got {v:?}")),
        }
    }
    pub fn name(&self, i: usize) -> VmResult<NameId> {
        match &self.args[i] {
            Value::Name(v) => Ok(*v),
            v => fail(format!("argument {i}: expected name, got {v:?}")),
        }
    }
    pub fn obj(&self, i: usize) -> VmResult<Option<ObjectKey>> {
        match &self.args[i] {
            Value::Object(v) => Ok(*v),
            v => fail(format!("argument {i}: expected object, got {v:?}")),
        }
    }
    pub fn vec(&self, i: usize) -> VmResult<[f32; 3]> {
        floats3(&self.args[i])
    }
    pub fn rot(&self, i: usize) -> VmResult<[i32; 3]> {
        ints3(&self.args[i])
    }
    pub fn has(&self, i: usize) -> bool {
        self.present.get(i).copied().unwrap_or(false)
    }
}

#[derive(Debug, Clone, Default)]
pub struct MeshAnims {
    pub sequences: Vec<NameId>,
    pub bones: Vec<NameId>,
}

pub struct Natives {
    map: HashMap<&'static str, NativeFn>,
}

impl Natives {
    pub fn standard() -> Self {
        let mut n = Self { map: HashMap::new() };
        core::register(&mut n);
        engine::register(&mut n);
        engine::register_config(&mut n);
        engine::register_scripted_texture(&mut n);
        n
    }

    pub fn add(&mut self, path: &'static str, f: NativeFn) {
        self.map.insert(path, f);
    }

    pub fn get(&self, path: &str) -> Option<NativeFn> {
        self.map.get(path).copied()
    }

    /// Every path an implementation is registered under: what a diagnostic compares with the
    /// natives the scripts declare, so a registration under a name no class carries shows up.
    pub fn paths(&self) -> impl Iterator<Item = &'static str> + '_ {
        self.map.keys().copied()
    }

    pub fn len(&self) -> usize {
        self.map.len()
    }

    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }
}

/// The number the audio backend knows an actor by.
pub fn sound_owner(a: grim_object::ObjectKey) -> u64 {
    engine::sound_key(a)
}

/// The backend's id for a sound object, decoding it the first time.
pub fn sound_of(vm: &mut crate::vm::Vm, sound: grim_object::ObjectKey) -> Option<grim_audio::SoundId> {
    engine::sound_id(vm, sound)
}
