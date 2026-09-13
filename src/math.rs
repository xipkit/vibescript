// Adapted from Go's math package to preserve the reference's floating-point results.
// Copyright 2009, 2011, 2017, 2018 The Go Authors. All rights reserved.
// See licenses/Go-BSD-3-Clause.txt and licenses/math-NOTICE.txt.
#![allow(clippy::excessive_precision)]

use std::f64::consts::{FRAC_1_SQRT_2, FRAC_PI_2, FRAC_PI_4, LOG2_E, LOG10_E, PI};

fn frexp(mut x: f64) -> (f64, i32) {
    if x == 0.0 || !x.is_finite() {
        return (x, 0);
    }
    let mut exponent = 0;
    if x.abs() < f64::MIN_POSITIVE {
        x *= (1u64 << 52) as f64;
        exponent = -52;
    }
    let bits = x.to_bits();
    exponent += ((bits >> 52) & 0x7ff) as i32 - 1022;
    (
        f64::from_bits((bits & !(0x7ff << 52)) | (1022 << 52)),
        exponent,
    )
}

pub(crate) fn log(x: f64) -> f64 {
    const LN2_HI: f64 = 6.93147180369123816490e-01;
    const LN2_LO: f64 = 1.90821492927058770002e-10;
    const L1: f64 = 6.666666666666735130e-01;
    const L2: f64 = 3.999999999940941908e-01;
    const L3: f64 = 2.857142874366239149e-01;
    const L4: f64 = 2.222219843214978396e-01;
    const L5: f64 = 1.818357216161805012e-01;
    const L6: f64 = 1.531383769920937332e-01;
    const L7: f64 = 1.479819860511658591e-01;
    if x.is_nan() || x == f64::INFINITY {
        return x;
    }
    if x < 0.0 {
        return f64::NAN;
    }
    if x == 0.0 {
        return f64::NEG_INFINITY;
    }
    let (mut fraction, mut exponent) = frexp(x);
    if fraction < FRAC_1_SQRT_2 {
        fraction *= 2.0;
        exponent -= 1;
    }
    let f = fraction - 1.0;
    let k = f64::from(exponent);
    let s = f / (2.0 + f);
    let s2 = s * s;
    let s4 = s2 * s2;
    let p = s4.mul_add(s4.mul_add(s4.mul_add(L7, L5), L3), L1);
    let t2 = s4 * s4.mul_add(s4.mul_add(L6, L4), L2);
    let r = s2.mul_add(p, t2);
    let half_f = 0.5 * f;
    let sum = s.mul_add(half_f.mul_add(f, r), k * LN2_LO);
    k.mul_add(LN2_HI, -(half_f.mul_add(f, -sum) - f))
}

pub(crate) fn log2(x: f64) -> f64 {
    let (fraction, exponent) = frexp(x);
    if fraction == 0.5 {
        f64::from(exponent - 1)
    } else {
        log(fraction).mul_add(LOG2_E, f64::from(exponent))
    }
}

pub(crate) fn log10(x: f64) -> f64 {
    log(x) * LOG10_E
}

pub(crate) fn exp(x: f64) -> f64 {
    const LN2_HI: f64 = 6.93147180369123816490e-01;
    const LN2_LO: f64 = 1.90821492927058770002e-10;
    const P: [f64; 5] = [
        4.13813679705723846039e-08,
        -1.65339022054652515390e-06,
        6.61375632143793436117e-05,
        -2.77777777770155933842e-03,
        1.66666666666666657415e-01,
    ];
    if x.is_nan() {
        return x;
    }
    if x > 7.09782712893383973096e+02 {
        return f64::INFINITY;
    }
    if x < -7.45133219101941108420e+02 {
        return 0.0;
    }
    if x.abs() < f64::from_bits(0x3e30000000000000) {
        return 1.0 + x;
    }
    let k = x.mul_add(LOG2_E, if x < 0.0 { -0.5 } else { 0.5 }) as i32;
    let hi = (-f64::from(k)).mul_add(LN2_HI, x);
    let lo = f64::from(k) * LN2_LO;
    let r = hi - lo;
    let t = r * r;
    let c = (-t).mul_add(polynomial(t, &P), r);
    let y = 1.0 - ((lo - (r * c) / (2.0 - c)) - hi);
    let bits = y.to_bits();
    let mut exponent = (bits >> 52) as i32 + k;
    let mut multiplier = 1.0;
    if exponent < 1 {
        exponent += 52;
        multiplier = f64::from_bits(0x3cb0000000000000);
    }
    f64::from_bits((bits & ((1 << 52) - 1)) | ((exponent as u64) << 52)) * multiplier
}

pub(crate) fn hypot(mut p: f64, mut q: f64) -> f64 {
    p = p.abs();
    q = q.abs();
    if p.is_infinite() || q.is_infinite() {
        return f64::INFINITY;
    }
    if p.is_nan() || q.is_nan() {
        return f64::NAN;
    }
    if p < q {
        std::mem::swap(&mut p, &mut q);
    }
    if p == 0.0 {
        return 0.0;
    }
    q /= p;
    p * q.mul_add(q, 1.0).sqrt()
}

pub(crate) fn cbrt(x: f64) -> f64 {
    const C: f64 = 5.42857142857142815906e-01;
    const D: f64 = -7.05306122448979611050e-01;
    const E: f64 = 1.41428571428571436819e+00;
    const F: f64 = 1.60714285714285720630e+00;
    const G: f64 = 3.57142857142857150787e-01;
    if x == 0.0 || !x.is_finite() {
        return x;
    }
    let a = x.abs();
    let mut t = f64::from_bits(a.to_bits() / 3 + (715094163u64 << 32));
    if a < f64::MIN_POSITIVE {
        t = (1u64 << 54) as f64 * a;
        t = f64::from_bits(t.to_bits() / 3 + (696219795u64 << 32));
    }
    let r = t * t / a;
    let s = r.mul_add(t, C);
    t *= G + F / (s + E + D / s);
    t = f64::from_bits((t.to_bits() & (0xffffffffcu64 << 28)) + (1 << 30));
    let s = t * t;
    let r = a / s;
    let w = t + t;
    let r = (r - t) / (w + r);
    t = t.mul_add(r, t);
    t.copysign(x)
}

fn atan_series(x: f64) -> f64 {
    const P0: f64 = -8.750608600031904122785e-01;
    const P1: f64 = -1.615753718733365076637e+01;
    const P2: f64 = -7.500855792314704667340e+01;
    const P3: f64 = -1.228866684490136173410e+02;
    const P4: f64 = -6.485021904942025371773e+01;
    const Q0: f64 = 2.485846490142306297962e+01;
    const Q1: f64 = 1.650270098316988542046e+02;
    const Q2: f64 = 4.328810604912902668951e+02;
    const Q3: f64 = 4.853903996359136964868e+02;
    const Q4: f64 = 1.945506571482613964425e+02;
    let z = x * x;
    let numerator = z * polynomial(z, &[P0, P1, P2, P3, P4]);
    let denominator = x
        .mul_add(x, Q0)
        .mul_add(z, Q1)
        .mul_add(z, Q2)
        .mul_add(z, Q3)
        .mul_add(z, Q4);
    x.mul_add(numerator / denominator, x)
}

fn positive_atan(x: f64) -> f64 {
    const MORE_BITS: f64 = 6.123233995736765886130e-17;
    const TAN_3_PI_8: f64 = 2.41421356237309504880;
    if x <= 0.66 {
        atan_series(x)
    } else if x > TAN_3_PI_8 {
        FRAC_PI_2 - atan_series(1.0 / x) + MORE_BITS
    } else {
        FRAC_PI_4 + atan_series((x - 1.0) / (x + 1.0)) + 0.5 * MORE_BITS
    }
}

pub(crate) fn atan(x: f64) -> f64 {
    if x == 0.0 {
        x
    } else if x > 0.0 {
        positive_atan(x)
    } else {
        -positive_atan(-x)
    }
}

pub(crate) fn asin(x: f64) -> f64 {
    if x == 0.0 {
        return x;
    }
    let a = x.abs();
    if a > 1.0 {
        return f64::NAN;
    }
    let root = (-a).mul_add(a, 1.0).sqrt();
    let result = if a > 0.7 {
        FRAC_PI_2 - positive_atan(root / a)
    } else {
        positive_atan(a / root)
    };
    result.copysign(x)
}

pub(crate) fn acos(x: f64) -> f64 {
    FRAC_PI_2 - asin(x)
}

pub(crate) fn atan2(y: f64, x: f64) -> f64 {
    if y.is_nan() || x.is_nan() {
        return f64::NAN;
    }
    if y == 0.0 {
        return if x >= 0.0 && !x.is_sign_negative() {
            0.0f64.copysign(y)
        } else {
            PI.copysign(y)
        };
    }
    if x == 0.0 {
        return FRAC_PI_2.copysign(y);
    }
    if x.is_infinite() {
        return match (x.is_sign_positive(), y.is_infinite()) {
            (true, true) => FRAC_PI_4.copysign(y),
            (true, false) => 0.0f64.copysign(y),
            (false, true) => (3.0 * FRAC_PI_4).copysign(y),
            (false, false) => PI.copysign(y),
        };
    }
    if y.is_infinite() {
        return FRAC_PI_2.copysign(y);
    }
    let q = atan(y / x);
    if x < 0.0 {
        if q <= 0.0 { q + PI } else { q - PI }
    } else {
        q
    }
}

const SIN: [f64; 6] = [
    1.58962301576546568060e-10,
    -2.50507477628578072866e-8,
    2.75573136213857245213e-6,
    -1.98412698295895385996e-4,
    8.33333333332211858878e-3,
    -1.66666666666666307295e-1,
];
const COS: [f64; 6] = [
    -1.13585365213876817300e-11,
    2.08757008419747316778e-9,
    -2.75573141792967388112e-7,
    2.48015872888517045348e-5,
    -1.38888888888730564116e-3,
    4.16666666666665929218e-2,
];

// Explicit fusion matches the ARM64 Go reference independently of Rust's optimizer.
fn polynomial(x: f64, coefficients: &[f64]) -> f64 {
    let mut value = coefficients[0];
    for &coefficient in &coefficients[1..] {
        value = value.mul_add(x, coefficient);
    }
    value
}

fn sin_series(z: f64) -> f64 {
    let zz = z * z;
    (z * zz).mul_add(polynomial(zz, &SIN), z)
}

fn cos_series(z: f64) -> f64 {
    let zz = z * z;
    (zz * zz).mul_add(polynomial(zz, &COS), (-0.5f64).mul_add(zz, 1.0))
}

pub(crate) fn sin(x: f64) -> f64 {
    if x == 0.0 || x.is_nan() {
        return x;
    }
    if x.is_infinite() {
        return f64::NAN;
    }
    let (mut octant, z) = reduce(x.abs());
    let mut negative = x < 0.0;
    if octant > 3 {
        negative = !negative;
        octant -= 4;
    }
    let result = if octant == 1 || octant == 2 {
        cos_series(z)
    } else {
        sin_series(z)
    };
    if negative { -result } else { result }
}

pub(crate) fn cos(x: f64) -> f64 {
    if !x.is_finite() {
        return f64::NAN;
    }
    let (mut octant, z) = reduce(x.abs());
    let mut negative = false;
    if octant > 3 {
        negative = !negative;
        octant -= 4;
    }
    if octant > 1 {
        negative = !negative;
    }
    let result = if octant == 1 || octant == 2 {
        sin_series(z)
    } else {
        cos_series(z)
    };
    if negative { -result } else { result }
}

pub(crate) fn tan(x: f64) -> f64 {
    const P: [f64; 3] = [
        -1.30936939181383777646e4,
        1.15351664838587416140e6,
        -1.79565251976484877988e7,
    ];
    const Q: [f64; 4] = [
        1.36812963470692954678e4,
        -1.32089234440210967447e6,
        2.50083801823357915839e7,
        -5.38695755929454629881e7,
    ];
    if x == 0.0 || x.is_nan() {
        return x;
    }
    if x.is_infinite() {
        return f64::NAN;
    }
    let (octant, z) = reduce(x.abs());
    let zz = z * z;
    let mut result = if zz > 1e-14 {
        let numerator = zz * polynomial(zz, &P);
        let denominator = z
            .mul_add(z, Q[0])
            .mul_add(zz, Q[1])
            .mul_add(zz, Q[2])
            .mul_add(zz, Q[3]);
        z.mul_add(numerator / denominator, z)
    } else {
        z
    };
    if octant & 2 == 2 {
        result = -1.0 / result;
    }
    if x < 0.0 { -result } else { result }
}

fn reduce(x: f64) -> (u64, f64) {
    if x >= (1u64 << 29) as f64 {
        return reduce_large(x);
    }
    const PI4_A: f64 = 7.85398125648498535156e-1;
    const PI4_B: f64 = 3.77489470793079817668e-8;
    const PI4_C: f64 = 2.69515142907905952645e-15;
    let mut octant = (x * (2.0 * std::f64::consts::FRAC_2_PI)) as u64;
    if octant & 1 == 1 {
        octant += 1;
    }
    let y = octant as f64;
    (
        octant & 7,
        (-y).mul_add(PI4_C, (-y).mul_add(PI4_B, (-y).mul_add(PI4_A, x))),
    )
}

// Payne-Hanek reduction retains the low phase bits even at the largest exponent.
fn reduce_large(x: f64) -> (u64, f64) {
    const PI4_DIGITS: [u64; 20] = [
        0x0000000000000001,
        0x45f306dc9c882a53,
        0xf84eafa3ea69bb81,
        0xb6c52b3278872083,
        0xfca2c757bd778ac3,
        0x6e48dc74849ba5c0,
        0x0c925dd413a32439,
        0xfc3bd63962534e7d,
        0xd1046bea5d768909,
        0xd338e04d68befc82,
        0x7323ac7306a673e9,
        0x3908bf177bf25076,
        0x3ff12fffbc0b301f,
        0xde5e2316b414da3e,
        0xda6cfd9e4f96136e,
        0x9e8c7ecd3cbfd45a,
        0xea4f758fd7cbe2f6,
        0x7a0e73ef14a525d4,
        0xd7f6bf623f1aba10,
        0xac06608df8f6d757,
    ];
    let bits = x.to_bits();
    let exponent = ((bits >> 52) & 0x7ff) as i32 - 1023 - 52;
    let mantissa = (bits & !(0x7ff << 52)) | (1 << 52);
    let digit = ((exponent + 61) / 64) as usize;
    let shift = ((exponent + 61) % 64) as u32;
    let word = |i: usize| {
        (PI4_DIGITS[i] << shift) | PI4_DIGITS[i + 1].checked_shr(64 - shift).unwrap_or(0)
    };
    let z2 = u128::from(word(digit + 2)) * u128::from(mantissa);
    let z1 = u128::from(word(digit + 1)) * u128::from(mantissa);
    let z0 = word(digit).wrapping_mul(mantissa);
    let (lo, carry) = (z1 as u64).overflowing_add((z2 >> 64) as u64);
    let hi = z0
        .wrapping_add((z1 >> 64) as u64)
        .wrapping_add(u64::from(carry));
    let mut octant = hi >> 61;
    let hi = (hi << 3) | (lo >> 61);
    let leading = hi.leading_zeros() + 1;
    let exponent = u64::from(1023 - leading);
    let fraction = (hi.checked_shl(leading).unwrap_or(0)
        | lo.checked_shr(64u32.wrapping_sub(leading)).unwrap_or(0))
        >> 12;
    let mut z = f64::from_bits(fraction | (exponent << 52));
    if octant & 1 == 1 {
        octant = (octant + 1) & 7;
        z -= 1.0;
    }
    (octant, z * FRAC_PI_4)
}
