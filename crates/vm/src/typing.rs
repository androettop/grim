//! Static types of bytecode expressions, inferred from declarations. Used for typed zero values
//! (e.g. reading through a `None` context) and to validate the conversion token table.

use grim_assets::bytecode::Expr;
use grim_object::{ClassId, FuncId, NameId, ObjectKey, PropType, World};
use grim_package::ObjectRef;

#[derive(Debug, Clone, PartialEq)]
pub enum Ty {
    Byte,
    Int,
    Bool,
    Float,
    Object,
    Class,
    Name,
    Str,
    Struct(NameId),
    Array,
}

pub fn ty_of_prop(world: &World, t: &PropType) -> Ty {
    match t {
        PropType::Byte(_) => Ty::Byte,
        PropType::Int => Ty::Int,
        PropType::Bool => Ty::Bool,
        PropType::Float => Ty::Float,
        PropType::Object(_) => Ty::Object,
        PropType::Class(_) => Ty::Class,
        PropType::Name => Ty::Name,
        PropType::Str => Ty::Str,
        PropType::Struct(s) => Ty::Struct(world.structs[s.0 as usize].name),
        PropType::Array(_) => Ty::Array,
    }
}

/// Where an expression is evaluated: the function (or state) code and its class.
pub struct Scope {
    pub package: grim_object::PkgId,
    pub class: ClassId,
    pub function: Option<FuncId>,
}

pub struct Typer<'a> {
    pub world: &'a mut World,
    /// Native index to function, for `Expr::Native`.
    pub natives: &'a grim_object::FastMap<u16, FuncId>,
}

impl Typer<'_> {
    fn key(&mut self, s: &Scope, r: ObjectRef) -> Option<ObjectKey> {
        self.world.resolve_in(s.package, r).ok().flatten()
    }

    fn prop_ty(&mut self, s: &Scope, r: ObjectRef) -> Option<(Ty, PropType)> {
        let k = self.key(s, r)?;
        let p = self.world.prop(k)?.ty.clone();
        Some((ty_of_prop(self.world, &p), p))
    }

    /// Function a virtual call by name would reach from `class`, ignoring states.
    pub fn find_function(&self, mut class: ClassId, name: NameId) -> Option<FuncId> {
        loop {
            let c = self.world.class(class);
            if let Some(&f) = c.functions.get(&name) {
                return Some(f);
            }
            class = c.super_?;
        }
    }

    pub fn ret_ty(&self, f: FuncId) -> Option<Ty> {
        let f = self.world.function(f);
        f.ret.map(|r| ty_of_prop(self.world, &f.locals[r].ty))
    }

    /// Callee of a call expression, when statically known.
    pub fn callee(&mut self, s: &Scope, e: &Expr) -> Option<FuncId> {
        match e {
            Expr::FinalFunction { function, .. } => {
                let k = self.key(s, *function)?;
                self.world.link_function(k).ok()
            }
            Expr::VirtualFunction { name, .. } | Expr::GlobalFunction { name, .. } => {
                let pkg = self.world.package(s.package).clone();
                let n = self.world.name(&pkg, *name);
                self.find_function(s.class, n)
            }
            Expr::Native { index, .. } => self.natives.get(index).copied(),
            _ => None,
        }
    }

    pub fn ty(&mut self, s: &Scope, e: &Expr) -> Option<Ty> {
        Some(match e {
            Expr::LocalVariable(r) | Expr::InstanceVariable(r) | Expr::DefaultVariable(r) => self.prop_ty(s, *r)?.0,
            Expr::ArrayElement { array, .. } => self.ty(s, array)?,
            Expr::DynArrayElement { array, .. } => match self.var_prop(s, array)? {
                PropType::Array(inner) => ty_of_prop(self.world, &inner.ty),
                _ => return None,
            },
            Expr::StructMember { member, .. } => self.prop_ty(s, *member)?.0,
            Expr::BoolVariable(inner) => return self.ty(s, inner),
            Expr::Context { object, expr, .. } | Expr::ClassContext { object, expr, .. } => return self.context_ty(s, object, expr),
            Expr::IntConst(_) | Expr::IntZero | Expr::IntOne | Expr::IntConstByte(_) | Expr::DynArrayLength(_) => Ty::Int,
            Expr::FloatConst(_) => Ty::Float,
            Expr::StringConst(_) | Expr::UnicodeStringConst(_) => Ty::Str,
            Expr::ObjectConst(_) | Expr::NoObject | Expr::SelfRef | Expr::DynamicCast { .. } | Expr::New { .. } => Ty::Object,
            Expr::MetaCast { .. } => Ty::Class,
            Expr::NameConst(_) => Ty::Name,
            Expr::ByteConst(_) => Ty::Byte,
            Expr::True | Expr::False | Expr::StructCmpEq { .. } | Expr::StructCmpNe { .. } => Ty::Bool,
            Expr::VectorConst(_) => Ty::Struct(self.world.names.intern("Vector")),
            Expr::RotationConst(_) => Ty::Struct(self.world.names.intern("Rotator")),
            Expr::FinalFunction { .. } | Expr::VirtualFunction { .. } | Expr::GlobalFunction { .. } | Expr::Native { .. } => {
                let f = self.callee(s, e)?;
                self.ret_ty(f)?
            }
            _ => return None,
        })
    }

    /// Type of an expression evaluated in another object's context. Virtual calls are looked
    /// up in the declared class of the context expression.
    fn context_ty(&mut self, s: &Scope, object: &Expr, e: &Expr) -> Option<Ty> {
        match e {
            Expr::VirtualFunction { name, .. } | Expr::GlobalFunction { name, .. } => {
                let class = self.object_class(s, object)?;
                let pkg = self.world.package(s.package).clone();
                let n = self.world.name(&pkg, *name);
                let f = self.find_function(class, n)?;
                self.ret_ty(f)
            }
            other => self.ty(s, other),
        }
    }

    /// Static class of an object-valued expression.
    pub fn object_class(&mut self, s: &Scope, e: &Expr) -> Option<ClassId> {
        let class_of = |t: &mut Self, p: PropType| match p {
            PropType::Object(Some(grim_object::ClassRef::Key(k))) => t.world.link_class(k).ok(),
            PropType::Object(Some(grim_object::ClassRef::Intrinsic(n))) => t.world.intrinsic_class(n),
            _ => None,
        };
        match e {
            Expr::SelfRef => Some(s.class),
            Expr::DynamicCast { class, .. } => {
                let k = self.key(s, *class)?;
                self.world.link_class(k).ok()
            }
            Expr::FinalFunction { .. } | Expr::VirtualFunction { .. } | Expr::GlobalFunction { .. } | Expr::Native { .. } => {
                let f = self.callee(s, e)?;
                let fd = self.world.function(f);
                let ret = fd.locals[fd.ret?].ty.clone();
                class_of(self, ret)
            }
            Expr::Context { object, expr, .. } => match expr.as_ref() {
                Expr::VirtualFunction { name, .. } => {
                    let c = self.object_class(s, object)?;
                    let pkg = self.world.package(s.package).clone();
                    let n = self.world.name(&pkg, *name);
                    let f = self.find_function(c, n)?;
                    let fd = self.world.function(f);
                    let ret = fd.locals[fd.ret?].ty.clone();
                    class_of(self, ret)
                }
                other => {
                    let p = self.var_prop(s, other)?;
                    class_of(self, p)
                }
            },
            other => {
                let p = self.var_prop(s, other)?;
                class_of(self, p)
            }
        }
    }

    /// Declared property of a variable expression.
    pub fn var_prop(&mut self, s: &Scope, e: &Expr) -> Option<PropType> {
        match e {
            Expr::LocalVariable(r) | Expr::InstanceVariable(r) | Expr::DefaultVariable(r) => Some(self.prop_ty(s, *r)?.1),
            Expr::StructMember { member, .. } => Some(self.prop_ty(s, *member)?.1),
            Expr::Context { expr, .. } | Expr::ClassContext { expr, .. } => self.var_prop(s, expr),
            Expr::ArrayElement { array, .. } => self.var_prop(s, array),
            _ => None,
        }
    }
}
