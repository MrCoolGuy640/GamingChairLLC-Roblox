//! Rotation-matrix and look-at maths shared by Fly, Fling and Aim Assist.
//!
//! Roblox stores a CFrame's rotation as a row-major 3x3 matrix. The basis
//! vectors live in its *columns*: column 0 is right, column 1 is up and column
//! 2 is backwards (the look direction is its negation). Everything here uses
//! that same layout, matching the Python implementation.

use std::f32::consts::PI;

use super::vector3::Vector3;

/// A row-major 3x3 matrix.
pub type Mat3 = [Vector3; 3];

pub fn identity() -> Mat3 {
    [
        Vector3::new(1.0, 0.0, 0.0),
        Vector3::new(0.0, 1.0, 0.0),
        Vector3::new(0.0, 0.0, 1.0),
    ]
}

pub fn is_valid(rows: &Mat3) -> bool {
    rows.iter().all(|row| row.is_finite())
        && rows
            .iter()
            .all(|row| (row.magnitude() - 1.0).abs() <= 0.02)
}

pub fn to_bytes(rows: &Mat3) -> [u8; 36] {
    let mut out = [0u8; 36];
    out[0..12].copy_from_slice(&rows[0].to_bytes());
    out[12..24].copy_from_slice(&rows[1].to_bytes());
    out[24..36].copy_from_slice(&rows[2].to_bytes());
    out
}

pub fn from_bytes(data: &[u8]) -> Option<Mat3> {
    if data.len() < 36 {
        return None;
    }

    let row = |offset: usize| {
        Vector3::from_bytes(&data[offset..offset + 12]).unwrap_or_default()
    };
    Some([row(0), row(12), row(24)])
}

/// The camera's look direction: the negation of column 2.
pub fn look_vector(rows: &Mat3) -> Vector3 {
    Vector3::new(-rows[0].z, -rows[1].z, -rows[2].z)
}

/// Yaw of a row-major 3x3, in radians, in the game's angle convention.
///
/// Read off the first column (`r0.x = cos`, `r2.x = -sin`), which is the one
/// place the yaw survives pitch untouched.
pub fn yaw_from_matrix(rows: &Mat3) -> f32 {
    let yaw = (-rows[2].x).atan2(rows[0].x) + PI;
    wrap_angle(yaw)
}

/// Normalise an angle to `[-PI, PI]`.
pub fn wrap_angle(angle: f32) -> f32 {
    let mut angle = angle % (2.0 * PI);
    if angle > PI {
        angle -= 2.0 * PI;
    } else if angle < -PI {
        angle += 2.0 * PI;
    }
    angle
}

/// Yaw of a look direction, in radians (the convention `forward` uses).
pub fn yaw_from_look(look: Vector3) -> f32 {
    look.x.atan2(look.z)
}

pub fn forward(yaw: f32) -> Vector3 {
    Vector3::new(yaw.sin(), 0.0, yaw.cos())
}

pub fn right(yaw: f32) -> Vector3 {
    Vector3::new(yaw.cos(), 0.0, -yaw.sin())
}

/// Full upright matrix for a given yaw. Equivalent to the Python
/// `rotation_matrix_from_angles(0, yaw)`.
pub fn rotation_matrix_from_yaw(yaw: f32) -> Mat3 {
    let game_yaw = yaw - PI;
    let (cos, sin) = (game_yaw.cos(), game_yaw.sin());

    [
        Vector3::new(cos, 0.0, sin),
        Vector3::new(0.0, 1.0, 0.0),
        Vector3::new(-sin, 0.0, cos),
    ]
}

/// Row-major matrix that points a camera from `eye` at `target`, with no roll.
///
/// Built the way `CFrame.lookAt` does: column 2 is the backwards vector,
/// column 0 is right, column 1 is up. Returns `None` when the points coincide
/// or the view is exactly vertical, where "no roll" has no answer.
pub fn look_at_rows(eye: Vector3, target: Vector3) -> Option<Mat3> {
    let back = eye.sub(target).normalize();
    if back.magnitude() == 0.0 {
        return None;
    }

    let right = Vector3::new(0.0, 1.0, 0.0).cross(back).normalize();
    if right.magnitude() == 0.0 {
        return None;
    }

    let up = back.cross(right);

    Some([
        Vector3::new(right.x, up.x, back.x),
        Vector3::new(right.y, up.y, back.y),
        Vector3::new(right.z, up.z, back.z),
    ])
}

/// Where `point` lands on screen: `(pixels from centre, depth in studs)`.
pub fn project(rows: &Mat3, eye: Vector3, fov: f32, viewport: (f32, f32), point: Vector3) -> Option<(f32, f32)> {
    let rel = point.sub(eye);
    let right = Vector3::new(rows[0].x, rows[1].x, rows[2].x);
    let up = Vector3::new(rows[0].y, rows[1].y, rows[2].y);
    let look = look_vector(rows);

    let depth = rel.dot(look);
    if depth <= 0.05 {
        return None;
    }

    // The vertical fov is enough: pixels are square, so one focal length
    // serves both axes.
    let focal = (viewport.1 * 0.5) / (fov * 0.5).tan();
    let x = rel.dot(right) / depth * focal;
    let y = rel.dot(up) / depth * focal;

    Some(((x * x + y * y).sqrt(), depth))
}

// ---------------------------------------------------------------------------
// Quaternions, for smooth aim easing
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug)]
pub struct Quaternion {
    pub w: f32,
    pub x: f32,
    pub y: f32,
    pub z: f32,
}

pub fn quaternion_from_matrix(rows: &Mat3) -> Quaternion {
    let m00 = rows[0].x;
    let m01 = rows[0].y;
    let m02 = rows[0].z;
    let m10 = rows[1].x;
    let m11 = rows[1].y;
    let m12 = rows[1].z;
    let m20 = rows[2].x;
    let m21 = rows[2].y;
    let m22 = rows[2].z;

    let trace = m00 + m11 + m22;

    let (w, x, y, z) = if trace > 0.0 {
        let s = (trace + 1.0).sqrt() * 2.0;
        (0.25 * s, (m21 - m12) / s, (m02 - m20) / s, (m10 - m01) / s)
    } else if m00 > m11 && m00 > m22 {
        let s = (1.0 + m00 - m11 - m22).sqrt() * 2.0;
        ((m21 - m12) / s, 0.25 * s, (m01 + m10) / s, (m02 + m20) / s)
    } else if m11 > m22 {
        let s = (1.0 + m11 - m00 - m22).sqrt() * 2.0;
        ((m02 - m20) / s, (m01 + m10) / s, 0.25 * s, (m12 + m21) / s)
    } else {
        let s = (1.0 + m22 - m00 - m11).sqrt() * 2.0;
        ((m10 - m01) / s, (m02 + m20) / s, (m12 + m21) / s, 0.25 * s)
    };

    Quaternion { w, x, y, z }.normalized()
}

pub fn matrix_from_quaternion(q: Quaternion) -> Mat3 {
    let q = q.normalized();
    let (w, x, y, z) = (q.w, q.x, q.y, q.z);

    let xx = x * x;
    let yy = y * y;
    let zz = z * z;
    let xy = x * y;
    let xz = x * z;
    let yz = y * z;
    let wx = w * x;
    let wy = w * y;
    let wz = w * z;

    [
        Vector3::new(
            1.0 - 2.0 * (yy + zz),
            2.0 * (xy - wz),
            2.0 * (xz + wy),
        ),
        Vector3::new(
            2.0 * (xy + wz),
            1.0 - 2.0 * (xx + zz),
            2.0 * (yz - wx),
        ),
        Vector3::new(
            2.0 * (xz - wy),
            2.0 * (yz + wx),
            1.0 - 2.0 * (xx + yy),
        ),
    ]
}

impl Quaternion {
    pub fn normalized(self) -> Self {
        let length = (self.w * self.w + self.x * self.x + self.y * self.y + self.z * self.z)
            .sqrt();
        if length <= 1e-8 {
            return Self {
                w: 1.0,
                x: 0.0,
                y: 0.0,
                z: 0.0,
            };
        }

        Self {
            w: self.w / length,
            x: self.x / length,
            y: self.y / length,
            z: self.z / length,
        }
    }

    fn dot(self, other: Self) -> f32 {
        self.w * other.w + self.x * other.x + self.y * other.y + self.z * other.z
    }

    fn scale(self, value: f32) -> Self {
        Self {
            w: self.w * value,
            x: self.x * value,
            y: self.y * value,
            z: self.z * value,
        }
    }

    fn add(self, other: Self) -> Self {
        Self {
            w: self.w + other.w,
            x: self.x + other.x,
            y: self.y + other.y,
            z: self.z + other.z,
        }
    }
}

/// Shortest-path spherical interpolation. `t` is clamped to `[0, 1]`.
pub fn slerp(from: Quaternion, to: Quaternion, t: f32) -> Quaternion {
    let t = t.clamp(0.0, 1.0);
    let mut cosine = from.dot(to);

    // Take the shorter arc.
    let mut target = to;
    if cosine < 0.0 {
        cosine = -cosine;
        target = to.scale(-1.0);
    }

    if cosine > 0.9995 {
        // Nearly identical: a plain lerp is stable and visually identical.
        return from.add(target.sub(from).scale(t)).normalized();
    }

    let theta = cosine.clamp(-1.0, 1.0).acos();
    let sin_theta = theta.sin();
    let from_weight = ((1.0 - t) * theta).sin() / sin_theta;
    let to_weight = (t * theta).sin() / sin_theta;

    from.scale(from_weight).add(target.scale(to_weight)).normalized()
}

impl Quaternion {
    fn sub(self, other: Self) -> Self {
        Self {
            w: self.w - other.w,
            x: self.x - other.x,
            y: self.y - other.y,
            z: self.z - other.z,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: f32, b: f32) -> bool {
        (a - b).abs() < 1e-4
    }

    #[test]
    fn yaw_round_trips_through_an_upright_matrix() {
        for degrees in [-170.0f32, -90.0, 0.0, 45.0, 179.0] {
            let yaw = degrees.to_radians();
            let rows = rotation_matrix_from_yaw(yaw);

            assert!(is_valid(&rows), "matrix for {degrees} should be orthonormal");
            assert!(close(yaw_from_matrix(&rows), yaw), "yaw for {degrees}");
        }
    }

    #[test]
    fn look_at_points_down_the_negative_back_column() {
        let rows =
            look_at_rows(Vector3::new(0.0, 5.0, 10.0), Vector3::new(0.0, 5.0, 0.0)).unwrap();
        let look = look_vector(&rows);

        assert!(close(look.x, 0.0));
        assert!(close(look.y, 0.0));
        assert!(close(look.z, -1.0));
    }

    #[test]
    fn projection_puts_a_point_on_the_crosshair() {
        let rows = rotation_matrix_from_yaw(0.0);
        let eye = Vector3::new(0.0, 0.0, 0.0);
        // Straight ahead for yaw 0 is +Z.
        let (distance, depth) = project(&rows, eye, 1.0, (1920.0, 1080.0), Vector3::new(0.0, 0.0, 10.0)).unwrap();

        assert!(distance < 1e-3);
        assert!(close(depth, 10.0));
    }

    #[test]
    fn quaternion_round_trip_preserves_the_matrix() {
        let rows = look_at_rows(
            Vector3::new(3.0, 4.0, 5.0),
            Vector3::new(-2.0, 1.0, 0.0),
        )
        .unwrap();
        let back = matrix_from_quaternion(quaternion_from_matrix(&rows));

        for row in 0..3 {
            assert!(close(back[row].x, rows[row].x));
            assert!(close(back[row].y, rows[row].y));
            assert!(close(back[row].z, rows[row].z));
        }
    }
}
