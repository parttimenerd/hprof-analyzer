// Compress a page of i32 into MAT ArrayIntCompressed bytes.
pub fn compress_int(vals: &[i32]) -> Vec<u8> {
    // ArrayIntCompressed: BIT_LENGTH = 32, mask and shifts are Java `int`.
    // For the 32-bit case `1 << x` stays in range 0..=31, so this matches the
    // mathematically-correct leading/trailing computation.
    let mut mask: i32 = 0;
    for &v in vals {
        mask |= v;
    }
    let (varying, trailing) = mat_bits_int(mask);
    compress_impl(vals.iter().map(|&v| v as u32 as u64), varying, trailing)
}
pub fn compress_long(vals: &[i64]) -> Vec<u8> {
    // ArrayLongCompressed: BIT_LENGTH = 64, but MAT uses `1 << x` with a Java
    // `int` literal 1. When the shift amount is >= 32 the shift wraps mod 32
    // (Java int shift semantics) and the result is sign-extended to long. This
    // MAT quirk often makes it pick a WIDER varying-bit width than the minimum;
    // we must replicate it byte-for-byte.
    let mut mask: i64 = 0;
    for &v in vals {
        mask |= v;
    }
    let (varying, trailing) = mat_bits_long(mask);
    compress_impl(vals.iter().map(|&v| v as u64), varying, trailing)
}

/// Replicate ArrayIntCompressed's leading/trailing-clear-bit computation
/// (BIT_LENGTH = 32). Returns (varying_bits, trailing_clear_bits).
fn mat_bits_int(mask: i32) -> (u32, u32) {
    const BIT: i32 = 32;
    let mut leading = 0i32;
    while leading < BIT {
        let bit = 1i32.wrapping_shl((BIT - leading - 1) as u32);
        if (mask & bit) != 0 {
            break;
        }
        leading += 1;
    }
    let mut trailing = 0i32;
    while trailing < BIT - leading {
        let bit = 1i32.wrapping_shl(trailing as u32);
        if (mask & bit) != 0 {
            break;
        }
        trailing += 1;
    }
    ((BIT - leading - trailing) as u32, trailing as u32)
}

/// Replicate ArrayLongCompressed's leading/trailing-clear-bit computation
/// (BIT_LENGTH = 64) INCLUDING the Java `int`-literal shift bug: `1 << x` uses
/// int shift semantics (mask x by 31) and the resulting int is sign-extended to
/// long before the AND. Returns (varying_bits, trailing_clear_bits).
fn mat_bits_long(mask: i64) -> (u32, u32) {
    const BIT: i32 = 64;
    let mut leading = 0i32;
    while leading < BIT {
        // Java: (1 << (BIT - leading - 1)) — 1 is an int, shift wraps mod 32,
        // the int result is sign-extended to long for the AND.
        let as_int = 1i32.wrapping_shl(((BIT - leading - 1) as u32) & 31);
        let bit = as_int as i64; // sign-extend int -> long
        if (mask & bit) != 0 {
            break;
        }
        leading += 1;
    }
    let mut trailing = 0i32;
    while trailing < BIT - leading {
        let as_int = 1i32.wrapping_shl((trailing as u32) & 31);
        let bit = as_int as i64;
        if (mask & bit) != 0 {
            break;
        }
        trailing += 1;
    }
    ((BIT - leading - trailing) as u32, trailing as u32)
}

fn compress_impl(iter: impl Iterator<Item = u64> + Clone, varying: u32, trailing: u32) -> Vec<u8> {
    let mut out = Vec::new();
    out.push(varying as u8);
    out.push(trailing as u8);
    if varying == 0 {
        return out;
    }
    // Bit-writer: emit `varying` bits per value, most-significant bit first, into
    // a byte stream. Using a small accumulator that is drained every 8 bits keeps
    // us within u64 even when `varying` is up to 64 (the MAT "wide" case). This
    // matches ArrayLongCompressed.set(): value >>= trailing, then the low
    // `varying` bits are stored MSB-first.
    let value_mask: u64 = if varying >= 64 {
        u64::MAX
    } else {
        (1u64 << varying) - 1
    };
    let mut acc: u64 = 0; // holds < 8 pending bits between values
    let mut acc_bits: u32 = 0;
    for v in iter {
        let packed = (v >> trailing) & value_mask;
        // Emit the `varying` bits of `packed`, MSB first.
        let mut remaining = varying;
        while remaining > 0 {
            // How many bits we can take now without exceeding an 8-bit boundary
            // in the accumulator (we drain to bytes as soon as we have >= 8).
            let take = remaining.min(8 - acc_bits);
            let shift = remaining - take;
            let chunk = (packed >> shift) & ((1u64 << take) - 1);
            acc = (acc << take) | chunk;
            acc_bits += take;
            remaining -= take;
            if acc_bits == 8 {
                out.push(acc as u8);
                acc = 0;
                acc_bits = 0;
            }
        }
    }
    if acc_bits > 0 {
        out.push((acc << (8 - acc_bits)) as u8);
    }
    out
}

#[cfg(test)]
pub fn decode_int(bytes: &[u8], n: usize) -> Vec<i32> {
    decode_impl(bytes, n).into_iter().map(|v| v as i32).collect()
}
#[cfg(test)]
pub(crate) fn decode_long(bytes: &[u8], n: usize) -> Vec<i64> {
    decode_impl(bytes, n)
}
#[cfg(test)]
fn decode_impl(bytes: &[u8], n: usize) -> Vec<i64> {
    let varying = bytes[0] as u32;
    let trailing = bytes[1] as u32;
    if varying == 0 {
        return vec![0; n];
    }
    let mut out = Vec::with_capacity(n);
    let mut bit = 0usize;
    let data = &bytes[2..];
    for _ in 0..n {
        let mut val: u64 = 0;
        for _ in 0..varying {
            let byte = data[bit / 8];
            let b = (byte >> (7 - (bit % 8))) & 1;
            val = (val << 1) | b as u64;
            bit += 1;
        }
        out.push((val << trailing) as i64);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn compress_int_page_roundtrips_and_matches_layout() {
        let vals: Vec<i32> = vec![0, 8, 16, 24, 40];
        let bytes = compress_int(&vals);
        assert_eq!(bytes[1], 3, "trailing clear bits (all multiples of 8)");
        let out = decode_int(&bytes, vals.len());
        assert_eq!(out, vals);
        // length formula cross-check (Task 1 Step 5):
        let varying = bytes[0] as usize;
        if varying > 0 {
            assert_eq!(bytes.len(), 2 + ((vals.len() * varying - 1) / 8) + 1);
        }
    }
    #[test]
    fn compress_all_zero_page() {
        let vals = vec![0i32; 4];
        let bytes = compress_int(&vals);
        assert_eq!(bytes[0], 0, "varyingBits=0 for all-zero");
        assert_eq!(decode_int(&bytes, 4), vals);
    }
    #[test]
    fn compress_long_page_roundtrips() {
        let vals: Vec<i64> = vec![0, 0x1000, 0x2000, 0x1_0000_0000];
        let bytes = compress_long(&vals);
        assert_eq!(decode_long(&bytes, vals.len()), vals);
    }
}
