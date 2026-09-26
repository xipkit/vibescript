#[derive(Clone, Copy)]
pub(crate) enum Class {
    Ascii,
    JsonParse,
    JsonStringify,
}

pub(crate) fn ordinary(b: u8, class: Class) -> bool {
    match class {
        Class::Ascii => b < 128,
        Class::JsonParse => (32..128).contains(&b) && b != b'"' && b != b'\\',
        Class::JsonStringify => {
            (32..128).contains(&b) && b != b'"' && b != b'\\' && b != b'<' && b != b'>' && b != b'&'
        }
    }
}

pub(crate) fn prefix(s: &[u8], class: Class) -> usize {
    if s.is_empty() || !ordinary(s[0], class) {
        return 0;
    }
    let mut i = 0;
    #[cfg(all(feature = "simd", any(target_arch = "aarch64", target_arch = "x86_64")))]
    while s.len() - i >= 16 {
        // SAFETY: each unaligned vector load stays within this 16-byte slice;
        // NEON and SSE2 are baseline features of the respective target architectures.
        let n = unsafe { vector_prefix(&s[i..i + 16], class) };
        if n != 16 {
            return i + n;
        }
        i += 16;
    }
    while s.len() - i >= 8 {
        let v = u64::from_le_bytes(s[i..i + 8].try_into().unwrap());
        let equal = |byte: u8| {
            let x = v ^ (u64::from(byte) * 0x0101010101010101);
            !(((x & 0x7f7f7f7f7f7f7f7f) + 0x7f7f7f7f7f7f7f7f) | x | 0x7f7f7f7f7f7f7f7f)
        };
        let mut bad = v & 0x8080808080808080;
        if !matches!(class, Class::Ascii) {
            bad |= !(v | ((v & 0x7f7f7f7f7f7f7f7f) + 0x6060606060606060)) & 0x8080808080808080;
            bad |= equal(b'"') | equal(b'\\');
        }
        if matches!(class, Class::JsonStringify) {
            bad |= equal(b'<') | equal(b'>') | equal(b'&');
        }
        if bad != 0 {
            return i + bad.trailing_zeros() as usize / 8;
        }
        i += 8;
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
            if span.len < s.len() && s[span.len] < 128 {
                break;
            }
        } else {
            let tail = &s[span.len..];
            if matches!(class, Class::JsonParse) {
                let unicode = unicode_span(tail);
                if unicode.len == 0 {
                    break;
                }
                span.len += unicode.len;
                span.runes += unicode.runes;
                span.steps += unicode.steps;
                continue;
            }
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
    #[cfg(all(feature = "simd", any(target_arch = "aarch64", target_arch = "x86_64")))]
    // SAFETY: NEON and SSE2 are baseline features of these architectures.
    let (mut len, mut runes) = unsafe { vector_unicode(s) };
    #[cfg(not(all(feature = "simd", any(target_arch = "aarch64", target_arch = "x86_64"))))]
    let (mut len, mut runes) = (0, 0);
    // While a whole four-byte sequence fits, the width comes from the lead's
    // leading ones rather than a table load, which keeps the loop's carried
    // dependency short; the table only validates, off that path.
    while len + 4 <= s.len() {
        let lead = s[len];
        let n = lead.leading_ones() as usize;
        let tag = UTF8_LEAD[usize::from(lead)];
        if !(2..=4).contains(&n) || usize::from(tag & 7) != n {
            break;
        }
        let range = usize::from(tag >> 4);
        let low = [0x80u8, 0xa0, 0x80, 0x90, 0x80][range];
        let high = [0xbfu8, 0xbf, 0x9f, 0xbf, 0x8f][range];
        if s[len + 1].wrapping_sub(low) > high - low
            || (n >= 3 && s[len + 2] & 0xc0 != 0x80)
            || (n == 4 && s[len + 3] & 0xc0 != 0x80)
        {
            break;
        }
        len += n;
        runes += 1;
    }
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
unsafe fn vector_prefix(s: &[u8], class: Class) -> usize {
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
        let bits = vget_lane_u64::<0>(vreinterpret_u64_u8(vshrn_n_u16::<4>(vreinterpretq_u16_u8(
            bad,
        ))));
        bits.trailing_zeros() as usize / 4
    }
}

#[cfg(all(feature = "simd", target_arch = "x86_64"))]
unsafe fn vector_unicode(s: &[u8]) -> (usize, usize) {
    use std::arch::x86_64::*;
    // SAFETY: the loop bounds every unaligned 16-byte load; SSE2 is baseline.
    unsafe {
        let at_least = |v, b: u8| {
            _mm_cmpgt_epi8(
                _mm_xor_si128(v, _mm_set1_epi8(-128)),
                _mm_set1_epi8((b ^ 128).wrapping_sub(1) as i8),
            )
        };
        let below = |v, b| _mm_andnot_si128(at_least(v, b), _mm_set1_epi8(-1));
        let equal = |v, b: u8| _mm_cmpeq_epi8(v, _mm_set1_epi8(b as i8));
        let mut previous = _mm_setzero_si128();
        let (mut start, mut leads, mut boundary) = (0, 0, 0);
        while s.len() - start >= 16 {
            let v = _mm_loadu_si128(s.as_ptr().add(start).cast());
            let prev1 = _mm_or_si128(_mm_slli_si128::<1>(v), _mm_srli_si128::<15>(previous));
            let prev2 = _mm_or_si128(_mm_slli_si128::<2>(v), _mm_srli_si128::<14>(previous));
            let prev3 = _mm_or_si128(_mm_slli_si128::<3>(v), _mm_srli_si128::<13>(previous));
            let continuation = below(v, 0xc0);
            let expected = _mm_or_si128(
                _mm_or_si128(at_least(prev1, 0xc0), at_least(prev2, 0xe0)),
                at_least(prev3, 0xf0),
            );
            let mut bad = _mm_or_si128(_mm_xor_si128(continuation, expected), below(v, 0x80));
            bad = _mm_or_si128(bad, _mm_and_si128(equal(prev1, 0xe0), below(v, 0xa0)));
            bad = _mm_or_si128(bad, _mm_and_si128(equal(prev1, 0xed), at_least(v, 0xa0)));
            bad = _mm_or_si128(bad, _mm_and_si128(equal(prev1, 0xf0), below(v, 0x90)));
            bad = _mm_or_si128(bad, _mm_and_si128(equal(prev1, 0xf4), at_least(v, 0x90)));
            bad = _mm_or_si128(
                bad,
                _mm_andnot_si128(
                    continuation,
                    _mm_or_si128(below(v, 0xc2), at_least(v, 0xf5)),
                ),
            );
            if _mm_movemask_epi8(bad) != 0 {
                break;
            }
            let lead = (!_mm_movemask_epi8(continuation) as u32) & 0xffff;
            leads += lead.count_ones() as usize;
            boundary = start + (31 - lead.leading_zeros()) as usize;
            previous = v;
            start += 16;
        }
        (boundary, leads.saturating_sub(1))
    }
}

/// Validates `s` 16 bytes at a time as `rune_width` would decode it, from a
/// rune boundary until a block holds ASCII, an invalid byte or too few bytes.
/// Returns the start of the last rune a valid block began, which may continue
/// past it and so is left to the scalar decoder, and the number of complete
/// runes before that point.
#[cfg(all(feature = "simd", target_arch = "aarch64"))]
unsafe fn vector_unicode(s: &[u8]) -> (usize, usize) {
    use std::arch::aarch64::*;
    // SAFETY: every load reads the 16 bytes at `start`, which the loop keeps
    // within `s`; the caller guarantees a NEON-capable target.
    unsafe {
        let at_least = |v, b| vcgeq_u8(v, vdupq_n_u8(b));
        let below = |v, b| vcltq_u8(v, vdupq_n_u8(b));
        let equal = |v, b| vceqq_u8(v, vdupq_n_u8(b));
        // Nothing before a rune boundary expects a continuation byte.
        let mut previous = vdupq_n_u8(0);
        // Lead bytes are counted per lane and summed every 255 blocks, before
        // a lane can overflow; the last valid block's leads give the boundary.
        let (mut leads, mut pending, mut blocks) = (0, vdupq_n_u8(0), 0);
        let mut last = None;
        let mut start = 0;
        while s.len() - start >= 16 {
            let v = vld1q_u8(s.as_ptr().add(start));
            let prev1 = vextq_u8::<15>(previous, v);
            let prev2 = vextq_u8::<14>(previous, v);
            let prev3 = vextq_u8::<13>(previous, v);
            let continuation = below(v, 0xc0);
            let expected = vorrq_u8(
                vorrq_u8(at_least(prev1, 0xc0), at_least(prev2, 0xe0)),
                at_least(prev3, 0xf0),
            );
            let mut bad = vorrq_u8(veorq_u8(continuation, expected), below(v, 0x80));
            bad = vorrq_u8(bad, vandq_u8(equal(prev1, 0xe0), below(v, 0xa0)));
            bad = vorrq_u8(bad, vandq_u8(equal(prev1, 0xed), at_least(v, 0xa0)));
            bad = vorrq_u8(bad, vandq_u8(equal(prev1, 0xf0), below(v, 0x90)));
            bad = vorrq_u8(bad, vandq_u8(equal(prev1, 0xf4), at_least(v, 0x90)));
            bad = vorrq_u8(
                bad,
                vbicq_u8(vorrq_u8(below(v, 0xc2), at_least(v, 0xf5)), continuation),
            );
            if vmaxvq_u8(bad) != 0 {
                break;
            }
            let lead = vmvnq_u8(continuation);
            // A lead mask lane is 0xff, so subtracting it counts one.
            pending = vsubq_u8(pending, lead);
            blocks += 1;
            if blocks == 255 {
                leads += usize::from(vaddlvq_u8(pending));
                (pending, blocks) = (vdupq_n_u8(0), 0);
            }
            last = Some((start, lead));
            previous = v;
            start += 16;
        }
        let Some((start, lead)) = last else {
            return (0, 0);
        };
        leads += usize::from(vaddlvq_u8(pending));
        // Four mask bits per byte, so lane i owns bits 4i..4i+3. A valid
        // block starts a rune in every four bytes, so `bits` is nonzero.
        let bits = vget_lane_u64::<0>(vreinterpret_u64_u8(vshrn_n_u16::<4>(vreinterpretq_u16_u8(
            lead,
        ))));
        (start + (63 - bits.leading_zeros() as usize) / 4, leads - 1)
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
unsafe fn vector_prefix(s: &[u8], class: Class) -> usize {
    use std::arch::x86_64::*;
    // SAFETY: caller provides 16 accessible bytes; x86_64 guarantees SSE2.
    unsafe {
        let v = _mm_loadu_si128(s.as_ptr().cast());
        if matches!(class, Class::Ascii) {
            return (_mm_movemask_epi8(v) as u32 | (1 << 16)).trailing_zeros() as usize;
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
        (_mm_movemask_epi8(bad) as u32 | (1 << 16)).trailing_zeros() as usize
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

    /// The rune-at-a-time span the vector and wide paths must reproduce.
    fn reference_unicode_span(s: &[u8]) -> (usize, usize) {
        let (mut len, mut runes) = (0, 0);
        while len < s.len() && s[len] >= 128 {
            let (n, valid) = rune_width(&s[len..]);
            if !valid {
                break;
            }
            len += n;
            runes += 1;
        }
        (len, runes)
    }

    #[test]
    fn unicode_spans_match_rune_at_a_time_decoding() {
        let pieces: [&[u8]; 24] = [
            b"\xc2\x80",
            b"\xdf\xbf",
            "é".as_bytes(),
            b"\xe0\xa0\x80",
            b"\xe0\x9f\xbf",
            b"\xed\x9f\xbf",
            b"\xed\xa0\x80",
            b"\xef\xbf\xbf",
            "界".as_bytes(),
            b"\xf0\x90\x80\x80",
            b"\xf0\x8f\xbf\xbf",
            b"\xf4\x8f\xbf\xbf",
            b"\xf4\x90\x80\x80",
            "🙂".as_bytes(),
            b"\xc0\x80",
            b"\xc1\xbf",
            b"\xf5\x80\x80\x80",
            b"\xff",
            b"\x80",
            b"\xbf",
            b"\xe7\x95",
            b"\xf0\x9f\x99",
            b"a",
            b"\xe1\x80\x80",
        ];
        // Uniform text makes the same lanes lead in every block, the case
        // where per-lane counts come closest to overflowing.
        for text in ["é", "界", "🙂", "é界🙂"] {
            for tail in [&b""[..], b"a", b"\xff", b"\xe7\x95"] {
                let mut input = text.repeat(8000 / text.len()).into_bytes();
                input.extend_from_slice(tail);
                let span = unicode_span(&input);
                let (len, runes) = reference_unicode_span(&input);
                assert_eq!((span.len, span.runes), (len, runes), "{text} {tail:02x?}");
            }
        }
        let mut seed = 0x2545f4914f6cdd1du64;
        let mut next = move |bound: usize| {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            (seed % bound as u64) as usize
        };
        for case in 0..20000 {
            let mut input = Vec::new();
            // Mostly valid text, so spans run long enough to cross blocks.
            let valid = next(4) != 0;
            // Some inputs outgrow the 255 blocks a vector lane can count.
            let target = if case % 50 == 0 {
                4000 + next(4000)
            } else {
                next(200) + 1
            };
            while input.len() < target {
                let piece = if valid && next(40 + input.len() / 8) != 0 {
                    pieces[[2, 8, 13, 0, 1, 3, 5, 7, 9, 11, 23][next(11)]]
                } else {
                    pieces[next(pieces.len())]
                };
                input.extend_from_slice(piece);
            }
            for start in 0..input.len().min(5) {
                for end in [input.len(), input.len().saturating_sub(1).max(start)] {
                    let slice = &input[start..end];
                    let span = unicode_span(slice);
                    assert_eq!(
                        (span.len, span.runes, span.steps),
                        {
                            let (len, runes) = reference_unicode_span(slice);
                            (len, runes, runes as u64)
                        },
                        "case {case}: {slice:02x?}"
                    );
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
