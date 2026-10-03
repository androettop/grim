//! Checks the conversion token table (grim-assets `Cast`) against the types inferred from the
//! game's own bytecode.

use std::collections::{BTreeMap, HashMap};

use grim_assets::bytecode::{Cast, Expr};
use grim_object::{ObjectKey, World};
use grim_package::{Library, ObjectRef};
use grim_vm::typing::{Scope, Ty, Typer};

type Stats = BTreeMap<String, (usize, usize, BTreeMap<String, usize>)>;

fn expected_types(c: Cast, vector: &Ty, rotator: &Ty) -> (Vec<Ty>, Ty) {
    use Cast::*;
    let (from, to) = match c {
        RotatorToVector => (rotator.clone(), vector.clone()),
        ByteToInt => (Ty::Byte, Ty::Int),
        ByteToFloat => (Ty::Byte, Ty::Float),
        IntToByte => (Ty::Int, Ty::Byte),
        IntToFloat => (Ty::Int, Ty::Float),
        BoolToInt => (Ty::Bool, Ty::Int),
        FloatToByte => (Ty::Float, Ty::Byte),
        FloatToInt => (Ty::Float, Ty::Int),
        StringToInt => (Ty::Str, Ty::Int),
        StringToBool => (Ty::Str, Ty::Bool),
        StringToFloat => (Ty::Str, Ty::Float),
        VectorToRotator => (vector.clone(), rotator.clone()),
        ByteToString => (Ty::Byte, Ty::Str),
        IntToString => (Ty::Int, Ty::Str),
        BoolToString => (Ty::Bool, Ty::Str),
        FloatToString => (Ty::Float, Ty::Str),
        ObjectToString => return (vec![Ty::Object, Ty::Class], Ty::Str),
        NameToString => (Ty::Name, Ty::Str),
        VectorToString => (vector.clone(), Ty::Str),
        RotatorToString => (rotator.clone(), Ty::Str),
        StringToName => (Ty::Str, Ty::Name),
    };
    (vec![from], to)
}

fn walk(t: &mut Typer, s: &Scope, e: &Expr, expected: Option<Ty>, out: &mut Stats) {
    let mut sub = |t: &mut Typer, e: &Expr, exp: Option<Ty>, out: &mut Stats| walk(t, s, e, exp, out);
    match e {
        Expr::Cast { cast, expr } => {
            let from = t.ty(s, expr);
            let v = Ty::Struct(t.world.names.intern("Vector"));
            let r = Ty::Struct(t.world.names.intern("Rotator"));
            let (want_from, want_to) = expected_types(*cast, &v, &r);
            let entry = out.entry(format!("{cast:?}")).or_default();
            if let (Some(f), Some(to)) = (&from, &expected) {
                if want_from.contains(f) && *to == want_to {
                    entry.0 += 1;
                } else {
                    entry.1 += 1;
                    *entry.2.entry(format!("{f:?} -> {to:?}")).or_default() += 1;
                }
            }
            sub(t, expr, None, out);
        }
        Expr::Let(a, b) | Expr::LetBool(a, b) => {
            let ta = t.ty(s, a);
            sub(t, a, None, out);
            sub(t, b, ta, out);
        }
        Expr::JumpIfNot { cond, .. } => sub(t, cond, Some(Ty::Bool), out),
        Expr::Return(x) => {
            let r = s.function.and_then(|f| t.ret_ty(f));
            sub(t, x, r, out);
        }
        Expr::FinalFunction { args, .. } | Expr::VirtualFunction { args, .. } | Expr::GlobalFunction { args, .. } | Expr::Native { args, .. } => {
            let f = t.callee(s, e);
            for (i, a) in args.iter().enumerate() {
                let exp = f.and_then(|f| {
                    let fd = t.world.function(f);
                    let p = *fd.params.get(i)?;
                    Some(grim_vm::typing::ty_of_prop(t.world, &fd.locals[p].ty.clone()))
                });
                sub(t, a, exp, out);
            }
        }
        Expr::Context { object, expr, .. } | Expr::ClassContext { object, expr, .. } => {
            sub(t, object, None, out);
            sub(t, expr, expected, out);
        }
        other => {
            for c in children(other) {
                sub(t, c, None, out);
            }
        }
    }
}

fn children(e: &Expr) -> Vec<&Expr> {
    use Expr::*;
    match e {
        Return(a) | GotoLabel(a) | EatString(a) | BoolVariable(a) | DynArrayLength(a) => vec![a],
        Switch { value, .. } => vec![value],
        JumpIfNot { cond, .. } | Assert { cond, .. } => vec![cond],
        Case { value: Some(v), .. } => vec![v],
        DynArrayElement { index, array } | ArrayElement { index, array } => vec![index, array],
        New { outer, name, flags, class } => vec![outer, name, flags, class],
        MetaCast { expr, .. } | DynamicCast { expr, .. } | Skip { expr, .. } | StructMember { expr, .. } | Iterator { expr, .. } => vec![expr],
        StructCmpEq { a, b, .. } | StructCmpNe { a, b, .. } => vec![a, b],
        _ => vec![],
    }
}

#[test]
fn conversion_tokens() {
    let root = grim_testkit::require_game_dir();
    let mut world = World::new(Library::open(&root));
    let mut classes = Vec::new();
    for f in grim_testkit::package_files(&root) {
        let pkg = world.lib.load_path(&f).unwrap();
        for i in 0..pkg.exports.len() as u32 {
            if pkg.class_name(ObjectRef::Export(i)) == "Class" {
                let key = ObjectKey { package: world.pkg_id(&pkg), export: i };
                classes.push(world.link_class(key).unwrap());
            }
        }
    }
    let natives: grim_object::FastMap<u16, _> = (0..world.functions.len())
        .filter(|&i| world.functions[i].native != 0)
        .map(|i| (world.functions[i].native, grim_object::FuncId(i as u32)))
        .collect();
    // (code, scope) for every function and state of every class.
    let mut jobs = Vec::new();
    for &c in &classes {
        let cd = world.class(c).clone();
        for &f in cd.functions.values() {
            let fd = world.function(f);
            jobs.push((fd.code.clone(), Scope { package: fd.key.package, class: c, function: Some(f) }));
        }
        for &st in cd.states.values() {
            let sd = world.state(st).clone();
            jobs.push((sd.code.clone(), Scope { package: sd.key.package, class: c, function: None }));
            for &f in sd.functions.values() {
                let fd = world.function(f);
                jobs.push((fd.code.clone(), Scope { package: fd.key.package, class: c, function: Some(f) }));
            }
        }
    }
    let mut out = Stats::new();
    let mut t = Typer { world: &mut world, natives: &natives };
    for (code, scope) in &jobs {
        for st in &code.statements {
            walk(&mut t, scope, &st.expr, None, &mut out);
        }
    }
    let mut bad = Vec::new();
    for (cast, (ok, wrong, examples)) in &out {
        eprintln!("{cast:16} consistent {ok:5}  inconsistent {wrong:3} {examples:?}");
        if *wrong * 50 > ok + wrong || *ok == 0 && *wrong > 0 {
            bad.push(cast.clone());
        }
    }
    assert!(out.len() >= 20, "expected most casts to occur, saw {}", out.len());
    assert!(bad.is_empty(), "casts inconsistent with inferred types: {bad:?}");
}
