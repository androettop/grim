use std::fmt::Write;

/// `xxd`-style hexdump with absolute offsets; `mark` highlights one offset.
pub fn hexdump(bytes: &[u8], base: usize, mark: Option<usize>) -> String {
    let mut out = String::new();
    let first = base & !0xf;
    let last = base + bytes.len();
    let mut line = first;
    while line < last {
        let marked = mark.is_some_and(|m| m >= line && m < line + 16);
        let _ = write!(out, "{}{:08x}: ", if marked { '>' } else { ' ' }, line);
        for i in 0..16 {
            let off = line + i;
            if off >= base && off < last {
                let sep = if Some(off) == mark { '[' } else { ' ' };
                let _ = write!(out, "{sep}{:02x}", bytes[off - base]);
            } else {
                out.push_str("   ");
            }
        }
        out.push_str("  ");
        for i in 0..16 {
            let off = line + i;
            if off >= base && off < last {
                let b = bytes[off - base];
                out.push(if b.is_ascii_graphic() || b == b' ' { b as char } else { '.' });
            }
        }
        out.push('\n');
        line += 16;
    }
    out
}

pub fn hexdump_around(file: &[u8], offset: usize, before: usize, after: usize) -> String {
    let start = offset.saturating_sub(before);
    let end = (offset + after).min(file.len());
    hexdump(&file[start..end], start, Some(offset))
}
