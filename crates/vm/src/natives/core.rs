//! `Core.Object` natives: operators, strings, math, vectors, rotators, states and logging.

use grim_object::Value;

use super::{NativeCall, Natives};
use crate::vm::{rotator, vector, Vm, VmResult};

type R = VmResult<Value>;

fn b(v: bool) -> R {
    Ok(Value::Bool(v))
}
fn i(v: i32) -> R {
    Ok(Value::Int(v))
}
fn f(v: f32) -> R {
    Ok(Value::Float(v))
}
fn s(v: String) -> R {
    Ok(Value::Str(v))
}
fn v3(v: [f32; 3]) -> R {
    Ok(vector(v[0], v[1], v[2]))
}
fn r3(v: [i32; 3]) -> R {
    Ok(rotator(v[0], v[1], v[2]))
}

/// Unit axes of a rotator: forward, right, up (Unreal units: 65536 = full turn).
pub fn axes(r: [i32; 3]) -> [[f32; 3]; 3] {
    let a = |u: i32| u as f32 * std::f32::consts::PI / 32768.0;
    let (sp, cp) = a(r[0]).sin_cos();
    let (sy, cy) = a(r[1]).sin_cos();
    let (sr, cr) = a(r[2]).sin_cos();
    [
        [cp * cy, cp * sy, sp],
        [sr * sp * cy - cr * sy, sr * sp * sy + cr * cy, -sr * cp],
        [-(cr * sp * cy + sr * sy), cy * sr - cr * sp * sy, cr * cp],
    ]
}

fn dot(a: [f32; 3], b: [f32; 3]) -> f32 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

fn add(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [a[0] + b[0], a[1] + b[1], a[2] + b[2]]
}

fn sub(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}

fn scale(a: [f32; 3], k: f32) -> [f32; 3] {
    [a[0] * k, a[1] * k, a[2] * k]
}

fn size(a: [f32; 3]) -> f32 {
    dot(a, a).sqrt()
}

fn normal(a: [f32; 3]) -> [f32; 3] {
    let l = size(a);
    if l > 1e-8 { scale(a, 1.0 / l) } else { [0.0; 3] }
}

fn norm_angle(a: i32) -> i32 {
    let a = a & 0xffff;
    if a > 32767 { a - 65536 } else { a }
}

fn out(c: &mut NativeCall, idx: usize, v: Value) -> R {
    c.args[idx] = v.clone();
    Ok(v)
}

pub fn register(n: &mut Natives) {
    macro_rules! nat {
        ($name:literal, |$vm:ident, $c:ident| $body:expr) => {
            n.add(concat!("Core.Object.", $name), |$vm: &mut Vm, $c: &mut NativeCall| -> R {
                let _ = (&$vm, &$c);
                $body
            });
        };
    }

    // ---- bool
    nat!("Not_PreBool", |vm, c| b(!c.bool(0)?));
    nat!("AndAnd_BoolBool", |vm, c| b(c.bool(0)? && c.bool(1)?));
    nat!("OrOr_BoolBool", |vm, c| b(c.bool(0)? || c.bool(1)?));
    nat!("XorXor_BoolBool", |vm, c| b(c.bool(0)? != c.bool(1)?));
    nat!("EqualEqual_BoolBool", |vm, c| b(c.bool(0)? == c.bool(1)?));
    nat!("NotEqual_BoolBool", |vm, c| b(c.bool(0)? != c.bool(1)?));

    // ---- byte
    nat!("MultiplyEqual_ByteByte", |vm, c| { let v = c.byte(0)?.wrapping_mul(c.byte(1)?); out(c, 0, Value::Byte(v)) });
    nat!("DivideEqual_ByteByte", |vm, c| {
        let d = c.byte(1)?;
        let v = if d == 0 { vm.warn_here("Divide by zero"); 0 } else { c.byte(0)? / d };
        out(c, 0, Value::Byte(v))
    });
    nat!("AddEqual_ByteByte", |vm, c| { let v = c.byte(0)?.wrapping_add(c.byte(1)?); out(c, 0, Value::Byte(v)) });
    nat!("SubtractEqual_ByteByte", |vm, c| { let v = c.byte(0)?.wrapping_sub(c.byte(1)?); out(c, 0, Value::Byte(v)) });
    nat!("AddAdd_PreByte", |vm, c| { let v = c.byte(0)?.wrapping_add(1); out(c, 0, Value::Byte(v)) });
    nat!("SubtractSubtract_PreByte", |vm, c| { let v = c.byte(0)?.wrapping_sub(1); out(c, 0, Value::Byte(v)) });
    nat!("AddAdd_Byte", |vm, c| { let old = c.byte(0)?; out(c, 0, Value::Byte(old.wrapping_add(1)))?; Ok(Value::Byte(old)) });
    nat!("SubtractSubtract_Byte", |vm, c| { let old = c.byte(0)?; out(c, 0, Value::Byte(old.wrapping_sub(1)))?; Ok(Value::Byte(old)) });

    // ---- int
    nat!("Complement_PreInt", |vm, c| i(!c.int(0)?));
    nat!("Subtract_PreInt", |vm, c| i(c.int(0)?.wrapping_neg()));
    nat!("Multiply_IntInt", |vm, c| i(c.int(0)?.wrapping_mul(c.int(1)?)));
    nat!("Divide_IntInt", |vm, c| {
        let d = c.int(1)?;
        if d == 0 { vm.warn_here("Divide by zero"); i(0) } else { i(c.int(0)?.wrapping_div(d)) }
    });
    nat!("Add_IntInt", |vm, c| i(c.int(0)?.wrapping_add(c.int(1)?)));
    nat!("Subtract_IntInt", |vm, c| i(c.int(0)?.wrapping_sub(c.int(1)?)));
    nat!("LessLess_IntInt", |vm, c| i(c.int(0)?.wrapping_shl(c.int(1)? as u32)));
    nat!("GreaterGreater_IntInt", |vm, c| i(c.int(0)?.wrapping_shr(c.int(1)? as u32)));
    nat!("GreaterGreaterGreater_IntInt", |vm, c| i((c.int(0)? as u32).wrapping_shr(c.int(1)? as u32) as i32));
    nat!("Less_IntInt", |vm, c| b(c.int(0)? < c.int(1)?));
    nat!("Greater_IntInt", |vm, c| b(c.int(0)? > c.int(1)?));
    nat!("LessEqual_IntInt", |vm, c| b(c.int(0)? <= c.int(1)?));
    nat!("GreaterEqual_IntInt", |vm, c| b(c.int(0)? >= c.int(1)?));
    nat!("EqualEqual_IntInt", |vm, c| b(c.int(0)? == c.int(1)?));
    nat!("NotEqual_IntInt", |vm, c| b(c.int(0)? != c.int(1)?));
    nat!("And_IntInt", |vm, c| i(c.int(0)? & c.int(1)?));
    nat!("Xor_IntInt", |vm, c| i(c.int(0)? ^ c.int(1)?));
    nat!("Or_IntInt", |vm, c| i(c.int(0)? | c.int(1)?));
    nat!("MultiplyEqual_IntFloat", |vm, c| { let v = (c.int(0)? as f32 * c.float(1)?) as i32; out(c, 0, Value::Int(v)) });
    nat!("DivideEqual_IntFloat", |vm, c| {
        let d = c.float(1)?;
        let v = if d == 0.0 { vm.warn_here("Divide by zero"); 0 } else { (c.int(0)? as f32 / d) as i32 };
        out(c, 0, Value::Int(v))
    });
    nat!("AddEqual_IntInt", |vm, c| { let v = c.int(0)?.wrapping_add(c.int(1)?); out(c, 0, Value::Int(v)) });
    nat!("SubtractEqual_IntInt", |vm, c| { let v = c.int(0)?.wrapping_sub(c.int(1)?); out(c, 0, Value::Int(v)) });
    nat!("AddAdd_PreInt", |vm, c| { let v = c.int(0)?.wrapping_add(1); out(c, 0, Value::Int(v)) });
    nat!("SubtractSubtract_PreInt", |vm, c| { let v = c.int(0)?.wrapping_sub(1); out(c, 0, Value::Int(v)) });
    nat!("AddAdd_Int", |vm, c| { let old = c.int(0)?; out(c, 0, Value::Int(old.wrapping_add(1)))?; i(old) });
    nat!("SubtractSubtract_Int", |vm, c| { let old = c.int(0)?; out(c, 0, Value::Int(old.wrapping_sub(1)))?; i(old) });
    nat!("Rand", |vm, c| { let m = c.int(0)?; i(if m > 0 { (vm.frand() * m as f32) as i32 % m } else { 0 }) });
    nat!("Min", |vm, c| i(c.int(0)?.min(c.int(1)?)));
    nat!("Max", |vm, c| i(c.int(0)?.max(c.int(1)?)));
    nat!("Clamp", |vm, c| { let (v, a, bb) = (c.int(0)?, c.int(1)?, c.int(2)?); i(v.max(a).min(bb)) });

    // ---- float
    nat!("Subtract_PreFloat", |vm, c| f(-c.float(0)?));
    nat!("MultiplyMultiply_FloatFloat", |vm, c| f(c.float(0)?.powf(c.float(1)?)));
    nat!("Multiply_FloatFloat", |vm, c| f(c.float(0)? * c.float(1)?));
    nat!("Divide_FloatFloat", |vm, c| {
        let d = c.float(1)?;
        if d == 0.0 { vm.warn_here("Divide by zero"); f(0.0) } else { f(c.float(0)? / d) }
    });
    nat!("Percent_FloatFloat", |vm, c| {
        let d = c.float(1)?;
        if d == 0.0 { vm.warn("Modulo by zero"); f(0.0) } else { f(c.float(0)? % d) }
    });
    nat!("Add_FloatFloat", |vm, c| f(c.float(0)? + c.float(1)?));
    nat!("Subtract_FloatFloat", |vm, c| f(c.float(0)? - c.float(1)?));
    nat!("Less_FloatFloat", |vm, c| b(c.float(0)? < c.float(1)?));
    nat!("Greater_FloatFloat", |vm, c| b(c.float(0)? > c.float(1)?));
    nat!("LessEqual_FloatFloat", |vm, c| b(c.float(0)? <= c.float(1)?));
    nat!("GreaterEqual_FloatFloat", |vm, c| b(c.float(0)? >= c.float(1)?));
    nat!("EqualEqual_FloatFloat", |vm, c| b(c.float(0)? == c.float(1)?));
    nat!("NotEqual_FloatFloat", |vm, c| b(c.float(0)? != c.float(1)?));
    nat!("ComplementEqual_FloatFloat", |vm, c| b((c.float(0)? - c.float(1)?).abs() < 1e-4));
    nat!("MultiplyEqual_FloatFloat", |vm, c| { let v = c.float(0)? * c.float(1)?; out(c, 0, Value::Float(v)) });
    nat!("DivideEqual_FloatFloat", |vm, c| {
        let d = c.float(1)?;
        let v = if d == 0.0 { vm.warn_here("Divide by zero"); 0.0 } else { c.float(0)? / d };
        out(c, 0, Value::Float(v))
    });
    nat!("AddEqual_FloatFloat", |vm, c| { let v = c.float(0)? + c.float(1)?; out(c, 0, Value::Float(v)) });
    nat!("SubtractEqual_FloatFloat", |vm, c| { let v = c.float(0)? - c.float(1)?; out(c, 0, Value::Float(v)) });
    nat!("Abs", |vm, c| f(c.float(0)?.abs()));
    nat!("Sin", |vm, c| f(c.float(0)?.sin()));
    nat!("Cos", |vm, c| f(c.float(0)?.cos()));
    nat!("Tan", |vm, c| f(c.float(0)?.tan()));
    nat!("Atan", |vm, c| f(c.float(0)?.atan()));
    nat!("Exp", |vm, c| f(c.float(0)?.exp()));
    nat!("Loge", |vm, c| f(c.float(0)?.ln()));
    nat!("Sqrt", |vm, c| f(c.float(0)?.sqrt()));
    nat!("Square", |vm, c| { let x = c.float(0)?; f(x * x) });
    nat!("FRand", |vm, c| f(vm.frand()));
    nat!("RandRange", |vm, c| { let (a, bb) = (c.float(0)?, c.float(1)?); f(a + (bb - a) * vm.frand()) });
    nat!("FMin", |vm, c| f(c.float(0)?.min(c.float(1)?)));
    nat!("FMax", |vm, c| f(c.float(0)?.max(c.float(1)?)));
    nat!("FClamp", |vm, c| { let (v, a, bb) = (c.float(0)?, c.float(1)?, c.float(2)?); f(v.max(a).min(bb)) });
    nat!("Lerp", |vm, c| { let (t, a, bb) = (c.float(0)?, c.float(1)?, c.float(2)?); f(a + t * (bb - a)) });
    nat!("Smerp", |vm, c| {
        let (t, a, bb) = (c.float(0)?, c.float(1)?, c.float(2)?);
        f(a + (3.0 * t * t - 2.0 * t * t * t) * (bb - a))
    });

    // ---- string (`==` is case-sensitive, `~=` is not)
    nat!("Concat_StrStr", |vm, c| s(format!("{}{}", c.str(0)?, c.str(1)?)));
    nat!("At_StrStr", |vm, c| s(format!("{} {}", c.str(0)?, c.str(1)?)));
    nat!("Less_StrStr", |vm, c| b(c.str(0)? < c.str(1)?));
    nat!("Greater_StrStr", |vm, c| b(c.str(0)? > c.str(1)?));
    nat!("LessEqual_StrStr", |vm, c| b(c.str(0)? <= c.str(1)?));
    nat!("GreaterEqual_StrStr", |vm, c| b(c.str(0)? >= c.str(1)?));
    nat!("EqualEqual_StrStr", |vm, c| b(c.str(0)? == c.str(1)?));
    nat!("NotEqual_StrStr", |vm, c| b(c.str(0)? != c.str(1)?));
    nat!("ComplementEqual_StrStr", |vm, c| b(c.str(0)?.eq_ignore_ascii_case(c.str(1)?)));
    nat!("Len", |vm, c| i(c.str(0)?.chars().count() as i32));
    nat!("InStr", |vm, c| {
        let (h, n) = (c.str(0)?, c.str(1)?);
        i(h.find(n).map(|p| h[..p].chars().count() as i32).unwrap_or(-1))
    });
    nat!("Mid", |vm, c| {
        let chars: Vec<char> = c.str(0)?.chars().collect();
        let start = c.int(1)?.max(0) as usize;
        let count = if c.has(2) { c.int(2)?.max(0) as usize } else { usize::MAX };
        s(chars.iter().skip(start).take(count).collect())
    });
    nat!("Left", |vm, c| s(c.str(0)?.chars().take(c.int(1)?.max(0) as usize).collect()));
    nat!("Right", |vm, c| {
        let chars: Vec<char> = c.str(0)?.chars().collect();
        let n = (c.int(1)?.max(0) as usize).min(chars.len());
        s(chars[chars.len() - n..].iter().collect())
    });
    nat!("Caps", |vm, c| s(c.str(0)?.to_uppercase()));
    nat!("Chr", |vm, c| s(char::from_u32(c.int(0)? as u32).map(String::from).unwrap_or_default()));
    nat!("Asc", |vm, c| i(c.str(0)?.chars().next().map(|ch| ch as i32).unwrap_or(0)));

    // ---- names and objects
    nat!("EqualEqual_NameName", |vm, c| b(c.name(0)? == c.name(1)?));
    nat!("NotEqual_NameName", |vm, c| b(c.name(0)? != c.name(1)?));
    nat!("EqualEqual_ObjectObject", |vm, c| b(c.obj(0)? == c.obj(1)?));
    nat!("NotEqual_ObjectObject", |vm, c| b(c.obj(0)? != c.obj(1)?));
    nat!("IsA", |vm, c| {
        let name = c.name(0)?;
        let mut class = Some(vm.class_of(c.target)?);
        while let Some(cl) = class {
            let cd = vm.world.class(cl);
            if cd.name == name {
                return b(true);
            }
            class = cd.super_;
        }
        b(false)
    });
    nat!("ClassIsChildOf", |vm, c| {
        let (Some(t), Some(p)) = (c.obj(0)?, c.obj(1)?) else { return b(false) };
        let t = vm.world.link_class(t).map_err(|e| crate::VmError::script(e.0))?;
        let p = vm.world.link_class(p).map_err(|e| crate::VmError::script(e.0))?;
        b(vm.world.is_child_of(t, p))
    });
    nat!("GetEnum", |vm, c| {
        let Some(e) = c.obj(0)? else { return Ok(Value::Name(vm.world.names.intern("None"))) };
        let id = vm.world.link_enum(e).map_err(|e| crate::VmError::script(e.0))?;
        let idx = c.int(1)?;
        let none = vm.world.names.intern("None");
        Ok(Value::Name(vm.world.enums[id.0 as usize].values.get(idx as usize).copied().unwrap_or(none)))
    });

    // ---- states
    nat!("GotoState", |vm, c| {
        let state = if c.has(0) { Some(c.name(0)?) } else { None };
        let label = if c.has(1) { Some(c.name(1)?) } else { None };
        vm.goto_state(c.target, state, label)?;
        i(0)
    });
    nat!("IsInState", |vm, c| {
        let want = c.name(0)?;
        let mut st = vm.instance(c.target)?.state;
        while let Some(s) = st {
            let sd = vm.world.state(s);
            if sd.name == want {
                return b(true);
            }
            st = sd.super_;
        }
        b(false)
    });
    nat!("GetStateName", |vm, c| {
        let st = vm.instance(c.target)?.state;
        Ok(Value::Name(match st {
            Some(s) => vm.world.state(s).name,
            None => vm.world.names.intern("None"),
        }))
    });
    // `Enable`/`Disable` switch a notification on and off for one object. Unreal only lets the
    // 64 probe names be switched; here any name the engine sends as an event can be, which is
    // the same for every name the scripts actually use.
    nat!("Enable", |vm, c| {
        let n = c.name(0)?;
        vm.instance(c.target)?.disabled.retain(|&d| d != n);
        i(0)
    });
    nat!("Disable", |vm, c| {
        let n = c.name(0)?;
        let inst = vm.instance(c.target)?;
        if !inst.disabled.contains(&n) {
            inst.disabled.push(n);
        }
        i(0)
    });

    // ---- logging
    nat!("Log", |vm, c| {
        let tag = if c.has(1) { vm.world.names.str(c.name(1)?).to_string() } else { "ScriptLog".into() };
        let line = format!("{tag}: {}", c.str(0)?);
        vm.log.push(line);
        i(0)
    });
    nat!("Warn", |vm, c| { let line = format!("ScriptWarning: {}", c.str(0)?); vm.log.push(line); i(0) });

    // ---- vectors
    nat!("Subtract_PreVector", |vm, c| v3(scale(c.vec(0)?, -1.0)));
    nat!("Multiply_VectorFloat", |vm, c| v3(scale(c.vec(0)?, c.float(1)?)));
    nat!("Multiply_FloatVector", |vm, c| v3(scale(c.vec(1)?, c.float(0)?)));
    nat!("Divide_VectorFloat", |vm, c| {
        let d = c.float(1)?;
        if d == 0.0 { vm.warn_here("Divide by zero"); v3([0.0; 3]) } else { v3(scale(c.vec(0)?, 1.0 / d)) }
    });
    nat!("Add_VectorVector", |vm, c| v3(add(c.vec(0)?, c.vec(1)?)));
    nat!("Subtract_VectorVector", |vm, c| v3(sub(c.vec(0)?, c.vec(1)?)));
    nat!("Multiply_VectorVector", |vm, c| { let (a, bb) = (c.vec(0)?, c.vec(1)?); v3([a[0] * bb[0], a[1] * bb[1], a[2] * bb[2]]) });
    nat!("EqualEqual_VectorVector", |vm, c| b(c.vec(0)? == c.vec(1)?));
    nat!("NotEqual_VectorVector", |vm, c| b(c.vec(0)? != c.vec(1)?));
    nat!("Dot_VectorVector", |vm, c| f(dot(c.vec(0)?, c.vec(1)?)));
    nat!("Cross_VectorVector", |vm, c| {
        let (a, bb) = (c.vec(0)?, c.vec(1)?);
        v3([a[1] * bb[2] - a[2] * bb[1], a[2] * bb[0] - a[0] * bb[2], a[0] * bb[1] - a[1] * bb[0]])
    });
    nat!("MultiplyEqual_VectorFloat", |vm, c| { let v = scale(c.vec(0)?, c.float(1)?); out(c, 0, vector(v[0], v[1], v[2])) });
    nat!("DivideEqual_VectorFloat", |vm, c| {
        let d = c.float(1)?;
        let v = if d == 0.0 { vm.warn_here("Divide by zero"); [0.0; 3] } else { scale(c.vec(0)?, 1.0 / d) };
        out(c, 0, vector(v[0], v[1], v[2]))
    });
    nat!("MultiplyEqual_VectorVector", |vm, c| {
        let (a, bb) = (c.vec(0)?, c.vec(1)?);
        out(c, 0, vector(a[0] * bb[0], a[1] * bb[1], a[2] * bb[2]))
    });
    nat!("AddEqual_VectorVector", |vm, c| { let v = add(c.vec(0)?, c.vec(1)?); out(c, 0, vector(v[0], v[1], v[2])) });
    nat!("SubtractEqual_VectorVector", |vm, c| { let v = sub(c.vec(0)?, c.vec(1)?); out(c, 0, vector(v[0], v[1], v[2])) });
    nat!("VSize", |vm, c| f(size(c.vec(0)?)));
    nat!("Normal", |vm, c| v3(normal(c.vec(0)?)));
    nat!("VRand", |vm, c| {
        loop {
            let v = [vm.frand() * 2.0 - 1.0, vm.frand() * 2.0 - 1.0, vm.frand() * 2.0 - 1.0];
            let l = size(v);
            if l > 1e-3 && l <= 1.0 {
                return v3(scale(v, 1.0 / l));
            }
        }
    });
    nat!("MirrorVectorByNormal", |vm, c| {
        let (v, n) = (c.vec(0)?, normal(c.vec(1)?));
        v3(sub(v, scale(n, 2.0 * dot(v, n))))
    });
    nat!("Invert", |vm, c| {
        let (x, y, z) = (c.vec(0)?, c.vec(1)?, c.vec(2)?);
        c.args[0] = vector(x[0], y[0], z[0]);
        c.args[1] = vector(x[1], y[1], z[1]);
        c.args[2] = vector(x[2], y[2], z[2]);
        i(0)
    });
    nat!("GetAxes", |vm, c| {
        let [x, y, z] = axes(c.rot(0)?);
        c.args[1] = vector(x[0], x[1], x[2]);
        c.args[2] = vector(y[0], y[1], y[2]);
        c.args[3] = vector(z[0], z[1], z[2]);
        i(0)
    });
    nat!("GetUnAxes", |vm, c| {
        let [x, y, z] = axes(c.rot(0)?);
        c.args[1] = vector(x[0], y[0], z[0]);
        c.args[2] = vector(x[1], y[1], z[1]);
        c.args[3] = vector(x[2], y[2], z[2]);
        i(0)
    });
    // `V >> R` takes a local vector to world space; `V << R` the inverse.
    nat!("GreaterGreater_VectorRotator", |vm, c| {
        let (v, [x, y, z]) = (c.vec(0)?, axes(c.rot(1)?));
        v3(add(add(scale(x, v[0]), scale(y, v[1])), scale(z, v[2])))
    });
    nat!("LessLess_VectorRotator", |vm, c| {
        let (v, [x, y, z]) = (c.vec(0)?, axes(c.rot(1)?));
        v3([dot(v, x), dot(v, y), dot(v, z)])
    });

    // ---- rotators
    nat!("EqualEqual_RotatorRotator", |vm, c| b(c.rot(0)? == c.rot(1)?));
    nat!("NotEqual_RotatorRotator", |vm, c| b(c.rot(0)? != c.rot(1)?));
    nat!("Multiply_RotatorFloat", |vm, c| { let (r, k) = (c.rot(0)?, c.float(1)?); r3(r.map(|a| (a as f32 * k) as i32)) });
    nat!("Multiply_FloatRotator", |vm, c| { let (k, r) = (c.float(0)?, c.rot(1)?); r3(r.map(|a| (a as f32 * k) as i32)) });
    nat!("Divide_RotatorFloat", |vm, c| {
        let (r, k) = (c.rot(0)?, c.float(1)?);
        if k == 0.0 { vm.warn_here("Divide by zero"); r3([0; 3]) } else { r3(r.map(|a| (a as f32 / k) as i32)) }
    });
    nat!("MultiplyEqual_RotatorFloat", |vm, c| { let (r, k) = (c.rot(0)?, c.float(1)?); let r = r.map(|a| (a as f32 * k) as i32); out(c, 0, rotator(r[0], r[1], r[2])) });
    nat!("DivideEqual_RotatorFloat", |vm, c| {
        let (r, k) = (c.rot(0)?, c.float(1)?);
        let r = if k == 0.0 { vm.warn_here("Divide by zero"); [0; 3] } else { r.map(|a| (a as f32 / k) as i32) };
        out(c, 0, rotator(r[0], r[1], r[2]))
    });
    nat!("Add_RotatorRotator", |vm, c| { let (a, bb) = (c.rot(0)?, c.rot(1)?); r3([a[0].wrapping_add(bb[0]), a[1].wrapping_add(bb[1]), a[2].wrapping_add(bb[2])]) });
    nat!("Subtract_RotatorRotator", |vm, c| { let (a, bb) = (c.rot(0)?, c.rot(1)?); r3([a[0].wrapping_sub(bb[0]), a[1].wrapping_sub(bb[1]), a[2].wrapping_sub(bb[2])]) });
    nat!("AddEqual_RotatorRotator", |vm, c| { let (a, bb) = (c.rot(0)?, c.rot(1)?); out(c, 0, rotator(a[0].wrapping_add(bb[0]), a[1].wrapping_add(bb[1]), a[2].wrapping_add(bb[2]))) });
    nat!("SubtractEqual_RotatorRotator", |vm, c| { let (a, bb) = (c.rot(0)?, c.rot(1)?); out(c, 0, rotator(a[0].wrapping_sub(bb[0]), a[1].wrapping_sub(bb[1]), a[2].wrapping_sub(bb[2]))) });
    nat!("Normalize", |vm, c| r3(c.rot(0)?.map(norm_angle)));
    nat!("RotRand", |vm, c| {
        let roll = if c.has(0) && c.bool(0)? { (vm.frand() * 65536.0) as i32 } else { 0 };
        r3([(vm.frand() * 65536.0) as i32, (vm.frand() * 65536.0) as i32, roll])
    });
    // ---- not yet backed by a subsystem
    // What the player changed is written to this engine's own `System/grim.ini`, and read back
    // over the game's settings when it next starts. The game's own files are never written to.
    nat!("SaveConfig", |vm, c| {
        if let Err(e) = vm.config.save_changes() {
            vm.warn(format!("SaveConfig: {e}"));
        }
        i(0)
    });
    nat!("StaticSaveConfig", |vm, c| {
        if let Err(e) = vm.config.save_changes() {
            vm.warn(format!("SaveConfig: {e}"));
        }
        i(0)
    });
    nat!("ResetConfig", |vm, c| { vm.warn("ResetConfig ignored (no config persistence yet)"); i(0) });
}
