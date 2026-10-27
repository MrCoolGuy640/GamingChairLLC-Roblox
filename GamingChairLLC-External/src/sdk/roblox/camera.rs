//! `Camera` helpers: the properties Fly, Fling and Aim Assist need.

use crate::sdk::roblox::instance::Instance;
use crate::sdk::roblox::math::{self, Mat3};
use crate::sdk::roblox::vector3::Vector3;

pub struct Camera {
    instance: Instance,
}

impl Camera {
    pub fn new(instance: Instance) -> Self {
        Self { instance }
    }

    pub fn instance(&self) -> &Instance {
        &self.instance
    }

    pub fn position(&self) -> Option<Vector3> {
        let offsets = &self.instance.ctx().offsets.camera;
        let address = self.instance.address().wrapping_add(offsets.position);
        let data = self.instance.ctx().memory.read_bytes(address, 12);
        Vector3::from_bytes(&data).filter(|position| position.is_finite())
    }

    pub fn rotation_matrix(&self) -> Option<Mat3> {
        let offsets = &self.instance.ctx().offsets.camera;
        let address = self.instance.address().wrapping_add(offsets.rotation);
        let data = self.instance.ctx().memory.read_bytes(address, 36);
        math::from_bytes(&data).filter(math::is_valid)
    }

    pub fn rotation_address(&self) -> u64 {
        self.instance
            .address()
            .wrapping_add(self.instance.ctx().offsets.camera.rotation)
    }

    /// Direction the camera is looking, from its rotation matrix.
    pub fn look_vector(&self) -> Option<Vector3> {
        self.rotation_matrix().map(|rows| math::look_vector(&rows))
    }

    pub fn camera_subject(&self) -> u64 {
        let offsets = &self.instance.ctx().offsets.camera;
        self.instance
            .ctx()
            .memory
            .read_ptr(self.instance.address().wrapping_add(offsets.camera_subject))
    }

    pub fn set_camera_subject(&self, address: u64) -> bool {
        let offsets = &self.instance.ctx().offsets.camera;
        self.instance
            .ctx()
            .memory
            .write_ptr(self.instance.address().wrapping_add(offsets.camera_subject), address)
    }

    /// `Camera.FieldOfView`, in radians.
    pub fn field_of_view(&self) -> Option<f32> {
        let offsets = &self.instance.ctx().offsets.camera;
        let value = self
            .instance
            .ctx()
            .memory
            .read_float(self.instance.address().wrapping_add(offsets.field_of_view));

        (value.is_finite() && value > 0.01 && value < 3.2).then_some(value)
    }

    /// Viewport `(width, height)` in pixels.
    pub fn viewport_size(&self) -> Option<(f32, f32)> {
        let offsets = &self.instance.ctx().offsets.camera;
        let address = self.instance.address().wrapping_add(offsets.viewport_size);
        let data = self.instance.ctx().memory.read_bytes(address, 8);
        if data.len() < 8 {
            return None;
        }

        let width = f32::from_le_bytes([data[0], data[1], data[2], data[3]]);
        let height = f32::from_le_bytes([data[4], data[5], data[6], data[7]]);

        (width.is_finite()
            && height.is_finite()
            && (16.0..=32768.0).contains(&width)
            && (16.0..=32768.0).contains(&height))
            .then_some((width, height))
    }
}
