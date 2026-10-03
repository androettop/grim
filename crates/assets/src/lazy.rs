//! UE1 `TLazyArray`: i32 absolute end offset + regular array (compact count + elements).
//! The v61 packages (`Detail.utx`, `Palettes.utx`) have no end offset.

use grim_package::{Error, Reader, Result};

/// No v62..v67 files exist, so the exact cutoff is unverifiable; 68 is the validated bound.
pub const LAZY_SEEK_VERSION: u16 = 68;

/// Validates the end offset against where the data really ends.
pub fn read_lazy_bytes<'a>(r: &mut Reader<'a>, version: u16, elem_size: usize) -> Result<(usize, &'a [u8])> {
    if version < LAZY_SEEK_VERSION {
        let count = r.count(elem_size)?;
        return Ok((count, r.bytes(count * elem_size)?));
    }
    let at = r.offset();
    let end = r.i32()?;
    let count = r.count(elem_size)?;
    let data = r.bytes(count * elem_size)?;
    if end as i64 != r.offset() as i64 {
        return Err(Error::invalid(
            at,
            format!("lazy array: end offset {end:#x} but data ends at {:#x}", r.offset()),
        ));
    }
    Ok((count, data))
}

pub fn read_lazy<T>(
    r: &mut Reader,
    version: u16,
    min_elem: usize,
    mut f: impl FnMut(&mut Reader) -> Result<T>,
) -> Result<Vec<T>> {
    if version < LAZY_SEEK_VERSION {
        let count = r.count(min_elem)?;
        return (0..count).map(|_| f(r)).collect();
    }
    let at = r.offset();
    let end = r.i32()?;
    let count = r.count(min_elem)?;
    let items = (0..count).map(|_| f(r)).collect::<Result<Vec<_>>>()?;
    if end as i64 != r.offset() as i64 {
        return Err(Error::invalid(
            at,
            format!("lazy array: end offset {end:#x} but data ends at {:#x}", r.offset()),
        ));
    }
    Ok(items)
}
