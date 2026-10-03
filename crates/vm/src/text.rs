//! Text conversions as UnrealScript performs them (lenient leading-number parsing).

fn number_prefix(s: &str, float: bool) -> &str {
    let s = s.trim_start();
    let b = s.as_bytes();
    let mut i = 0;
    if i < b.len() && (b[i] == b'-' || b[i] == b'+') {
        i += 1;
    }
    let mut dot = false;
    while i < b.len() && (b[i].is_ascii_digit() || (float && b[i] == b'.' && !dot)) {
        dot |= b[i] == b'.';
        i += 1;
    }
    &s[..i]
}

pub fn parse_int(s: &str) -> i32 {
    number_prefix(s, false).parse::<i64>().map(|v| v as i32).unwrap_or(0)
}

pub fn parse_float(s: &str) -> f32 {
    number_prefix(s, true).parse().unwrap_or(0.0)
}

pub fn parse_bool(s: &str) -> bool {
    let t = s.trim();
    t.eq_ignore_ascii_case("true") || parse_int(t) != 0
}

/// Floats print with six decimals, like `%f`.
pub fn float_to_string(x: f32) -> String {
    format!("{x:.6}")
}
