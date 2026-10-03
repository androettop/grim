//! The object model: links classes, structs and enums across packages, computes property
//! layouts and defaults, and instantiates serialized objects.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use grim_assets::script::{Field, PropertyKind};
use grim_assets::{load_export, Asset};
use grim_package::property::{read_name, read_object_ref, read_string, Property as Tag, PropertyValue};
use grim_package::{Library, NameIndex, ObjectRef, Package, Reader};

use crate::names::{NameId, Names};
use crate::types::*;
use crate::value::Value;

/// Link or instantiation failure, with the object it happened in.
#[derive(Debug, Clone)]
pub struct LinkError(pub String);

impl std::fmt::Display for LinkError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

pub type Result<T> = std::result::Result<T, LinkError>;

fn err<T>(msg: impl Into<String>) -> Result<T> {
    Err(LinkError(msg.into()))
}

/// A live object built from a serialized one.
#[derive(Debug, Clone)]
pub struct Object {
    pub key: ObjectKey,
    pub class: ClassId,
    pub values: Vec<Value>,
}

pub struct World {
    pub lib: Library,
    pub names: Names,
    packages: Vec<Arc<Package>>,
    package_ids: HashMap<usize, PkgId>,
    pub classes: Vec<ClassDef>,
    class_ids: HashMap<ObjectKey, ClassId>,
    intrinsic_ids: HashMap<NameId, ClassId>,
    linking: HashSet<ObjectKey>,
    pub structs: Vec<StructDef>,
    struct_ids: HashMap<ObjectKey, StructId>,
    pub enums: Vec<EnumDef>,
    enum_ids: HashMap<ObjectKey, EnumId>,
    pub functions: Vec<FunctionDef>,
    function_ids: HashMap<ObjectKey, FuncId>,
    pub states: Vec<StateDef>,
    state_ids: HashMap<ObjectKey, StateId>,
    /// Every linked property (class member, struct field or function local) by its key.
    props: HashMap<ObjectKey, PropDef>,
    resolved: HashMap<(PkgId, ObjectRef), Option<ObjectKey>>,
    /// References a package the game does not have, left empty rather than dropped: what the
    /// engine could not load, for whoever is keeping the warnings.
    pub unresolved: Vec<String>,
}

fn field_of(a: &Asset) -> Option<&Field> {
    Some(match a {
        Asset::Property(p) => &p.field,
        Asset::Const(c) => &c.field,
        Asset::Enum(e) => &e.field,
        Asset::Struct(s) => &s.field,
        Asset::Function(f) => &f.struct_.field,
        Asset::State(s) => &s.struct_.field,
        _ => return None,
    })
}

impl World {
    pub fn new(lib: Library) -> Self {
        Self {
            lib,
            names: Names::default(),
            packages: Vec::new(),
            package_ids: HashMap::new(),
            classes: Vec::new(),
            class_ids: HashMap::new(),
            intrinsic_ids: HashMap::new(),
            linking: HashSet::new(),
            structs: Vec::new(),
            struct_ids: HashMap::new(),
            enums: Vec::new(),
            enum_ids: HashMap::new(),
            functions: Vec::new(),
            function_ids: HashMap::new(),
            states: Vec::new(),
            state_ids: HashMap::new(),
            props: HashMap::new(),
            resolved: HashMap::new(),
            unresolved: Vec::new(),
        }
    }

    pub fn pkg_id(&mut self, pkg: &Arc<Package>) -> PkgId {
        let ptr = Arc::as_ptr(pkg) as usize;
        if let Some(&id) = self.package_ids.get(&ptr) {
            return id;
        }
        let id = PkgId(self.packages.len() as u32);
        self.packages.push(pkg.clone());
        self.package_ids.insert(ptr, id);
        id
    }

    pub fn package(&self, id: PkgId) -> &Arc<Package> {
        &self.packages[id.0 as usize]
    }

    pub fn name(&mut self, pkg: &Package, n: NameIndex) -> NameId {
        self.names.intern(pkg.name(n))
    }

    /// `Package.Outer.Name` of a key, for messages.
    pub fn path(&self, key: ObjectKey) -> String {
        if key.package == INTRINSIC {
            return format!("<intrinsic>.{}", self.names.str(self.classes[key.export as usize].name));
        }
        self.package(key.package).path_name(ObjectRef::Export(key.export))
    }

    /// Resolves a reference made from `pkg` to the export it designates, in any package.
    pub fn resolve(&mut self, pkg: &Arc<Package>, r: ObjectRef) -> Result<Option<ObjectKey>> {
        if r.is_null() {
            return Ok(None);
        }
        let id = self.pkg_id(pkg);
        if let Some(&k) = self.resolved.get(&(id, r)) {
            return Ok(k);
        }
        let k = match self.lib.resolve(pkg, r) {
            Ok(h) => ObjectKey { package: self.pkg_id(&h.package), export: h.export },
            Err(_) if matches!(r, ObjectRef::Import(i) if pkg.name(pkg.import(i).class_name) == "Class") => {
                let name = pkg.object_name(r).to_string();
                ObjectKey { package: INTRINSIC, export: self.intrinsic(&name).0 }
            }
            Err(e) => return err(e),
        };
        self.resolved.insert((id, r), Some(k));
        Ok(Some(k))
    }

    /// Like [`resolve`](Self::resolve), from a package id.
    pub fn resolve_in(&mut self, pkg: PkgId, r: ObjectRef) -> Result<Option<ObjectKey>> {
        let p = self.package(pkg).clone();
        self.resolve(&p, r)
    }

    /// A linked property by its key.
    pub fn prop(&self, key: ObjectKey) -> Option<&PropDef> {
        self.props.get(&key)
    }

    pub fn function(&self, id: FuncId) -> &FunctionDef {
        &self.functions[id.0 as usize]
    }

    pub fn state(&self, id: StateId) -> &StateDef {
        &self.states[id.0 as usize]
    }

    pub fn class(&self, id: ClassId) -> &ClassDef {
        &self.classes[id.0 as usize]
    }

    /// The class id of an already linked class key.
    pub fn class_id(&self, key: ObjectKey) -> Option<ClassId> {
        self.class_ids.get(&key).copied()
    }

    pub fn function_id(&self, key: ObjectKey) -> Option<FuncId> {
        self.function_ids.get(&key).copied()
    }

    pub fn intrinsic_class(&self, name: NameId) -> Option<ClassId> {
        self.intrinsic_ids.get(&name).copied()
    }

    /// Whether `class` is `ancestor` or derives from it.
    pub fn is_child_of(&self, mut class: ClassId, ancestor: ClassId) -> bool {
        loop {
            if class == ancestor {
                return true;
            }
            match self.class(class).super_ {
                Some(s) => class = s,
                None => return false,
            }
        }
    }

    /// Reference to a class, which may be intrinsic (no serialized definition anywhere).
    pub fn class_ref(&mut self, pkg: &Arc<Package>, r: ObjectRef) -> Result<Option<ClassRef>> {
        if r.is_null() {
            return Ok(None);
        }
        match self.lib.resolve(pkg, r) {
            Ok(h) => Ok(Some(ClassRef::Key(ObjectKey { package: self.pkg_id(&h.package), export: h.export }))),
            Err(_) if matches!(r, ObjectRef::Import(i) if pkg.name(pkg.import(i).class_name) == "Class") => {
                let name = pkg.object_name(r).to_string();
                self.intrinsic(&name);
                Ok(Some(ClassRef::Intrinsic(self.names.intern(&name))))
            }
            Err(e) => err(e),
        }
    }

    fn load(&self, key: ObjectKey) -> Result<Asset> {
        let pkg = self.package(key.package);
        load_export(pkg, key.export)
            .map(|o| o.asset)
            .map_err(|e| LinkError(format!("{}: {e}", self.path(key))))
    }

    // ------------------------------------------------------------------ classes

    /// Class of a serialized object. Classes without a serialized definition anywhere
    /// (`Core.Package`, `Engine.Model`, ...) become intrinsic classes without properties.
    pub fn class_of(&mut self, pkg: &Arc<Package>, export: u32) -> Result<ClassId> {
        let r = pkg.export(export).class;
        if r.is_null() {
            return Ok(self.intrinsic("Class"));
        }
        match self.lib.resolve(pkg, r) {
            Ok(h) => {
                let key = ObjectKey { package: self.pkg_id(&h.package), export: h.export };
                self.link_class(key)
            }
            Err(_) if matches!(r, ObjectRef::Import(_)) => {
                let name = pkg.object_name(r).to_string();
                Ok(self.intrinsic(&name))
            }
            Err(e) => err(e),
        }
    }

    /// Declares that a class the engine implements natively extends a class from the scripts,
    /// so instances of it carry that class's properties. Unreal's `Viewport` works this way:
    /// the packages only import it, but the scripts use it as a `Player`.
    pub fn native_class(&mut self, name: &str, super_: ClassId) -> ClassId {
        let id = self.intrinsic(name);
        if self.classes[id.0 as usize].super_.is_some() {
            return id;
        }
        let parent = self.classes[super_.0 as usize].clone();
        let class = &mut self.classes[id.0 as usize];
        class.super_ = Some(super_);
        class.props = parent.props;
        class.by_name = parent.by_name;
        class.slots = parent.slots;
        class.defaults = parent.defaults;
        class.functions = parent.functions;
        class.states = parent.states;
        id
    }

    fn intrinsic(&mut self, name: &str) -> ClassId {
        let n = self.names.intern(name);
        if let Some(&id) = self.intrinsic_ids.get(&n) {
            return id;
        }
        let id = ClassId(self.classes.len() as u32);
        self.classes.push(ClassDef {
            name: n,
            key: Some(ObjectKey { package: INTRINSIC, export: id.0 }),
            super_: None,
            flags: 0,
            props: Vec::new(),
            by_name: crate::FastMap::default(),
            slots: 0,
            defaults: Vec::new(),
            functions: crate::FastMap::default(),
            states: HashMap::new(),
            config_name: n,
            probe_mask: u64::MAX,
            ignore_mask: u64::MAX,
        });
        self.intrinsic_ids.insert(n, id);
        id
    }

    /// Names of the intrinsic classes created so far.
    pub fn intrinsic_classes(&self) -> Vec<&str> {
        let mut v: Vec<&str> = self.intrinsic_ids.keys().map(|&n| self.names.str(n)).collect();
        v.sort();
        v
    }

    pub fn link_class(&mut self, key: ObjectKey) -> Result<ClassId> {
        if key.package == INTRINSIC {
            return Ok(ClassId(key.export));
        }
        if let Some(&id) = self.class_ids.get(&key) {
            return Ok(id);
        }
        if !self.linking.insert(key) {
            return err(format!("{}: class inherits from itself", self.path(key)));
        }
        let r = self.link_class_inner(key);
        self.linking.remove(&key);
        let class = r.map_err(|e| LinkError(format!("class {}: {e}", self.path(key))))?;
        let id = ClassId(self.classes.len() as u32);
        self.classes.push(class);
        self.class_ids.insert(key, id);
        Ok(id)
    }

    fn link_class_inner(&mut self, key: ObjectKey) -> Result<ClassDef> {
        let pkg = self.package(key.package).clone();
        let Asset::Class(c) = self.load(key)? else {
            return err("not a Class");
        };
        let name = self.names.intern(pkg.object_name(ObjectRef::Export(key.export)));
        let super_ = match self.resolve(&pkg, c.state.struct_.field.super_field)? {
            Some(k) => Some(self.link_class(k)?),
            None => None,
        };
        let (mut props, mut slots, mut defaults) = match super_ {
            Some(s) => {
                let s = &self.classes[s.0 as usize];
                (s.props.clone(), s.slots, s.defaults.clone())
            }
            None => (Vec::new(), 0, Vec::new()),
        };
        let mut functions = crate::FastMap::default();
        let mut states = HashMap::new();
        for (child, asset) in self.children(&pkg, c.state.struct_.children)? {
            let child_name = self.names.intern(pkg.object_name(ObjectRef::Export(child.export)));
            match asset {
                Asset::Property(_) => {
                    let p = self.prop_def(child, slots)?;
                    slots += p.array_dim;
                    for _ in 0..p.array_dim {
                        defaults.push(self.zero(&p.ty));
                    }
                    props.push(p);
                }
                Asset::Function(_) => {
                    functions.insert(child_name, self.link_function(child)?);
                }
                Asset::State(_) => {
                    let id = self.link_state(child)?;
                    // A state extends the state of the same name above it, and the packages
                    // do not write that link down: `cHarryAnimChannel.stateCast` only adds a
                    // `BeginState`, and the `Tick` that casts the spell is the one
                    // `cAnimChannel.stateCast` has. Without this the animation plays and
                    // nothing comes of it.
                    if self.states[id.0 as usize].super_.is_none() {
                        let mut above = super_;
                        while let Some(c) = above {
                            if let Some(&parent) = self.classes[c.0 as usize].states.get(&child_name) {
                                self.states[id.0 as usize].super_ = Some(parent);
                                break;
                            }
                            above = self.classes[c.0 as usize].super_;
                        }
                    }
                    states.insert(child_name, id);
                }
                _ => {}
            }
        }
        let by_name = props.iter().enumerate().map(|(i, p)| (p.name, i)).collect();
        let mut class = ClassDef {
            name,
            key: Some(key),
            super_,
            flags: c.flags,
            props,
            by_name,
            slots,
            defaults: Vec::new(),
            functions,
            states,
            config_name: self.name(&pkg, c.config_name),
            probe_mask: c.state.probe_mask,
            ignore_mask: c.state.ignore_mask,
        };
        for tag in &c.defaults {
            self.apply_tag(&pkg, &class, &mut defaults, tag)?;
        }
        class.defaults = defaults;
        Ok(class)
    }

    /// Walks a `Children` / `Next` chain within one package.
    fn children(&mut self, pkg: &Arc<Package>, first: ObjectRef) -> Result<Vec<(ObjectKey, Asset)>> {
        let mut out = Vec::new();
        let mut cur = first;
        while let ObjectRef::Export(e) = cur {
            let key = ObjectKey { package: self.pkg_id(pkg), export: e };
            let asset = self.load(key)?;
            let next = field_of(&asset).map(|f| f.next).ok_or_else(|| {
                LinkError(format!("{}: child is not a field", self.path(key)))
            })?;
            out.push((key, asset));
            cur = next;
            if out.len() > 100_000 {
                return err("children chain does not terminate");
            }
        }
        if !cur.is_null() {
            return err(format!("children chain points outside its package: {cur:?}"));
        }
        Ok(out)
    }

    fn prop_def(&mut self, key: ObjectKey, slot: u32) -> Result<PropDef> {
        let pkg = self.package(key.package).clone();
        let Asset::Property(p) = self.load(key)? else {
            return err(format!("{}: not a property", self.path(key)));
        };
        let ty = match p.kind {
            PropertyKind::Byte { enum_ } => match self.resolve(&pkg, enum_)? {
                Some(k) => PropType::Byte(Some(self.link_enum(k)?)),
                None => PropType::Byte(None),
            },
            PropertyKind::Int => PropType::Int,
            PropertyKind::Bool => PropType::Bool,
            PropertyKind::Float => PropType::Float,
            PropertyKind::Object { class } => PropType::Object(self.class_ref(&pkg, class)?),
            PropertyKind::Class { meta_class, .. } => PropType::Class(self.class_ref(&pkg, meta_class)?),
            PropertyKind::Name => PropType::Name,
            PropertyKind::Str => PropType::Str,
            PropertyKind::Struct { struct_ } => match self.resolve(&pkg, struct_)? {
                Some(k) => PropType::Struct(self.link_struct(k)?),
                None => return err(format!("{}: StructProperty without struct", self.path(key))),
            },
            PropertyKind::Array { inner } => match self.resolve(&pkg, inner)? {
                Some(k) => PropType::Array(Box::new(self.prop_def(k, 0)?)),
                None => return err(format!("{}: ArrayProperty without inner", self.path(key))),
            },
        };
        if p.array_dim < 1 {
            return err(format!("{}: ArrayDim {}", self.path(key), p.array_dim));
        }
        let name = self.names.intern(pkg.object_name(ObjectRef::Export(key.export)));
        let def = PropDef { name, array_dim: p.array_dim as u32, flags: p.flags, ty, slot, key };
        self.props.insert(key, def.clone());
        Ok(def)
    }

    pub fn link_function(&mut self, key: ObjectKey) -> Result<FuncId> {
        if let Some(&id) = self.function_ids.get(&key) {
            return Ok(id);
        }
        let pkg = self.package(key.package).clone();
        let Asset::Function(f) = self.load(key)? else {
            return err(format!("{}: not a Function", self.path(key)));
        };
        let mut locals = Vec::new();
        let mut slots = 0;
        for (child, asset) in self.children(&pkg, f.struct_.children)? {
            if let Asset::Property(_) = asset {
                let p = self.prop_def(child, slots)?;
                slots += p.array_dim;
                locals.push(p);
            }
        }
        let params = (0..locals.len())
            .filter(|&i| locals[i].flags & cpf::PARM != 0 && locals[i].flags & cpf::RETURN_PARM == 0)
            .collect();
        let ret = locals.iter().position(|p| p.flags & cpf::RETURN_PARM != 0);
        let super_ = self.resolve(&pkg, f.struct_.field.super_field)?;
        let def = FunctionDef {
            name: self.names.intern(pkg.object_name(ObjectRef::Export(key.export))),
            key,
            flags: f.flags,
            native: f.native_index,
            operator_precedence: f.operator_precedence,
            locals,
            slots,
            params,
            ret,
            code: Arc::new(Code::new(f.struct_.script.clone(), key.package)),
            super_,
        };
        let id = FuncId(self.functions.len() as u32);
        self.functions.push(def);
        self.function_ids.insert(key, id);
        Ok(id)
    }

    pub fn link_state(&mut self, key: ObjectKey) -> Result<StateId> {
        if let Some(&id) = self.state_ids.get(&key) {
            return Ok(id);
        }
        let pkg = self.package(key.package).clone();
        let Asset::State(st) = self.load(key)? else {
            return err(format!("{}: not a State", self.path(key)));
        };
        let super_ = match self.resolve(&pkg, st.struct_.field.super_field)? {
            Some(k) => Some(self.link_state(k)?),
            None => None,
        };
        let mut functions = crate::FastMap::default();
        for (child, asset) in self.children(&pkg, st.struct_.children)? {
            if let Asset::Function(_) = asset {
                let n = self.names.intern(pkg.object_name(ObjectRef::Export(child.export)));
                functions.insert(n, self.link_function(child)?);
            }
        }
        let mut labels = HashMap::new();
        for s in &st.struct_.script {
            if let grim_assets::bytecode::Expr::LabelTable(t) = &s.expr {
                for &(n, off) in t {
                    labels.insert(self.name(&pkg, n), off);
                }
            }
        }
        let def = StateDef {
            name: self.names.intern(pkg.object_name(ObjectRef::Export(key.export))),
            key,
            super_,
            flags: st.flags,
            probe_mask: st.probe_mask,
            ignore_mask: st.ignore_mask,
            functions,
            code: Arc::new(Code::new(st.struct_.script.clone(), key.package)),
            labels,
        };
        let id = StateId(self.states.len() as u32);
        self.states.push(def);
        self.state_ids.insert(key, id);
        Ok(id)
    }

    pub fn link_struct(&mut self, key: ObjectKey) -> Result<StructId> {
        if let Some(&id) = self.struct_ids.get(&key) {
            return Ok(id);
        }
        let pkg = self.package(key.package).clone();
        let Asset::Struct(s) = self.load(key)? else {
            return err(format!("{}: not a Struct", self.path(key)));
        };
        let super_ = match self.resolve(&pkg, s.field.super_field)? {
            Some(k) => Some(self.link_struct(k)?),
            None => None,
        };
        let (mut fields, mut slots) = match super_ {
            Some(s) => (self.structs[s.0 as usize].fields.clone(), self.structs[s.0 as usize].slots),
            None => (Vec::new(), 0),
        };
        for (child, asset) in self.children(&pkg, s.children)? {
            if let Asset::Property(_) = asset {
                let p = self.prop_def(child, slots)?;
                slots += p.array_dim;
                fields.push(p);
            }
        }
        let name = self.names.intern(pkg.object_name(ObjectRef::Export(key.export)));
        let id = StructId(self.structs.len() as u32);
        self.structs.push(StructDef { name, key, super_, fields, slots });
        self.struct_ids.insert(key, id);
        Ok(id)
    }

    pub fn link_enum(&mut self, key: ObjectKey) -> Result<EnumId> {
        if let Some(&id) = self.enum_ids.get(&key) {
            return Ok(id);
        }
        let pkg = self.package(key.package).clone();
        let Asset::Enum(e) = self.load(key)? else {
            return err(format!("{}: not an Enum", self.path(key)));
        };
        let values = e.names.iter().map(|&n| self.name(&pkg, n)).collect();
        let name = self.names.intern(pkg.object_name(ObjectRef::Export(key.export)));
        let id = EnumId(self.enums.len() as u32);
        self.enums.push(EnumDef { name, key, values });
        self.enum_ids.insert(key, id);
        Ok(id)
    }

    // ------------------------------------------------------------------ values

    pub fn zero(&mut self, ty: &PropType) -> Value {
        match ty {
            PropType::Byte(_) => Value::Byte(0),
            PropType::Int => Value::Int(0),
            PropType::Bool => Value::Bool(false),
            PropType::Float => Value::Float(0.0),
            PropType::Object(_) | PropType::Class(_) => Value::Object(None),
            PropType::Name => Value::Name(self.names.intern("None")),
            PropType::Str => Value::Str(String::new()),
            PropType::Struct(s) => {
                let fields = self.structs[s.0 as usize].fields.clone();
                let mut v = Vec::new();
                for f in &fields {
                    for _ in 0..f.array_dim {
                        v.push(self.zero(&f.ty));
                    }
                }
                Value::Struct(v)
            }
            PropType::Array(_) => Value::Array(Vec::new()),
        }
    }

    /// Applies one tagged property to `values`, checking it against the declared property.
    fn apply_tag(&mut self, pkg: &Arc<Package>, class: &ClassDef, values: &mut [Value], tag: &Tag) -> Result<()> {
        let name = self.name(pkg, tag.name);
        let ctx = |w: &World| format!("property {} (tag @{:#x})", w.names.str(name), tag.offset);
        // The one whose type the tag fits: a name can stand for more than one property down a
        // class chain, and what was written says which of them it is.
        let fits = |p: &&crate::types::PropDef| match (&tag.value, &p.ty) {
            (PropertyValue::Bool(_), PropType::Bool)
            | (PropertyValue::Byte(_), PropType::Byte(_))
            | (PropertyValue::Int(_), PropType::Int)
            | (PropertyValue::Float(_), PropType::Float)
            | (PropertyValue::Object(_), PropType::Object(_) | PropType::Class(_))
            | (PropertyValue::Class(_), PropType::Class(_))
            | (PropertyValue::Name(_), PropType::Name)
            | (PropertyValue::Str(_), PropType::Str)
            | (PropertyValue::Array(_), PropType::Array(_)) => true,
            (PropertyValue::Struct { name: sn, .. }, PropType::Struct(sid)) => {
                self.name(pkg, *sn) == self.structs[sid.0 as usize].name
            }
            _ => false,
        };
        let Some(prop) = class.props_named(name).find(fits).or_else(|| class.prop(name)).cloned() else {
            return err(format!("{}: not declared in class {}", ctx(self), self.names.str(class.name)));
        };
        if tag.array_index >= prop.array_dim {
            return err(format!("{}: index {} >= ArrayDim {}", ctx(self), tag.array_index, prop.array_dim));
        }
        let data = pkg.shared_bytes();
        let mut r = Reader::with_base(&data[tag.payload.clone()], tag.payload.start);
        let v = match (&tag.value, &prop.ty) {
            (PropertyValue::Bool(b), PropType::Bool) => Value::Bool(*b),
            (PropertyValue::Byte(b), PropType::Byte(_)) => Value::Byte(*b),
            (PropertyValue::Int(i), PropType::Int) => Value::Int(*i),
            (PropertyValue::Float(f), PropType::Float) => Value::Float(*f),
            (PropertyValue::Object(o), PropType::Object(_) | PropType::Class(_))
            | (PropertyValue::Class(o), PropType::Class(_)) => {
                // What the game cannot load leaves the reference empty, which is what Unreal
                // does with an import it fails to find. A saved game from another language
                // names dialog this install does not have (`AllDialog.fre.PC_Gv3_Vendor_05`
                // against a Spanish `AllDialog`), and the level still has to come up.
                match self.resolve(pkg, *o) {
                    Ok(k) => Value::Object(k),
                    Err(e) => {
                        self.unresolved.push(e.0);
                        Value::Object(None)
                    }
                }
            }
            (PropertyValue::Name(n), PropType::Name) => Value::Name(self.name(pkg, *n)),
            (PropertyValue::Str(s), PropType::Str) => Value::Str(s.clone()),
            (PropertyValue::Struct { name: sn, .. }, PropType::Struct(sid)) => {
                let declared = self.structs[sid.0 as usize].name;
                if self.name(pkg, *sn) != declared {
                    return err(format!("{}: struct {} but declared {}", ctx(self), pkg.name(*sn), self.names.str(declared)));
                }
                self.read_item(pkg, &mut r, &prop.ty).map_err(|e| LinkError(format!("{}: {e}", ctx(self))))?
            }
            (PropertyValue::Array(_), PropType::Array(_)) => {
                self.read_item(pkg, &mut r, &prop.ty).map_err(|e| LinkError(format!("{}: {e}", ctx(self))))?
            }
            (v, t) => return err(format!("{}: tag {v:?} does not match declared {t:?}", ctx(self))),
        };
        if matches!(prop.ty, PropType::Struct(_) | PropType::Array(_)) && !r.is_at_end() {
            return err(format!("{}: {} payload bytes left over", ctx(self), r.remaining()));
        }
        values[(prop.slot + tag.array_index) as usize] = v;
        Ok(())
    }

    /// Untagged binary serialization of one value, as used inside structs and arrays.
    fn read_item(&mut self, pkg: &Arc<Package>, r: &mut Reader, ty: &PropType) -> Result<Value> {
        let e = |x: grim_package::Error| LinkError(x.to_string());
        Ok(match ty {
            PropType::Byte(_) => Value::Byte(r.u8().map_err(e)?),
            PropType::Int => Value::Int(r.i32().map_err(e)?),
            PropType::Bool => {
                let at = r.offset();
                match r.u8().map_err(e)? {
                    0 => Value::Bool(false),
                    1 => Value::Bool(true),
                    v => return err(format!("@{at:#x}: bool item {v}")),
                }
            }
            PropType::Float => Value::Float(r.f32().map_err(e)?),
            PropType::Object(_) | PropType::Class(_) => {
                let o = read_object_ref(pkg, r).map_err(e)?;
                Value::Object(self.resolve(pkg, o)?)
            }
            PropType::Name => {
                let n = read_name(pkg, r).map_err(e)?;
                Value::Name(self.name(pkg, n))
            }
            PropType::Str => Value::Str(read_string(r).map_err(e)?),
            PropType::Struct(s) => {
                let fields = self.structs[s.0 as usize].fields.clone();
                let mut v = Vec::new();
                for f in &fields {
                    for _ in 0..f.array_dim {
                        v.push(self.read_item(pkg, r, &f.ty)?);
                    }
                }
                Value::Struct(v)
            }
            PropType::Array(inner) => {
                let n = r.count(1).map_err(e)?;
                let mut v = Vec::with_capacity(n);
                for _ in 0..n {
                    v.push(self.read_item(pkg, r, &inner.ty)?);
                }
                Value::Array(v)
            }
        })
    }

    // ------------------------------------------------------------------ objects

    /// Builds a live object from a serialized export: class defaults + its tagged properties.
    pub fn instantiate(&mut self, pkg: &Arc<Package>, export: u32) -> Result<Object> {
        let key = ObjectKey { package: self.pkg_id(pkg), export };
        let class = self.class_of(pkg, export)?;
        let o = load_export(pkg, export).map_err(|e| LinkError(format!("{}: {e}", self.path(key))))?;
        let def = self.classes[class.0 as usize].clone();
        let mut values = def.defaults.clone();
        for tag in o.base.properties.iter().flatten() {
            self.apply_tag(pkg, &def, &mut values, tag).map_err(|e| LinkError(format!("{}: {e}", self.path(key))))?;
        }
        Ok(Object { key, class, values })
    }
}
