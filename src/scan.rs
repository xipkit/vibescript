#[derive(Clone, Copy)]
pub(crate) enum Class {
    Ascii,
    JsonParse,
    JsonStringify,
}

fn ordinary(b: u8, class: Class) -> bool {
    match class {
        Class::Ascii => b < 128,
        Class::JsonParse => (32..128).contains(&b) && b != b'"' && b != b'\\',
        Class::JsonStringify => {
            (32..128).contains(&b) && b != b'"' && b != b'\\' && b != b'<' && b != b'>' && b != b'&'
        }
    }
}

pub(crate) fn prefix(s: &[u8], class: Class) -> usize {
    let mut i = 0;
    #[cfg(all(feature = "simd", any(target_arch = "aarch64", target_arch = "x86_64")))]
    while s.len() - i >= 16 {
        // SAFETY: each unaligned vector load stays within this 16-byte slice;
        // NEON and SSE2 are baseline features of the respective target architectures.
        if !unsafe { vector_ordinary(&s[i..i + 16], class) } {
            break;
        }
        i += 16;
    }
    while i < s.len() && ordinary(s[i], class) {
        i += 1;
    }
    i
}

#[derive(Default)]
pub(crate) struct TextSpan {
    pub len: usize,
    pub runes: usize,
    pub steps: u64,
}

/// Scans valid UTF-8 until an escape, invalid byte, or incomplete trailing rune.
pub(crate) fn text_span(s: &[u8], class: Class) -> TextSpan {
    let mut span = TextSpan::default();
    while span.len < s.len() {
        if s[span.len] < 128 {
            let n = prefix(&s[span.len..], class);
            if n == 0 {
                break;
            }
            span.len += n;
            span.runes += n;
            span.steps += (n as u64).div_ceil(64);
        } else {
            let tail = &s[span.len..];
            let (n, valid) = rune_width(tail);
            if !valid
                || (matches!(class, Class::JsonStringify)
                    && n == 3
                    && tail[0] == 0xe2
                    && tail[1] == 0x80
                    && matches!(tail[2], 0xa8 | 0xa9))
            {
                break;
            }
            span.len += n;
            span.runes += 1;
            span.steps += 1;
        }
    }
    span
}

/// Counts a run of valid non-ASCII characters without constructing code points.
pub(crate) fn unicode_span(s: &[u8]) -> TextSpan {
    let mut len = 0;
    let mut runes = 0;
    while len < s.len() && s[len] >= 128 {
        let (n, valid) = rune_width(&s[len..]);
        if !valid {
            break;
        }
        len += n;
        runes += 1;
    }
    TextSpan {
        len,
        runes,
        steps: runes as u64,
    }
}

pub(crate) fn ascii_case(s: &mut [u8], upper: bool) {
    #[cfg_attr(
        not(all(feature = "simd", any(target_arch = "aarch64", target_arch = "x86_64"))),
        allow(unused_mut)
    )]
    let mut i = 0;
    #[cfg(all(feature = "simd", any(target_arch = "aarch64", target_arch = "x86_64")))]
    while s.len() - i >= 16 {
        // SAFETY: the exclusive slice contains all 16 bytes loaded and stored;
        // the instructions are baseline features of the target architecture.
        unsafe {
            vector_case(&mut s[i..i + 16], upper);
        }
        i += 16;
    }
    for b in &mut s[i..] {
        *b = if upper {
            b.to_ascii_uppercase()
        } else {
            b.to_ascii_lowercase()
        };
    }
}

#[cfg(all(feature = "simd", target_arch = "aarch64"))]
unsafe fn vector_ordinary(s: &[u8], class: Class) -> bool {
    use std::arch::aarch64::*;
    // SAFETY: caller provides 16 accessible bytes and a NEON-capable target.
    unsafe {
        let v = vld1q_u8(s.as_ptr());
        let mut bad = vcgeq_u8(v, vdupq_n_u8(128));
        if !matches!(class, Class::Ascii) {
            bad = vorrq_u8(bad, vcltq_u8(v, vdupq_n_u8(32)));
            bad = vorrq_u8(bad, vceqq_u8(v, vdupq_n_u8(b'"')));
            bad = vorrq_u8(bad, vceqq_u8(v, vdupq_n_u8(b'\\')));
        }
        if matches!(class, Class::JsonStringify) {
            for b in *b"<>&" {
                bad = vorrq_u8(bad, vceqq_u8(v, vdupq_n_u8(b)));
            }
        }
        vmaxvq_u8(bad) == 0
    }
}

#[cfg(all(feature = "simd", target_arch = "aarch64"))]
unsafe fn vector_case(s: &mut [u8], upper: bool) {
    use std::arch::aarch64::*;
    // SAFETY: caller provides exclusive access to 16 bytes on a NEON target.
    unsafe {
        let v = vld1q_u8(s.as_ptr());
        let lo = if upper { b'a' } else { b'A' };
        let hi = lo + 25;
        let mask = vandq_u8(vcgeq_u8(v, vdupq_n_u8(lo)), vcleq_u8(v, vdupq_n_u8(hi)));
        let flipped = veorq_u8(v, vandq_u8(mask, vdupq_n_u8(32)));
        vst1q_u8(s.as_mut_ptr(), flipped);
    }
}

#[cfg(all(feature = "simd", target_arch = "x86_64"))]
unsafe fn vector_ordinary(s: &[u8], class: Class) -> bool {
    use std::arch::x86_64::*;
    // SAFETY: caller provides 16 accessible bytes; x86_64 guarantees SSE2.
    unsafe {
        let v = _mm_loadu_si128(s.as_ptr().cast());
        if matches!(class, Class::Ascii) {
            return _mm_movemask_epi8(v) == 0;
        }
        let mut bad = _mm_cmplt_epi8(v, _mm_set1_epi8(32));
        for b in *b"\"\\" {
            bad = _mm_or_si128(bad, _mm_cmpeq_epi8(v, _mm_set1_epi8(b as i8)));
        }
        if matches!(class, Class::JsonStringify) {
            for b in *b"<>&" {
                bad = _mm_or_si128(bad, _mm_cmpeq_epi8(v, _mm_set1_epi8(b as i8)));
            }
        }
        _mm_movemask_epi8(bad) == 0
    }
}

#[cfg(all(feature = "simd", target_arch = "x86_64"))]
unsafe fn vector_case(s: &mut [u8], upper: bool) {
    use std::arch::x86_64::*;
    // SAFETY: caller provides exclusive access to 16 bytes; x86_64 guarantees SSE2.
    unsafe {
        let v = _mm_loadu_si128(s.as_ptr().cast());
        let lo = if upper { b'a' } else { b'A' };
        let mask = _mm_and_si128(
            _mm_cmpgt_epi8(v, _mm_set1_epi8((lo - 1) as i8)),
            _mm_cmplt_epi8(v, _mm_set1_epi8((lo + 26) as i8)),
        );
        _mm_storeu_si128(
            s.as_mut_ptr().cast(),
            _mm_xor_si128(v, _mm_and_si128(mask, _mm_set1_epi8(32))),
        );
    }
}

// Low bits give sequence width; high bits select the legal second-byte range.
const UTF8_LEAD: [u8; 256] = {
    let mut table = [0; 256];
    let mut b = 0;
    while b < 256 {
        table[b] = match b {
            0..=0x7f => 1,
            0xc2..=0xdf => 2,
            0xe0 => 0x13,
            0xe1..=0xec | 0xee..=0xef => 3,
            0xed => 0x23,
            0xf0 => 0x34,
            0xf1..=0xf3 => 4,
            0xf4 => 0x44,
            _ => 0,
        };
        b += 1;
    }
    table
};

fn rune_width(s: &[u8]) -> (usize, bool) {
    let tag = UTF8_LEAD[usize::from(s[0])];
    let n = usize::from(tag & 7);
    if n <= 1 {
        return (1, n == 1);
    }
    if s.len() < n {
        return (1, false);
    }
    let range = usize::from(tag >> 4);
    let low = [0x80u8, 0xa0, 0x80, 0x90, 0x80][range];
    let high = [0xbfu8, 0xbf, 0x9f, 0xbf, 0x8f][range];
    if s[1].wrapping_sub(low) > high - low
        || (n >= 3 && s[2] & 0xc0 != 0x80)
        || (n == 4 && s[3] & 0xc0 != 0x80)
    {
        return (1, false);
    }
    (n, true)
}

/// Decodes one rune, replacing each invalid byte as Go's UTF-8 decoder does.
pub(crate) fn rune(s: &[u8]) -> (char, usize, bool) {
    if s[0] < 128 {
        return (s[0] as char, 1, true);
    }
    let (n, valid) = rune_width(s);
    if !valid {
        return ('\u{fffd}', 1, false);
    }
    let mut cp = u32::from(s[0] & (0x7f >> n));
    for &byte in &s[1..n] {
        cp = (cp << 6) | u32::from(byte & 0x3f);
    }
    (char::from_u32(cp).unwrap(), n, true)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reference_rune(bytes: &[u8]) -> (char, usize, bool) {
        let valid = match std::str::from_utf8(bytes) {
            Ok(text) => text,
            Err(error) => std::str::from_utf8(&bytes[..error.valid_up_to()]).unwrap(),
        };
        match valid.chars().next() {
            Some(ch) => (ch, ch.len_utf8(), true),
            None => ('\u{fffd}', 1, false),
        }
    }

    #[test]
    fn decoder_matches_every_unicode_scalar_and_invalid_prefixes() {
        for cp in 0..=0x10ffff {
            if let Some(ch) = char::from_u32(cp) {
                let mut buffer = [0; 4];
                let bytes = ch.encode_utf8(&mut buffer).as_bytes();
                assert_eq!(rune(bytes), (ch, bytes.len(), true), "U+{cp:04X}");
                for len in 1..bytes.len() {
                    assert_eq!(rune(&bytes[..len]), ('\u{fffd}', 1, false));
                }
            }
        }
        for a in 0..=255 {
            for b in 0..=255 {
                for tail in [[0, 0], [0x80, 0x80], [0xbf, 0xbf], [0xff, 0xff]] {
                    let bytes = [a, b, tail[0], tail[1]];
                    for len in 1..=bytes.len() {
                        assert_eq!(
                            rune(&bytes[..len]),
                            reference_rune(&bytes[..len]),
                            "{bytes:02x?}/{len}"
                        );
                    }
                }
            }
        }
    }
    #[test]
    fn classifiers_match_scalar_at_every_lane() {
        for class in [Class::Ascii, Class::JsonParse, Class::JsonStringify] {
            for len in [0, 1, 15, 16, 17, 31, 32, 33, 127] {
                for pos in 0..len {
                    for byte in 0..=255 {
                        let mut data = vec![b'a'; len];
                        data[pos] = byte;
                        assert_eq!(
                            prefix(&data, class),
                            data.iter()
                                .position(|&b| !ordinary(b, class))
                                .unwrap_or(len)
                        );
                    }
                }
            }
        }
    }
    #[test]
    fn case_matches_scalar_including_unaligned_slices() {
        for upper in [false, true] {
            for offset in 0..16 {
                for len in 0..80 {
                    let mut data: Vec<u8> = (0..offset + len).map(|i| (i * 31) as u8).collect();
                    let expected: Vec<_> = data[offset..]
                        .iter()
                        .map(|b| {
                            if upper {
                                b.to_ascii_uppercase()
                            } else {
                                b.to_ascii_lowercase()
                            }
                        })
                        .collect();
                    ascii_case(&mut data[offset..], upper);
                    assert_eq!(&data[offset..], expected);
                }
            }
        }
    }
}
