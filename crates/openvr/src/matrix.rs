//! 3×4 affine transforms in OpenVR's convention (`HmdMatrix34_t`: row-major, rotation in the
//! left 3×3 block, translation in column 3; right-handed, +Y up, −Z forward).
//!
//! An overlay's visible face points along its local +Z axis, so an overlay placed in front of the
//! HMD at `(0, 0, -d)` with the identity rotation faces the eye.

use crate::ffi::HmdMatrix34;

/// Row-major 3×4 affine transform, layout-compatible with `HmdMatrix34_t`.
pub type Mat34 = HmdMatrix34;

/// A 3-vector helper used by [`Mat34`] constructors.
pub type Vec3 = [f32; 3];

impl Mat34 {
    pub const IDENTITY: Self = Self {
        m: [
            [1.0, 0.0, 0.0, 0.0],
            [0.0, 1.0, 0.0, 0.0],
            [0.0, 0.0, 1.0, 0.0],
        ],
    };

    /// Pure translation.
    pub fn translation(x: f32, y: f32, z: f32) -> Self {
        Self {
            m: [[1.0, 0.0, 0.0, x], [0.0, 1.0, 0.0, y], [0.0, 0.0, 1.0, z]],
        }
    }

    /// Rotation about +X by `rad` (right-handed).
    pub fn rotation_x(rad: f32) -> Self {
        let (s, c) = rad.sin_cos();
        Self {
            m: [[1.0, 0.0, 0.0, 0.0], [0.0, c, -s, 0.0], [0.0, s, c, 0.0]],
        }
    }

    /// Rotation about +Y by `rad` (right-handed).
    pub fn rotation_y(rad: f32) -> Self {
        let (s, c) = rad.sin_cos();
        Self {
            m: [[c, 0.0, s, 0.0], [0.0, 1.0, 0.0, 0.0], [-s, 0.0, c, 0.0]],
        }
    }

    /// Rotation about +Z by `rad` (right-handed).
    pub fn rotation_z(rad: f32) -> Self {
        let (s, c) = rad.sin_cos();
        Self {
            m: [[c, -s, 0.0, 0.0], [s, c, 0.0, 0.0], [0.0, 0.0, 1.0, 0.0]],
        }
    }

    /// Builds a transform from basis vectors (the columns of the rotation block) and an origin.
    pub fn from_basis(x: Vec3, y: Vec3, z: Vec3, origin: Vec3) -> Self {
        Self {
            m: [
                [x[0], y[0], z[0], origin[0]],
                [x[1], y[1], z[1], origin[1]],
                [x[2], y[2], z[2], origin[2]],
            ],
        }
    }

    /// A transform at `position` whose +Z axis (the overlay's visible face) points at `target`,
    /// keeping +Y as close to `up` as possible. Returns a translation-only transform if
    /// `position == target` or `up` is parallel to the viewing direction.
    pub fn facing(position: Vec3, target: Vec3, up: Vec3) -> Self {
        let z = normalize(sub(target, position));
        let x = normalize(cross(up, z));
        if z == [0.0; 3] || x == [0.0; 3] {
            return Self::translation(position[0], position[1], position[2]);
        }
        let y = cross(z, x);
        Self::from_basis(x, y, z, position)
    }

    /// `self * rhs`: applies `rhs` first, then `self`.
    pub fn mul(&self, rhs: &Self) -> Self {
        let a = &self.m;
        let b = &rhs.m;
        let mut out = [[0.0f32; 4]; 3];
        for (r, row) in out.iter_mut().enumerate() {
            for (c, cell) in row.iter_mut().enumerate() {
                let mut v = a[r][0] * b[0][c] + a[r][1] * b[1][c] + a[r][2] * b[2][c];
                if c == 3 {
                    v += a[r][3];
                }
                *cell = v;
            }
        }
        Self { m: out }
    }

    /// Transforms a point (applies rotation and translation).
    pub fn transform_point(&self, p: Vec3) -> Vec3 {
        let m = &self.m;
        [
            m[0][0] * p[0] + m[0][1] * p[1] + m[0][2] * p[2] + m[0][3],
            m[1][0] * p[0] + m[1][1] * p[1] + m[1][2] * p[2] + m[1][3],
            m[2][0] * p[0] + m[2][1] * p[1] + m[2][2] * p[2] + m[2][3],
        ]
    }

    /// Transforms a direction (rotation only).
    pub fn transform_vector(&self, v: Vec3) -> Vec3 {
        let m = &self.m;
        [
            m[0][0] * v[0] + m[0][1] * v[1] + m[0][2] * v[2],
            m[1][0] * v[0] + m[1][1] * v[1] + m[1][2] * v[2],
            m[2][0] * v[0] + m[2][1] * v[1] + m[2][2] * v[2],
        ]
    }

    /// The translation column.
    pub fn origin(&self) -> Vec3 {
        [self.m[0][3], self.m[1][3], self.m[2][3]]
    }

    /// The local +Z axis in parent space (an overlay's face normal).
    pub fn axis_z(&self) -> Vec3 {
        [self.m[0][2], self.m[1][2], self.m[2][2]]
    }
}

/// Dot product.
pub fn dot(a: Vec3, b: Vec3) -> f32 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

/// Cross product.
pub fn cross(a: Vec3, b: Vec3) -> Vec3 {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}

/// `a - b`.
pub fn sub(a: Vec3, b: Vec3) -> Vec3 {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}

/// Unit vector, or zero for a (near-)zero input.
pub fn normalize(v: Vec3) -> Vec3 {
    let len = dot(v, v).sqrt();
    if len <= 1e-6 {
        [0.0; 3]
    } else {
        [v[0] / len, v[1] / len, v[2] / len]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::f32::consts::FRAC_PI_2;

    fn close(a: Vec3, b: Vec3) -> bool {
        a.iter().zip(b).all(|(x, y)| (x - y).abs() < 1e-5)
    }

    #[test]
    fn identity_and_translation() {
        let t = Mat34::translation(-0.12, -0.10, -0.6);
        assert_eq!(t.origin(), [-0.12, -0.10, -0.6]);
        assert_eq!(t.axis_z(), [0.0, 0.0, 1.0]);
        assert_eq!(Mat34::IDENTITY.mul(&t), t);
        assert_eq!(t.mul(&Mat34::IDENTITY), t);
        assert!(close(t.transform_point([1.0, 1.0, 1.0]), [0.88, 0.9, 0.4]));
    }

    #[test]
    fn rotations_are_right_handed() {
        assert!(close(
            Mat34::rotation_x(FRAC_PI_2).transform_vector([0.0, 1.0, 0.0]),
            [0.0, 0.0, 1.0]
        ));
        assert!(close(
            Mat34::rotation_y(FRAC_PI_2).transform_vector([0.0, 0.0, 1.0]),
            [1.0, 0.0, 0.0]
        ));
        assert!(close(
            Mat34::rotation_z(FRAC_PI_2).transform_vector([1.0, 0.0, 0.0]),
            [0.0, 1.0, 0.0]
        ));
    }

    #[test]
    fn mul_applies_rhs_first() {
        // Rotate a point at +X by 90° about Z, then translate by +1 Y.
        let m = Mat34::translation(0.0, 1.0, 0.0).mul(&Mat34::rotation_z(FRAC_PI_2));
        assert!(close(m.transform_point([1.0, 0.0, 0.0]), [0.0, 2.0, 0.0]));
        // Composition is associative with transform_point.
        let a = Mat34::rotation_x(0.3).mul(&Mat34::translation(0.1, 0.2, 0.3));
        let b = Mat34::rotation_y(-0.7);
        let p = [0.4, -0.5, 0.6];
        assert!(close(
            a.mul(&b).transform_point(p),
            a.transform_point(b.transform_point(p))
        ));
    }

    #[test]
    fn facing_points_z_at_target() {
        let pos = [-0.12, -0.10, -0.6];
        let m = Mat34::facing(pos, [0.0; 3], [0.0, 1.0, 0.0]);
        assert_eq!(m.origin(), pos);
        assert!(close(m.axis_z(), normalize(sub([0.0; 3], pos))));
        // Orthonormal basis, right-handed.
        let x = m.transform_vector([1.0, 0.0, 0.0]);
        let y = m.transform_vector([0.0, 1.0, 0.0]);
        let z = m.axis_z();
        assert!((dot(x, y)).abs() < 1e-5 && (dot(y, z)).abs() < 1e-5 && (dot(x, z)).abs() < 1e-5);
        assert!(close(cross(x, y), z));
        assert!(y[1] > 0.9, "up stays up: {y:?}");
        // Straight ahead reduces to identity rotation.
        let ahead = Mat34::facing([0.0, 0.0, -1.0], [0.0; 3], [0.0, 1.0, 0.0]);
        assert!(close(ahead.axis_z(), [0.0, 0.0, 1.0]));
        assert!(close(
            ahead.transform_vector([1.0, 0.0, 0.0]),
            [1.0, 0.0, 0.0]
        ));
    }

    #[test]
    fn facing_degenerate_inputs() {
        let m = Mat34::facing([1.0, 2.0, 3.0], [1.0, 2.0, 3.0], [0.0, 1.0, 0.0]);
        assert_eq!(m, Mat34::translation(1.0, 2.0, 3.0));
        let m = Mat34::facing([0.0, -1.0, 0.0], [0.0; 3], [0.0, 1.0, 0.0]);
        assert_eq!(m, Mat34::translation(0.0, -1.0, 0.0));
    }
}
