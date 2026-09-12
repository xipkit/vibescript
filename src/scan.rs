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
            let (ch, n, valid) = rune(&s[span.len..]);
            if !valid
                || (matches!(class, Class::JsonStringify) && matches!(ch, '\u{2028}' | '\u{2029}'))
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

/// Decodes one rune, replacing each invalid byte as Go's UTF-8 decoder does.
pub(crate) fn rune(s: &[u8]) -> (char, usize, bool) {
    let b = s[0];
    if b < 128 {
        return (b as char, 1, true);
    }
    let n = match b {
        0xc2..=0xdf => 2,
        0xe0..=0xef => 3,
        0xf0..=0xf4 => 4,
        _ => return ('\u{fffd}', 1, false),
    };
    if s.len() < n {
        return ('\u{fffd}', 1, false);
    }
    let second = s[1];
    if second & 0xc0 != 0x80
        || (b == 0xe0 && second < 0xa0)
        || (b == 0xed && second >= 0xa0)
        || (b == 0xf0 && second < 0x90)
        || (b == 0xf4 && second >= 0x90)
    {
        return ('\u{fffd}', 1, false);
    }
    let mut cp = u32::from(b & (0x7f >> n));
    for &byte in &s[1..n] {
        if byte & 0xc0 != 0x80 {
            return ('\u{fffd}', 1, false);
        }
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
