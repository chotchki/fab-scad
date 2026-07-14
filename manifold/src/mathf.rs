//! Pillar 2 — the deterministic transcendental seam (a VERBATIM port of Manifold's `math.h`).
//!
//! Every trig call the kernel makes routes through here so results are bit-identical native == wasm
//! AND bit-identical to the C++ differential oracle. The load-bearing detail the SPEC didn't know:
//! the linked Manifold v3.5.1 ships its OWN deterministic trig (`include/manifold/math.h`, a
//! transliteration of musl/FreeBSD `msun` — the Sun `__sin`/`__cos`/`__tan`/`e_acos`/`atan`/`atan2`
//! kernels). The C++ oracle computes trig with THAT code, not platform libm. So we don't "adopt libm"
//! for trig (an independent musl port — very-likely-but-unproven bit-match); we transliterate `math.h`
//! itself. A faithful transliteration of straight-line f64 arithmetic is bit-identical across C++ and
//! Rust BY CONSTRUCTION — Rust never auto-contracts FMA, and this module never uses `mul_add`. `libm`
//! stays a dep only for `remquo` (an exact op) and as a cross-check oracle in the unit tests.
//!
//! Degree-trig (`sind`/`cosd`) does exact-quadrant SNAPPING (`sind(180)==0`, `sind(90)==1`) so the
//! kernel and fab-lang speak ONE math dialect. `sqrt`/`floor`/`ceil`/`round`/`trunc` stay hardware
//! `f64::` (IEEE-exact — routing them through a polynomial buys nothing and costs bits). See
//! `SPEC_manifold-rs.md` Pillar 2.
//!
//! Provenance (per `math.h`): FreeBSD msun via musl libc.
//! Copyright (C) 1993 by Sun Microsystems, Inc. Permission to use/copy/modify/distribute freely,
//! provided this notice is preserved.

// This file is a VERBATIM numeric transliteration — its constants ARE the algorithm's magic numbers
// (some, like PIO2_1, DELIBERATELY truncated for the compensated reductions), kept bit-for-bit as
// upstream for auditability + the oracle match. clippy's "swap for a std const / trim precision / add
// underscores / use a range / use clamp" rewrites all fight that intent (and `approx_constant` would
// silently break the truncated reductions), and `x - x` is the musl idiom for turning inf into NaN.
// Silenced HERE ONLY (this one verbatim-port module), never crate-wide.
#![allow(
    clippy::approx_constant,
    clippy::excessive_precision,
    clippy::unreadable_literal,
    clippy::manual_range_contains,
    clippy::manual_clamp,
    clippy::eq_op
)]

// ---------------------------------------------------------------------------
// Scalar constants (common.h kPi/kTwoPi/kHalfPi).
// ---------------------------------------------------------------------------

/// π, to full f64 precision (Manifold `kPi`).
pub const PI: f64 = 3.14159265358979323846264338327950288;
/// 2π (Manifold `kTwoPi`).
pub const TWO_PI: f64 = 6.28318530717958647692528676655900576;
/// π/2 (Manifold `kHalfPi`).
pub const HALF_PI: f64 = 1.57079632679489661923132169163975144;

// ---------------------------------------------------------------------------
// Bit-level helpers (math.h AsUint64/FromUint64/HighWord/LowWord/WithLowWord).
// std::memcpy type-punning maps to the safe f64::to_bits / from_bits — bit-identical, no `unsafe`.
// ---------------------------------------------------------------------------

#[inline]
fn high_word(x: f64) -> u32 {
    (x.to_bits() >> 32) as u32
}

#[inline]
fn low_word(x: f64) -> u32 {
    x.to_bits() as u32
}

#[inline]
fn with_low_word(x: f64, low: u32) -> f64 {
    let u = (x.to_bits() & 0xffff_ffff_0000_0000) | low as u64;
    f64::from_bits(u)
}

/// 2^-120, the directional-rounding nudge `math.h` writes as the float literal `0x1p-120f`
/// (Rust has no hex-float literals; this bit pattern is exactly 2^-120).
const P_120: f64 = f64::from_bits(0x3870_0000_0000_0000);

// ---------------------------------------------------------------------------
// The reduced-argument kernels (math.h SinKernel/CosKernel/TanKernel).
// ---------------------------------------------------------------------------

fn sin_kernel(x: f64, y: f64, iy: i32) -> f64 {
    const S1: f64 = -1.66666666666666324348e-01;
    const S2: f64 = 8.33333333332248946124e-03;
    const S3: f64 = -1.98412698298579493134e-04;
    const S4: f64 = 2.75573137070700676789e-06;
    const S5: f64 = -2.50507602534068634195e-08;
    const S6: f64 = 1.58969099521155010221e-10;

    let z = x * x;
    let w = z * z;
    let r = S2 + z * (S3 + z * S4) + z * w * (S5 + z * S6);
    let v = z * x;
    if iy == 0 {
        x + v * (S1 + z * r)
    } else {
        x - ((z * (0.5 * y - v * r) - y) - v * S1)
    }
}

fn cos_kernel(x: f64, y: f64) -> f64 {
    const C1: f64 = 4.16666666666666019037e-02;
    const C2: f64 = -1.38888888888741095749e-03;
    const C3: f64 = 2.48015872894767294178e-05;
    const C4: f64 = -2.75573143513906633035e-07;
    const C5: f64 = 2.08757232129817482790e-09;
    const C6: f64 = -1.13596475577881948265e-11;

    let z = x * x;
    let w = z * z;
    let r = z * (C1 + z * (C2 + z * C3)) + w * w * (C4 + z * (C5 + z * C6));
    let hz = 0.5 * z;
    let w1 = 1.0 - hz;
    w1 + (((1.0 - w1) - hz) + (z * r - x * y))
}

fn tan_kernel(mut x: f64, mut y: f64, odd: i32) -> f64 {
    const T: [f64; 13] = [
        3.33333333333334091986e-01,
        1.33333333333201242699e-01,
        5.39682539762260521377e-02,
        2.18694882948595424599e-02,
        8.86323982359930005737e-03,
        3.59207910759131235356e-03,
        1.45620945432529025516e-03,
        5.88041240820264096874e-04,
        2.46463134818469906812e-04,
        7.81794442939557092300e-05,
        7.14072491382608190305e-05,
        -1.85586374855275456654e-05,
        2.59073051863633712884e-05,
    ];
    const PIO4: f64 = 7.85398163397448278999e-01;
    const PIO4LO: f64 = 3.06161699786838301793e-17;

    let hx = high_word(x);
    let big = (hx & 0x7fffffff) >= 0x3FE59428; // |x| >= 0.6744
    let mut sign = false;
    if big {
        sign = (hx >> 31) != 0;
        if sign {
            x = -x;
            y = -y;
        }
        x = (PIO4 - x) + (PIO4LO - y);
        y = 0.0;
    }

    let z = x * x;
    let w = z * z;
    let r = T[1] + w * (T[3] + w * (T[5] + w * (T[7] + w * (T[9] + w * T[11]))));
    let v = z * (T[2] + w * (T[4] + w * (T[6] + w * (T[8] + w * (T[10] + w * T[12])))));
    let s = z * x;
    let rr = y + z * (s * (r + v) + y) + s * T[0];
    let ww = x + rr;
    if big {
        let s2 = 1.0 - 2.0 * odd as f64;
        let vv = s2 - 2.0 * (x + (rr - ww * ww / (ww + s2)));
        return if sign { -vv } else { vv };
    }
    if odd == 0 {
        return ww;
    }
    // Compute -1/(x+r) with reduced cancellation error.
    let w0 = with_low_word(ww, 0);
    let vv = rr - (w0 - x);
    let aa = -1.0 / ww;
    let a0 = with_low_word(aa, 0);
    a0 + aa * (1.0 + a0 * w0 + a0 * vv)
}

// ---------------------------------------------------------------------------
// Argument reduction (math.h RemPio2 = musl __rem_pio2). Reduces x to y[0]+y[1]
// in [-π/4, π/4] and returns the quadrant n (x = n·π/2 + y).
// ---------------------------------------------------------------------------

fn rem_pio2(x: f64, y: &mut [f64; 2]) -> i32 {
    use core::f64::consts::FRAC_PI_2;
    const TOINT: f64 = 1.5 / f64::EPSILON;
    const PIO4: f64 = f64::from_bits(0x3fe921fb54442d18); // 0x1.921fb54442d18p-1
    const INVPIO2: f64 = 6.36619772367581382433e-01;
    const PIO2_1: f64 = 1.57079632673412561417e+00;
    const PIO2_1T: f64 = 6.07710050650619224932e-11;
    const PIO2_2: f64 = 6.07710050630396597660e-11;
    const PIO2_2T: f64 = 2.02226624879595063154e-21;
    const PIO2_3: f64 = 2.02226624871116645580e-21;
    const PIO2_3T: f64 = 8.47842766036889956997e-32;

    let ux = x.to_bits();
    let sign = (ux >> 63) != 0;
    let ix = ((ux >> 32) & 0x7fffffff) as u32;

    // Fast quadrant cases; each arm either returns or `break`s to the medium reduction below.
    'quadrant: {
        if ix <= 0x400f6a7a {
            // |x| ~<= 5π/4
            if (ix & 0xfffff) == 0x921fb {
                break 'quadrant; // near a multiple of π/2 — use medium
            }
            if ix <= 0x4002d97c {
                // |x| ~<= 3π/4
                if !sign {
                    let z = x - PIO2_1;
                    y[0] = z - PIO2_1T;
                    y[1] = (z - y[0]) - PIO2_1T;
                    return 1;
                }
                let z = x + PIO2_1;
                y[0] = z + PIO2_1T;
                y[1] = (z - y[0]) + PIO2_1T;
                return -1;
            }
            if !sign {
                let z = x - 2.0 * PIO2_1;
                y[0] = z - 2.0 * PIO2_1T;
                y[1] = (z - y[0]) - 2.0 * PIO2_1T;
                return 2;
            }
            let z = x + 2.0 * PIO2_1;
            y[0] = z + 2.0 * PIO2_1T;
            y[1] = (z - y[0]) + 2.0 * PIO2_1T;
            return -2;
        }
        if ix <= 0x401c463b {
            // |x| ~<= 9π/4
            if ix <= 0x4015fdbc {
                // |x| ~<= 7π/4
                if ix == 0x4012d97c {
                    break 'quadrant;
                }
                if !sign {
                    let z = x - 3.0 * PIO2_1;
                    y[0] = z - 3.0 * PIO2_1T;
                    y[1] = (z - y[0]) - 3.0 * PIO2_1T;
                    return 3;
                }
                let z = x + 3.0 * PIO2_1;
                y[0] = z + 3.0 * PIO2_1T;
                y[1] = (z - y[0]) + 3.0 * PIO2_1T;
                return -3;
            }
            if ix == 0x401921fb {
                break 'quadrant;
            }
            if !sign {
                let z = x - 4.0 * PIO2_1;
                y[0] = z - 4.0 * PIO2_1T;
                y[1] = (z - y[0]) - 4.0 * PIO2_1T;
                return 4;
            }
            let z = x + 4.0 * PIO2_1;
            y[0] = z + 4.0 * PIO2_1T;
            y[1] = (z - y[0]) + 4.0 * PIO2_1T;
            return -4;
        }
        if ix >= 0x413921fb {
            // |x| ~>= 2^20·(π/2): huge, or non-finite.
            if ix >= 0x7ff00000 {
                y[0] = x - x;
                y[1] = x - x;
                return 0;
            }
            let (r, q) = libm::remquo(x, FRAC_PI_2);
            y[0] = r;
            y[1] = 0.0;
            return q;
        }
        // 0x401c463b < ix < 0x413921fb: fall through to medium.
    }

    // medium: |x| ~< 2^20·(π/2), reduce with a 3-step compensated subtraction.
    let mut f_n = x * INVPIO2 + TOINT - TOINT;
    let mut n = f_n as i32;
    let mut r = x - f_n * PIO2_1;
    let mut w = f_n * PIO2_1T;
    if r - w < -PIO4 {
        n -= 1;
        f_n -= 1.0;
        r = x - f_n * PIO2_1;
        w = f_n * PIO2_1T;
    } else if r - w > PIO4 {
        n += 1;
        f_n += 1.0;
        r = x - f_n * PIO2_1;
        w = f_n * PIO2_1T;
    }
    y[0] = r - w;
    let ey = ((y[0].to_bits() >> 52) & 0x7ff) as i32;
    let ex = (ix >> 20) as i32;
    if ex - ey > 16 {
        let t = r;
        w = f_n * PIO2_2;
        r = t - w;
        w = f_n * PIO2_2T - ((t - r) - w);
        y[0] = r - w;
        let ey2 = ((y[0].to_bits() >> 52) & 0x7ff) as i32;
        if ex - ey2 > 49 {
            let t2 = r;
            w = f_n * PIO2_3;
            r = t2 - w;
            w = f_n * PIO2_3T - ((t2 - r) - w);
            y[0] = r - w;
        }
    }
    y[1] = (r - y[0]) - w;
    n
}

// ---------------------------------------------------------------------------
// Public transcendentals (radians in). math.h sin/cos/tan/acos/asin/atan/atan2.
// ---------------------------------------------------------------------------

/// Sine of an angle in radians. Deterministic (musl kernels) — bit-identical to the C++ oracle.
pub fn sin(x: f64) -> f64 {
    let ix = ((x.to_bits() >> 32) & 0x7fffffff) as u32;
    if ix <= 0x3fe921fb {
        if ix < 0x3e500000 {
            return x;
        }
        return sin_kernel(x, 0.0, 0);
    }
    if ix >= 0x7ff00000 {
        return x - x;
    }
    let mut y = [0.0f64; 2];
    let n = rem_pio2(x, &mut y);
    match n & 3 {
        0 => sin_kernel(y[0], y[1], 1),
        1 => cos_kernel(y[0], y[1]),
        2 => -sin_kernel(y[0], y[1], 1),
        _ => -cos_kernel(y[0], y[1]),
    }
}

/// Cosine of an angle in radians. Deterministic (musl kernels).
pub fn cos(x: f64) -> f64 {
    let ix = ((x.to_bits() >> 32) & 0x7fffffff) as u32;
    if ix <= 0x3fe921fb {
        if ix < 0x3e46a09e {
            return 1.0;
        }
        return cos_kernel(x, 0.0);
    }
    if ix >= 0x7ff00000 {
        return x - x;
    }
    let mut y = [0.0f64; 2];
    let n = rem_pio2(x, &mut y);
    match n & 3 {
        0 => cos_kernel(y[0], y[1]),
        1 => -sin_kernel(y[0], y[1], 1),
        2 => -cos_kernel(y[0], y[1]),
        _ => sin_kernel(y[0], y[1], 1),
    }
}

/// Tangent of an angle in radians. Deterministic (musl kernels).
pub fn tan(x: f64) -> f64 {
    let ix = high_word(x) & 0x7fffffff;
    if ix <= 0x3fe921fb {
        if ix < 0x3e400000 {
            return x;
        }
        return tan_kernel(x, 0.0, 0);
    }
    if ix >= 0x7ff00000 {
        return x - x;
    }
    let mut y = [0.0f64; 2];
    let n = rem_pio2(x, &mut y);
    tan_kernel(y[0], y[1], n & 1)
}

/// Arccosine, result in radians (musl `e_acos`). NaN outside [-1, 1].
pub fn acos(x: f64) -> f64 {
    const PIO2_HI: f64 = 1.57079632679489655800e+00;
    const PIO2_LO: f64 = 6.12323399573676603587e-17;
    const PS0: f64 = 1.66666666666666657415e-01;
    const PS1: f64 = -3.25565818622400915405e-01;
    const PS2: f64 = 2.01212532134862925881e-01;
    const PS3: f64 = -4.00555345006794114027e-02;
    const PS4: f64 = 7.91534994289814532176e-04;
    const PS5: f64 = 3.47933107596021167570e-05;
    const QS1: f64 = -2.40339491173441421878e+00;
    const QS2: f64 = 2.02094576023350569471e+00;
    const QS3: f64 = -6.88283971605453293030e-01;
    const QS4: f64 = 7.70381505559019352791e-02;
    let r_poly = |z: f64| -> f64 {
        let p = z * (PS0 + z * (PS1 + z * (PS2 + z * (PS3 + z * (PS4 + z * PS5)))));
        let q = 1.0 + z * (QS1 + z * (QS2 + z * (QS3 + z * QS4)));
        p / q
    };

    let xx = x.to_bits();
    let hx = (xx >> 32) as u32;
    let ix = hx & 0x7fffffff;
    if ix >= 0x3ff00000 {
        let lx = xx as u32;
        if ((ix - 0x3ff00000) | lx) == 0 {
            if hx >> 31 != 0 {
                return 2.0 * PIO2_HI + P_120;
            }
            return 0.0;
        }
        return 0.0 / (x - x);
    }
    if ix < 0x3fe00000 {
        if ix <= 0x3c600000 {
            return PIO2_HI + P_120;
        }
        return PIO2_HI - (x - (PIO2_LO - x * r_poly(x * x)));
    }
    if hx >> 31 != 0 {
        let z = (1.0 + x) * 0.5;
        let s = z.sqrt();
        let w = r_poly(z) * s - PIO2_LO;
        return 2.0 * (PIO2_HI - (s + w));
    }
    let z = (1.0 - x) * 0.5;
    let s = z.sqrt();
    let df = f64::from_bits(s.to_bits() & 0xffff_ffff_0000_0000);
    let c = (z - df * df) / (s + df);
    let w = r_poly(z) * s + c;
    2.0 * (df + w)
}

/// Arcsine, result in radians. Defined via `π/2 − acos(x)` (matches math.h). NaN outside [-1, 1].
pub fn asin(x: f64) -> f64 {
    if !x.is_finite() || x < -1.0 || x > 1.0 {
        return f64::NAN;
    }
    if x == 1.0 {
        return HALF_PI;
    }
    if x == -1.0 {
        return -HALF_PI;
    }
    HALF_PI - acos(x)
}

/// Arctangent, result in radians (musl `atan`).
pub fn atan(mut x: f64) -> f64 {
    const ATANHI: [f64; 4] = [
        4.63647609000806093515e-01,
        7.85398163397448278999e-01,
        9.82793723247329054082e-01,
        1.57079632679489655800e+00,
    ];
    const ATANLO: [f64; 4] = [
        2.26987774529616870924e-17,
        3.06161699786838301793e-17,
        1.39033110312309984516e-17,
        6.12323399573676603587e-17,
    ];
    const AT: [f64; 11] = [
        3.33333333333329318027e-01,
        -1.99999999998764832476e-01,
        1.42857142725034663711e-01,
        -1.11111104054623557880e-01,
        9.09088713343650656196e-02,
        -7.69187620504482999495e-02,
        6.66107313738753120669e-02,
        -5.83357013379057348645e-02,
        4.97687799461593236017e-02,
        -3.65315727442169155270e-02,
        1.62858201153657823623e-02,
    ];

    let mut ix = high_word(x);
    let sign = ix >> 31;
    ix &= 0x7fffffff;
    let id: i32;

    if ix >= 0x44100000 {
        // |x| >= 2^66
        if x.is_nan() {
            return x;
        }
        let z = ATANHI[3] + P_120;
        return if sign != 0 { -z } else { z };
    }
    if ix < 0x3fdc0000 {
        // |x| < 0.4375
        if ix < 0x3e400000 {
            return x; // |x| < 2^-27
        }
        id = -1;
    } else {
        x = x.abs();
        if ix < 0x3ff30000 {
            // |x| < 1.1875
            if ix < 0x3fe60000 {
                id = 0;
                x = (2.0 * x - 1.0) / (2.0 + x);
            } else {
                id = 1;
                x = (x - 1.0) / (x + 1.0);
            }
        } else if ix < 0x40038000 {
            // |x| < 2.4375
            id = 2;
            x = (x - 1.5) / (1.0 + 1.5 * x);
        } else {
            id = 3;
            x = -1.0 / x;
        }
    }

    let z = x * x;
    let w = z * z;
    let s1 = z * (AT[0] + w * (AT[2] + w * (AT[4] + w * (AT[6] + w * (AT[8] + w * AT[10])))));
    let s2 = w * (AT[1] + w * (AT[3] + w * (AT[5] + w * (AT[7] + w * AT[9]))));
    if id < 0 {
        return x - x * (s1 + s2);
    }
    let idx = id as usize;
    let zz = ATANHI[idx] - (x * (s1 + s2) - ATANLO[idx] - x);
    if sign != 0 { -zz } else { zz }
}

/// Two-argument arctangent, result in radians (musl `atan2`).
pub fn atan2(y: f64, x: f64) -> f64 {
    const PI: f64 = 3.1415926535897931160E+00;
    const PI_LO: f64 = 1.2246467991473531772E-16;

    if x.is_nan() || y.is_nan() {
        return x + y;
    }
    let mut ix = high_word(x);
    let mut iy = high_word(y);
    let lx = low_word(x);
    let ly = low_word(y);

    if (ix.wrapping_sub(0x3ff00000) | lx) == 0 {
        return atan(y); // x == 1.0 (C relies on unsigned wraparound here)
    }

    let m = ((iy >> 31) & 1) | ((ix >> 30) & 2);
    ix &= 0x7fffffff;
    iy &= 0x7fffffff;

    if (iy | ly) == 0 {
        // y == 0
        return match m {
            0 | 1 => y,
            2 => PI,
            _ => -PI,
        };
    }
    if (ix | lx) == 0 {
        // x == 0
        return if m & 1 != 0 { -PI / 2.0 } else { PI / 2.0 };
    }
    if ix == 0x7ff00000 {
        // x is INF
        if iy == 0x7ff00000 {
            return match m {
                0 => PI / 4.0,
                1 => -PI / 4.0,
                2 => 3.0 * PI / 4.0,
                _ => -3.0 * PI / 4.0,
            };
        }
        return match m {
            0 => 0.0,
            1 => -0.0,
            2 => PI,
            _ => -PI,
        };
    }
    if ix.wrapping_add(64 << 20) < iy || iy == 0x7ff00000 {
        return if m & 1 != 0 { -PI / 2.0 } else { PI / 2.0 }; // |y/x| > 2^64
    }

    let z = if (m & 2) != 0 && iy.wrapping_add(64 << 20) < ix {
        0.0 // |y/x| < 2^-64 and x < 0
    } else {
        atan((y / x).abs())
    };

    match m {
        0 => z,
        1 => -z,
        2 => PI - (z - PI_LO),
        _ => (z - PI_LO) - PI,
    }
}

// ---------------------------------------------------------------------------
// Degree helpers with exact-quadrant snapping (common.h radians/degrees/sind/cosd/smoothstep).
// ---------------------------------------------------------------------------

/// Degrees → radians.
#[inline]
pub fn radians(a: f64) -> f64 {
    a * PI / 180.0
}

/// Radians → degrees.
#[inline]
pub fn degrees(a: f64) -> f64 {
    a * 180.0 / PI
}

/// Sine of an angle in DEGREES, with multiples of 90° exact (`sind(180)==0`, `sind(90)==1`).
/// `remquo(|x|, 90)` splits x into a quadrant + a residual in [0, 90), so the exact cases never
/// reach the polynomial. `remquo` is an exact IEEE op — `libm::remquo` is deterministic and matches
/// the C++ `std::remquo`.
pub fn sind(x: f64) -> f64 {
    if !x.is_finite() {
        return f64::NAN;
    }
    if x < 0.0 {
        return -sind(-x);
    }
    let (rem, quo) = libm::remquo(x.abs(), 90.0);
    let xr = radians(rem);
    // `rem_euclid(4)` ∈ 0..=3, so `_` IS the quadrant-3 case (folding it in avoids a dead arm).
    match quo.rem_euclid(4) {
        0 => sin(xr),
        1 => cos(xr),
        2 => -sin(xr),
        _ => -cos(xr),
    }
}

/// Cosine of an angle in DEGREES, with multiples of 90° exact (`cosd(90)==0`, `cosd(0)==1`).
#[inline]
pub fn cosd(x: f64) -> f64 {
    sind(x + 90.0)
}

/// Smooth Hermite interpolation between 0 and 1 for edge0 < a < edge1 (common.h `smoothstep`).
pub fn smoothstep(edge0: f64, edge1: f64, a: f64) -> f64 {
    let x = ((a - edge0) / (edge1 - edge0)).max(0.0).min(1.0);
    x * x * (3.0 - 2.0 * x)
}

#[cfg(test)]
mod tests {
    use super::*;

    // The exact-quadrant snapping is the whole POINT of sind/cosd — these must be bit-exact.
    #[test]
    fn degree_trig_snaps_multiples_of_90() {
        assert_eq!(sind(0.0), 0.0);
        assert_eq!(sind(90.0), 1.0);
        assert_eq!(sind(180.0), 0.0);
        assert_eq!(sind(270.0), -1.0);
        assert_eq!(sind(360.0), 0.0);
        assert_eq!(sind(450.0), 1.0);
        assert_eq!(sind(-90.0), -1.0);

        assert_eq!(cosd(0.0), 1.0);
        assert_eq!(cosd(90.0), 0.0);
        assert_eq!(cosd(180.0), -1.0);
        assert_eq!(cosd(270.0), 0.0);
        assert_eq!(cosd(360.0), 1.0);
    }

    #[test]
    fn zero_and_identity_points() {
        assert_eq!(sin(0.0), 0.0);
        assert_eq!(cos(0.0), 1.0);
        assert_eq!(tan(0.0), 0.0);
        assert_eq!(atan(0.0), 0.0);
        assert_eq!(acos(1.0), 0.0);
        assert_eq!(asin(0.0), 0.0);
        assert_eq!(atan2(0.0, 1.0), 0.0);
    }

    #[test]
    fn asin_acos_domain_edges() {
        assert_eq!(asin(1.0), HALF_PI);
        assert_eq!(asin(-1.0), -HALF_PI);
        assert!(asin(1.5).is_nan());
        assert!(asin(-1.5).is_nan());
        assert_eq!(acos(1.0), 0.0);
        assert!(acos(2.0).is_nan());
    }

    /// Cross-check against the INDEPENDENT musl port (the `libm` crate) across a wide sweep.
    /// Both descend from FreeBSD msun; agreement here is strong evidence the transliteration is
    /// faithful. The AUTHORITATIVE gate (bit-match vs the C++ Manifold oracle) is K.0 (M.0.6) —
    /// if this ever splits from libm, that's exactly the divergence the SPEC predicted, and the
    /// oracle decides who's right.
    // `ours == theirs` bitwise, with any-NaN ≡ any-NaN (the tier_eq rule). Asserted INLINE (fail-fast)
    // rather than counted, so there's no unexecuted "increment on mismatch" branch to leave uncovered.
    fn same_bits(a: f64, b: f64) -> bool {
        a.to_bits() == b.to_bits() || (a.is_nan() && b.is_nan())
    }

    #[test]
    fn matches_libm_bitwise_over_sweep() {
        let mut i = -100_000i64;
        while i <= 100_000 {
            let x = i as f64 * 1e-4; // sweep [-10, 10] step 1e-4
            for (ours, theirs, name) in [
                (sin(x), libm::sin(x), "sin"),
                (cos(x), libm::cos(x), "cos"),
                (tan(x), libm::tan(x), "tan"),
                (atan(x), libm::atan(x), "atan"),
            ] {
                assert!(
                    same_bits(ours, theirs),
                    "{name}({x}): ours={ours:?} libm={theirs:?}"
                );
            }
            i += 1;
        }
    }

    #[test]
    fn matches_libm_inverse_and_atan2() {
        let mut i = -10_000i64;
        while i <= 10_000 {
            let x = i as f64 * 1e-4; // [-1, 1] for asin/acos
            // acos: musl e_acos — must match the libm crate's port bitwise.
            assert!(same_bits(acos(x), libm::acos(x)), "acos({x})");
            // asin: Manifold routes it through `HALF_PI - acos` (math.h), so it does NOT match libm's
            // DIRECT asin — the correct independent reference is `HALF_PI - libm::acos`.
            assert!(same_bits(asin(x), HALF_PI - libm::acos(x)), "asin({x})");
            i += 1;
        }
        // atan2 over a grid of quadrants (incl. axes) — musl atan2, must match libm bitwise.
        for yi in -50..=50 {
            for xi in -50..=50 {
                let (y, x) = (yi as f64 * 0.1, xi as f64 * 0.1);
                assert!(same_bits(atan2(y, x), libm::atan2(y, x)), "atan2({y},{x})");
            }
        }
    }

    #[test]
    fn covers_reduction_and_special_paths() {
        let bits_eq = same_bits;

        // Quadrant fast-paths (±1/±2/±3/±4, both signs), the EXACT-π/2-multiple `break`-to-medium
        // cases (π/2, π, 3π/2, 2π), the medium 3-step reduction with its ±1 correction, and its DEEP
        // 3rd refinement step (2915.397982531328 is within 2^-54 of 1856·π/2). Up to ~1.6e6 our
        // reduction is musl's, so it matches the libm crate BITWISE.
        for &x in &[
            1.7_f64,
            -1.7, // ±1
            2.5,
            -2.5, // ±2
            5.5,
            -5.5, // ±3
            7.0,
            -7.0, // ±4
            HALF_PI,
            PI,
            3.0 * HALF_PI,
            TWO_PI, // exact-multiple breaks (193/224/238)
            8.0,
            9.0,
            11.0,
            14.0,
            16.0,
            50.0,               // medium range
            8.63937979737193,   // forces the n−1 quadrant correction (272-275)
            13.351768777756622, // forces the n+1 quadrant correction (277-280)
            100.0,
            -100.0,
            1000.0,
            123456.0,
            1_000_000.0,
            1_600_000.0,
            2915.397982531328, // forces the 3rd reduction step
        ] {
            assert!(bits_eq(sin(x), libm::sin(x)), "sin({x})");
            assert!(bits_eq(cos(x), libm::cos(x)), "cos({x})");
            assert!(bits_eq(tan(x), libm::tan(x)), "tan({x})");
        }

        // rem_pio2's inf/NaN arm is UNREACHABLE through sin/cos/tan (they pre-check `ix>=0x7ff00000`),
        // so cover it by calling the reducer directly — it must return 0 with NaN residuals.
        let mut yb = [0.0_f64; 2];
        assert_eq!(rem_pio2(f64::INFINITY, &mut yb), 0);
        assert!(yb[0].is_nan() && yb[1].is_nan());

        // Huge args (|x| >= 2^20·π/2): our reduction is `remquo` — MATCHING Manifold's math.h (the C++
        // oracle), which deliberately differs from libm's payne-hanek there, so we only sanity-check.
        for &x in &[1e7_f64, -1e7, 1e15, 1e20] {
            assert!(sin(x).is_finite() && sin(x).abs() <= 1.0, "sin huge {x}");
            assert!(cos(x).is_finite() && cos(x).abs() <= 1.0, "cos huge {x}");
            assert!(tan(x).is_finite(), "tan huge {x}");
        }

        // Non-finite inputs → NaN (the `ix >= 0x7ff00000` `x - x` paths).
        for &nf in &[f64::INFINITY, f64::NEG_INFINITY, f64::NAN] {
            assert!(sin(nf).is_nan());
            assert!(cos(nf).is_nan());
            assert!(tan(nf).is_nan());
        }

        // atan huge branch (|x| >= 2^66) + NaN.
        assert!(bits_eq(atan(1e20), libm::atan(1e20)));
        assert!(bits_eq(atan(-1e20), libm::atan(-1e20)));
        assert!(atan(f64::NAN).is_nan());

        // acos boundaries + interior + out-of-domain → NaN.
        for &x in &[-1.0_f64, 1.0, 0.7, -0.3, 0.999_999] {
            assert!(bits_eq(acos(x), libm::acos(x)), "acos({x})");
        }
        assert!(acos(2.0).is_nan());
        assert!(acos(-2.0).is_nan());

        // atan2 across every quadrant, axis, and infinity combination.
        let vals = [0.0_f64, -0.0, 1.0, -1.0, f64::INFINITY, f64::NEG_INFINITY];
        for &y in &vals {
            for &x in &vals {
                assert!(bits_eq(atan2(y, x), libm::atan2(y, x)), "atan2({y},{x})");
            }
        }
        assert!(atan2(f64::NAN, 1.0).is_nan());
        assert!(atan2(1.0, f64::NAN).is_nan());
        // |y/x| < 2^-64 with x < 0 → the underflow-to-±π short-circuit (z = 0 branch).
        assert!(bits_eq(atan2(1.0, -1e30), libm::atan2(1.0, -1e30)));
        assert!(bits_eq(atan2(-1.0, -1e30), libm::atan2(-1.0, -1e30)));
        // |y/x| > 2^64 → ±π/2.
        assert!(bits_eq(atan2(1e30, 1.0), libm::atan2(1e30, 1.0)));

        // degrees is radians' inverse; smoothstep clamps + interpolates.
        assert_eq!(degrees(0.0), 0.0);
        assert!((degrees(PI) - 180.0).abs() < 1e-12);
        assert!((radians(degrees(1.234)) - 1.234).abs() < 1e-12);
        assert_eq!(smoothstep(0.0, 1.0, 0.5), 0.5);
        assert_eq!(smoothstep(0.0, 1.0, -3.0), 0.0); // clamp below
        assert_eq!(smoothstep(0.0, 1.0, 4.0), 1.0); // clamp above
        assert!(smoothstep(2.0, 6.0, 3.0) > 0.0 && smoothstep(2.0, 6.0, 3.0) < 0.5);

        // sind/cosd: all four quadrant arms (30°→arm0, 100°→1, 200°→2, 250°→3 = the `_` arm),
        // negatives, non-finite. Compared to sin-of-the-angle (robust to which internal arm runs).
        assert!(bits_eq(cosd(45.0), sind(135.0)));
        assert_eq!(sind(-30.0), -sind(30.0));
        for &deg in &[30.0_f64, 100.0, 200.0, 250.0, 350.0] {
            assert!(
                (sind(deg) - libm::sin(radians(deg))).abs() < 1e-12,
                "sind({deg})"
            );
        }
        assert!(sind(f64::INFINITY).is_nan());
        assert!(cosd(f64::NAN).is_nan());
    }
}
