// Copyright 2026 Marissa Tanner, Heronetic
// SPDX-License-Identifier: Apache-2.0

//! 4x4 matrices for CSS transforms (CSS Transforms 1 and 2).
//!
//! Matrices are row-major and act on column vectors: `p' = M · p` with
//! `p = (x, y, z, 1)`. A 2D CSS `matrix(a, b, c, d, e, f)` is
//! `[[a, c, 0, e], [b, d, 0, f], [0, 0, 1, 0], [0, 0, 0, 1]]`.

use super::style::TransformOp;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Mat4(pub [[f64; 4]; 4]);

impl Default for Mat4 {
    fn default() -> Self {
        Mat4::IDENTITY
    }
}

impl Mat4 {
    pub const IDENTITY: Mat4 = Mat4([[1.0, 0.0, 0.0, 0.0], [0.0, 1.0, 0.0, 0.0], [0.0, 0.0, 1.0, 0.0], [0.0, 0.0, 0.0, 1.0]]);

    pub fn translate(x: f64, y: f64, z: f64) -> Mat4 {
        let mut m = Mat4::IDENTITY;
        m.0[0][3] = x;
        m.0[1][3] = y;
        m.0[2][3] = z;
        m
    }

    pub fn scale(x: f64, y: f64, z: f64) -> Mat4 {
        let mut m = Mat4::IDENTITY;
        m.0[0][0] = x;
        m.0[1][1] = y;
        m.0[2][2] = z;
        m
    }

    /// `rotate3d(x, y, z, angle)`: rotation about a direction vector.
    pub fn rotate(x: f64, y: f64, z: f64, deg: f64) -> Mat4 {
        let len = (x * x + y * y + z * z).sqrt();
        if len < 1e-12 || !len.is_finite() {
            return Mat4::IDENTITY;
        }
        let (x, y, z) = (x / len, y / len, z / len);
        let a = deg.to_radians();
        let sc = (a / 2.0).sin() * (a / 2.0).cos();
        let sq = (a / 2.0).sin().powi(2);
        Mat4([
            [1.0 - 2.0 * (y * y + z * z) * sq, 2.0 * (x * y * sq - z * sc), 2.0 * (x * z * sq + y * sc), 0.0],
            [2.0 * (x * y * sq + z * sc), 1.0 - 2.0 * (x * x + z * z) * sq, 2.0 * (y * z * sq - x * sc), 0.0],
            [2.0 * (x * z * sq - y * sc), 2.0 * (y * z * sq + x * sc), 1.0 - 2.0 * (x * x + y * y) * sq, 0.0],
            [0.0, 0.0, 0.0, 1.0],
        ])
    }

    pub fn skew(ax_deg: f64, ay_deg: f64) -> Mat4 {
        let mut m = Mat4::IDENTITY;
        m.0[0][1] = ax_deg.to_radians().tan();
        m.0[1][0] = ay_deg.to_radians().tan();
        m
    }

    /// `perspective(d)`; lengths below 1px clamp to 1px (CSS Transforms 2).
    pub fn perspective(d: f64) -> Mat4 {
        let mut m = Mat4::IDENTITY;
        m.0[3][2] = -1.0 / d.max(1.0);
        m
    }

    /// From a 2D matrix `[a, b, c, d, e, f]`.
    pub fn from_2d(v: [f64; 6]) -> Mat4 {
        Mat4([[v[0], v[2], 0.0, v[4]], [v[1], v[3], 0.0, v[5]], [0.0, 0.0, 1.0, 0.0], [0.0, 0.0, 0.0, 1.0]])
    }

    /// From the 16 values of `matrix3d()` (column-major).
    pub fn from_css3d(v: &[f32; 16]) -> Mat4 {
        let mut m = Mat4::IDENTITY;
        for c in 0..4 {
            for r in 0..4 {
                m.0[r][c] = v[c * 4 + r] as f64;
            }
        }
        m
    }

    /// The 16 values in `matrix3d()` (column-major) order.
    pub fn to_css3d(&self) -> [f64; 16] {
        let mut v = [0.0; 16];
        for c in 0..4 {
            for r in 0..4 {
                v[c * 4 + r] = self.0[r][c];
            }
        }
        v
    }

    /// `self · o`
    pub fn mul(&self, o: &Mat4) -> Mat4 {
        let mut r = [[0.0; 4]; 4];
        for (i, row) in r.iter_mut().enumerate() {
            for (j, v) in row.iter_mut().enumerate() {
                *v = (0..4).map(|k| self.0[i][k] * o.0[k][j]).sum();
            }
        }
        Mat4(r)
    }

    /// Transform a point; returns homogeneous (x, y, z, w).
    pub fn apply(&self, x: f64, y: f64, z: f64) -> [f64; 4] {
        let m = &self.0;
        [
            m[0][0] * x + m[0][1] * y + m[0][2] * z + m[0][3],
            m[1][0] * x + m[1][1] * y + m[1][2] * z + m[1][3],
            m[2][0] * x + m[2][1] * y + m[2][2] * z + m[2][3],
            m[3][0] * x + m[3][1] * y + m[3][2] * z + m[3][3],
        ]
    }

    /// Map a point of the z=0 plane to 2D (None behind the viewer).
    pub fn project(&self, x: f64, y: f64) -> Option<(f64, f64)> {
        let p = self.apply(x, y, 0.0);
        (p[3] > 1e-9).then(|| (p[0] / p[3], p[1] / p[3]))
    }

    /// True when the matrix is a 2D affine transform (Blink's `IsAffine`).
    pub fn is_2d(&self) -> bool {
        let m = &self.0;
        let z = |v: f64| v.abs() < 1e-9;
        z(m[0][2]) && z(m[1][2]) && z(m[2][0]) && z(m[2][1]) && z(m[2][3]) && z(m[3][0]) && z(m[3][1]) && z(m[3][2]) && (m[2][2] - 1.0).abs() < 1e-9 && (m[3][3] - 1.0).abs() < 1e-9
    }

    /// The 2D part `[a, b, c, d, e, f]` (meaningful when `is_2d`).
    pub fn to_2d(&self) -> [f64; 6] {
        let m = &self.0;
        [m[0][0], m[1][0], m[0][1], m[1][1], m[0][3], m[1][3]]
    }

    /// The projective map of the z=0 plane: a row-major 3x3 matrix taking
    /// (x, y, 1) to homogeneous 2D coordinates (flattening).
    pub fn homography(&self) -> [f64; 9] {
        let m = &self.0;
        [m[0][0], m[0][1], m[0][3], m[1][0], m[1][1], m[1][3], m[3][0], m[3][1], m[3][3]]
    }

    /// The flattened transform as a 2D affine matrix `[a, b, c, d, e, f]`
    /// when it has no perspective component on the z=0 plane.
    pub fn flat_affine(&self) -> Option<[f64; 6]> {
        let m = &self.0;
        let w = m[3][3];
        if m[3][0].abs() > 1e-9 * w.abs().max(1.0) || m[3][1].abs() > 1e-9 * w.abs().max(1.0) || w.abs() < 1e-12 {
            return None;
        }
        Some([m[0][0] / w, m[1][0] / w, m[0][1] / w, m[1][1] / w, m[0][3] / w, m[1][3] / w])
    }

    pub fn inverse(&self) -> Option<Mat4> {
        let m: Vec<f64> = self.0.iter().flatten().copied().collect();
        let mut inv = [0.0f64; 16];
        inv[0] = m[5] * m[10] * m[15] - m[5] * m[11] * m[14] - m[9] * m[6] * m[15] + m[9] * m[7] * m[14] + m[13] * m[6] * m[11] - m[13] * m[7] * m[10];
        inv[4] = -m[4] * m[10] * m[15] + m[4] * m[11] * m[14] + m[8] * m[6] * m[15] - m[8] * m[7] * m[14] - m[12] * m[6] * m[11] + m[12] * m[7] * m[10];
        inv[8] = m[4] * m[9] * m[15] - m[4] * m[11] * m[13] - m[8] * m[5] * m[15] + m[8] * m[7] * m[13] + m[12] * m[5] * m[11] - m[12] * m[7] * m[9];
        inv[12] = -m[4] * m[9] * m[14] + m[4] * m[10] * m[13] + m[8] * m[5] * m[14] - m[8] * m[6] * m[13] - m[12] * m[5] * m[10] + m[12] * m[6] * m[9];
        inv[1] = -m[1] * m[10] * m[15] + m[1] * m[11] * m[14] + m[9] * m[2] * m[15] - m[9] * m[3] * m[14] - m[13] * m[2] * m[11] + m[13] * m[3] * m[10];
        inv[5] = m[0] * m[10] * m[15] - m[0] * m[11] * m[14] - m[8] * m[2] * m[15] + m[8] * m[3] * m[14] + m[12] * m[2] * m[11] - m[12] * m[3] * m[10];
        inv[9] = -m[0] * m[9] * m[15] + m[0] * m[11] * m[13] + m[8] * m[1] * m[15] - m[8] * m[3] * m[13] - m[12] * m[1] * m[11] + m[12] * m[3] * m[9];
        inv[13] = m[0] * m[9] * m[14] - m[0] * m[10] * m[13] - m[8] * m[1] * m[14] + m[8] * m[2] * m[13] + m[12] * m[1] * m[10] - m[12] * m[2] * m[9];
        inv[2] = m[1] * m[6] * m[15] - m[1] * m[7] * m[14] - m[5] * m[2] * m[15] + m[5] * m[3] * m[14] + m[13] * m[2] * m[7] - m[13] * m[3] * m[6];
        inv[6] = -m[0] * m[6] * m[15] + m[0] * m[7] * m[14] + m[4] * m[2] * m[15] - m[4] * m[3] * m[14] - m[12] * m[2] * m[7] + m[12] * m[3] * m[6];
        inv[10] = m[0] * m[5] * m[15] - m[0] * m[7] * m[13] - m[4] * m[1] * m[15] + m[4] * m[3] * m[13] + m[12] * m[1] * m[7] - m[12] * m[3] * m[5];
        inv[14] = -m[0] * m[5] * m[14] + m[0] * m[6] * m[13] + m[4] * m[1] * m[14] - m[4] * m[2] * m[13] - m[12] * m[1] * m[6] + m[12] * m[2] * m[5];
        inv[3] = -m[1] * m[6] * m[11] + m[1] * m[7] * m[10] + m[5] * m[2] * m[11] - m[5] * m[3] * m[10] - m[9] * m[2] * m[7] + m[9] * m[3] * m[6];
        inv[7] = m[0] * m[6] * m[11] - m[0] * m[7] * m[10] - m[4] * m[2] * m[11] + m[4] * m[3] * m[10] + m[8] * m[2] * m[7] - m[8] * m[3] * m[6];
        inv[11] = -m[0] * m[5] * m[11] + m[0] * m[7] * m[9] + m[4] * m[1] * m[11] - m[4] * m[3] * m[9] - m[8] * m[1] * m[7] + m[8] * m[3] * m[5];
        inv[15] = m[0] * m[5] * m[10] - m[0] * m[6] * m[9] - m[4] * m[1] * m[10] + m[4] * m[2] * m[9] + m[8] * m[1] * m[6] - m[8] * m[2] * m[5];
        let det = m[0] * inv[0] + m[1] * inv[4] + m[2] * inv[8] + m[3] * inv[12];
        if det.abs() < 1e-15 || !det.is_finite() {
            return None;
        }
        let mut r = [[0.0; 4]; 4];
        for i in 0..16 {
            r[i / 4][i % 4] = inv[i] / det;
        }
        Some(Mat4(r))
    }

    /// Whether the back of a plane with this accumulated transform faces the
    /// viewer (css-transforms-2 §backface-visibility).
    pub fn is_backface(&self) -> bool {
        match self.inverse() {
            Some(i) => i.0[2][2] < 0.0,
            // A singular matrix: decide by the orientation of the flattened image.
            None => {
                let h = self.homography();
                let det = h[0] * (h[4] * h[8] - h[5] * h[7]) - h[1] * (h[3] * h[8] - h[5] * h[6]) + h[2] * (h[3] * h[7] - h[4] * h[6]);
                det < 0.0 && (self.0[2][2] < 0.0)
            }
        }
    }

    pub fn is_finite(&self) -> bool {
        self.0.iter().flatten().all(|v| v.is_finite())
    }

    pub fn is_identity(&self) -> bool {
        self.0.iter().flatten().zip(Mat4::IDENTITY.0.iter().flatten()).all(|(a, b)| (a - b).abs() < 1e-9)
    }
}

/// The matrix of one transform function for a reference box of size w×h.
pub fn op_matrix(op: &TransformOp, w: f32, h: f32) -> Mat4 {
    match op {
        TransformOp::Translate(x, y) => Mat4::translate(x.resolve(w) as f64, y.resolve(h) as f64, 0.0),
        TransformOp::Translate3d(x, y, z) => Mat4::translate(x.resolve(w) as f64, y.resolve(h) as f64, *z as f64),
        TransformOp::Scale(x, y) => Mat4::scale(*x as f64, *y as f64, 1.0),
        TransformOp::Scale3d(x, y, z) => Mat4::scale(*x as f64, *y as f64, *z as f64),
        TransformOp::Rotate(d) => Mat4::rotate(0.0, 0.0, 1.0, *d as f64),
        TransformOp::Rotate3d(x, y, z, d) => Mat4::rotate(*x as f64, *y as f64, *z as f64, *d as f64),
        TransformOp::Skew(a, b) => Mat4::skew(*a as f64, *b as f64),
        TransformOp::Matrix(v) => Mat4::from_2d(v.map(|x| x as f64)),
        TransformOp::Matrix3d(v) => Mat4::from_css3d(v),
        TransformOp::Perspective(d) => match d {
            Some(d) => Mat4::perspective(*d as f64),
            None => Mat4::IDENTITY,
        },
    }
}

/// The matrix of a transform list (without transform-origin).
pub fn ops_matrix(ops: &[TransformOp], w: f32, h: f32) -> Mat4 {
    ops.iter().fold(Mat4::IDENTITY, |m, op| m.mul(&op_matrix(op, w, h)))
}

/// The full transform of a box whose border box is at (x, y) with size w×h:
/// `translate(origin) · ops · translate(-origin)`.
pub fn box_matrix(ops: &[TransformOp], x: f32, y: f32, w: f32, h: f32, origin: (f32, f32, f32)) -> Mat4 {
    let (ox, oy, oz) = ((x + origin.0) as f64, (y + origin.1) as f64, origin.2 as f64);
    Mat4::translate(ox, oy, oz).mul(&ops_matrix(ops, w, h)).mul(&Mat4::translate(-ox, -oy, -oz))
}

/// The perspective matrix an element with `perspective: d` applies to its
/// children, with the vanishing point at (px, py).
pub fn perspective_matrix(d: f32, px: f32, py: f32) -> Mat4 {
    Mat4::translate(px as f64, py as f64, 0.0).mul(&Mat4::perspective(d as f64)).mul(&Mat4::translate(-px as f64, -py as f64, 0.0))
}

/// Multiply two row-major 3x3 matrices.
pub fn mul3(a: &[f64; 9], b: &[f64; 9]) -> [f64; 9] {
    let mut r = [0.0; 9];
    for i in 0..3 {
        for j in 0..3 {
            r[i * 3 + j] = (0..3).map(|k| a[i * 3 + k] * b[k * 3 + j]).sum();
        }
    }
    r
}

/// Invert a row-major 3x3 matrix.
pub fn inv3(m: &[f64; 9]) -> Option<[f64; 9]> {
    let c0 = m[4] * m[8] - m[5] * m[7];
    let c1 = m[5] * m[6] - m[3] * m[8];
    let c2 = m[3] * m[7] - m[4] * m[6];
    let det = m[0] * c0 + m[1] * c1 + m[2] * c2;
    if det.abs() < 1e-15 || !det.is_finite() {
        return None;
    }
    let d = 1.0 / det;
    Some([
        c0 * d,
        (m[2] * m[7] - m[1] * m[8]) * d,
        (m[1] * m[5] - m[2] * m[4]) * d,
        c1 * d,
        (m[0] * m[8] - m[2] * m[6]) * d,
        (m[2] * m[3] - m[0] * m[5]) * d,
        c2 * d,
        (m[1] * m[6] - m[0] * m[7]) * d,
        (m[0] * m[4] - m[1] * m[3]) * d,
    ])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rotate_z_matches_2d() {
        let m = Mat4::rotate(0.0, 0.0, 1.0, 30.0);
        let a = m.to_2d();
        let r = 30f64.to_radians();
        assert!((a[0] - r.cos()).abs() < 1e-12 && (a[1] - r.sin()).abs() < 1e-12 && (a[2] + r.sin()).abs() < 1e-12);
    }

    #[test]
    fn inverse_roundtrip() {
        let m = Mat4::rotate(1.0, 2.0, 3.0, 40.0).mul(&Mat4::perspective(300.0)).mul(&Mat4::translate(5.0, -7.0, 11.0));
        let p = m.mul(&m.inverse().unwrap());
        assert!(p.is_identity());
    }

    #[test]
    fn backface() {
        assert!(Mat4::rotate(0.0, 1.0, 0.0, 180.0).is_backface());
        assert!(!Mat4::scale(-1.0, 1.0, 1.0).is_backface());
        assert!(!Mat4::rotate(0.0, 1.0, 0.0, 30.0).is_backface());
    }
}
