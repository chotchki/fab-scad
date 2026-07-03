//! Tiny `[f64; 3]` vector helpers for the orientation geometry (#40) + auto-orient (#42). The lib
//! has no glam dependency, and this math is small + pure, so it lives here and is unit-tested.

pub type V3 = [f64; 3];

pub fn dot(a: V3, b: V3) -> f64 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

pub fn add(a: V3, b: V3) -> V3 {
    [a[0] + b[0], a[1] + b[1], a[2] + b[2]]
}

pub fn sub(a: V3, b: V3) -> V3 {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}

pub fn scale(a: V3, s: f64) -> V3 {
    [a[0] * s, a[1] * s, a[2] * s]
}

pub fn cross(a: V3, b: V3) -> V3 {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}

pub fn norm(a: V3) -> f64 {
    dot(a, a).sqrt()
}

/// Unit vector; returns the input unchanged if it's ~zero (avoid NaN).
pub fn normalize(a: V3) -> V3 {
    let n = norm(a);
    if n < 1e-12 {
        a
    } else {
        scale(a, 1.0 / n)
    }
}

/// Angle between `a` and `b`, in DEGREES (0..=180).
pub fn angle_deg(a: V3, b: V3) -> f64 {
    dot(normalize(a), normalize(b))
        .clamp(-1.0, 1.0)
        .acos()
        .to_degrees()
}
