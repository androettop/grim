use crate::names::NameId;
use crate::types::ObjectKey;

#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Byte(u8),
    Int(i32),
    Bool(bool),
    Float(f32),
    Name(NameId),
    Str(String),
    Object(Option<ObjectKey>),
    /// Field slots of the struct, laid out like `StructDef::fields`.
    Struct(Vec<Value>),
    Array(Vec<Value>),
}
