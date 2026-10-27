//! `Part` (BasePart) helpers: position/size/rotation, primitive flags, and the
//! memory-freezer variants of the write helpers.

use std::collections::HashMap;
use std::ops::Deref;

use parking_lot::Mutex;

use crate::sdk::roblox::instance::Instance;
use crate::sdk::roblox::math::Mat3;
use crate::sdk::roblox::vector3::Vector3;

const FROZEN_POSITION: &str = "Position";
const FROZEN_ROTATION: &str = "Rotation";
const FROZEN_VELOCITY: &str = "Velocity";
const FROZEN_ANGULAR_VELOCITY: &str = "AngularVelocity";

pub struct Part {
    instance: Instance,
    /// Addresses cached by freeze/update/unfreeze, keyed by property.
    frozen: Mutex<HashMap<&'static str, u64>>,
}

impl Part {
    pub fn new(instance: Instance) -> Self {
        Self {
            instance,
            frozen: Mutex::new(HashMap::new()),
        }
    }

    pub fn instance(&self) -> &Instance {
        &self.instance
    }

    fn primitive_address(&self) -> u64 {
        let offsets = &self.instance.ctx().offsets;
        self.instance
            .ctx()
            .memory
            .read_ptr(self.instance.address().wrapping_add(offsets.base_part.primitive))
    }

    /// A genuine BasePart: the Primitive exists and its `Owner` back-pointer
    /// points at this Instance. A non-BasePart child holds junk at the
    /// Primitive offset, and writing there would write into the game's heap.
    pub fn is_valid(&self) -> bool {
        let primitive = self.primitive_address();
        if primitive == 0 {
            return false;
        }

        let owner = self
            .instance
            .ctx()
            .memory
            .read_ptr(primitive.wrapping_add(self.instance.ctx().offsets.primitive.owner));

        owner == self.instance.address()
    }

    pub fn rotation_address(&self) -> Option<u64> {
        self.vec3_address(self.instance.ctx().offsets.primitive.rotation)
    }

    pub fn angular_velocity_address(&self) -> Option<u64> {
        self.vec3_address(
            self.instance
                .ctx()
                .offsets
                .primitive
                .assembly_angular_velocity,
        )
    }

    /// The part's full row-major 3x3 rotation, validated as orthonormal.
    pub fn rotation_matrix(&self) -> Option<Mat3> {
        let address = self.rotation_address()?;
        let data = self.instance.ctx().memory.read_bytes(address, 36);
        let rows = crate::sdk::roblox::math::from_bytes(&data)?;

        crate::sdk::roblox::math::is_valid(&rows).then_some(rows)
    }

    /// Write all nine floats of the rotation matrix. `write_rotation` only
    /// replaces row 0 and leaves a sheared, non-orthonormal matrix behind.
    pub fn write_rotation_matrix(&self, rows: &Mat3) -> bool {
        let Some(address) = self.rotation_address() else {
            return false;
        };

        self.instance
            .ctx()
            .memory
            .write_bytes(address, &crate::sdk::roblox::math::to_bytes(rows))
    }

    fn vec3_address(&self, offset: u64) -> Option<u64> {
        let primitive = self.primitive_address();
        (primitive != 0).then_some(primitive.wrapping_add(offset))
    }

    fn read_vec3(&self, offset: u64) -> Option<Vector3> {
        let address = self.vec3_address(offset)?;
        let data = self.instance.ctx().memory.read_bytes(address, 12);
        Vector3::from_bytes(&data)
    }

    pub fn position(&self) -> Option<Vector3> {
        self.read_vec3(self.instance.ctx().offsets.primitive.position)
    }

    pub fn size(&self) -> Option<Vector3> {
        self.read_vec3(self.instance.ctx().offsets.primitive.size)
    }

    pub fn rotation(&self) -> Option<Vector3> {
        self.read_vec3(self.instance.ctx().offsets.primitive.rotation)
    }

    pub fn velocity(&self) -> Option<Vector3> {
        self.read_vec3(self.instance.ctx().offsets.primitive.assembly_linear_velocity)
    }

    pub fn angular_velocity(&self) -> Option<Vector3> {
        self.read_vec3(
            self.instance
                .ctx()
                .offsets
                .primitive
                .assembly_angular_velocity,
        )
    }

    pub fn write_position(&self, position: Vector3, freeze: bool) -> bool {
        self.write_vec3(
            self.instance.ctx().offsets.primitive.position,
            FROZEN_POSITION,
            position,
            freeze,
        )
    }

    pub fn update_frozen_position(&self, position: Vector3) {
        self.update_frozen(FROZEN_POSITION, position);
    }

    pub fn unfreeze_position(&self) {
        self.unfreeze(FROZEN_POSITION);
    }

    pub fn write_rotation(&self, rotation: Vector3, freeze: bool) -> bool {
        self.write_vec3(
            self.instance.ctx().offsets.primitive.rotation,
            FROZEN_ROTATION,
            rotation,
            freeze,
        )
    }

    pub fn unfreeze_rotation(&self) {
        self.unfreeze(FROZEN_ROTATION);
    }

    pub fn write_velocity(&self, velocity: Vector3, freeze: bool) -> bool {
        self.write_vec3(
            self.instance.ctx().offsets.primitive.assembly_linear_velocity,
            FROZEN_VELOCITY,
            velocity,
            freeze,
        )
    }

    pub fn update_frozen_velocity(&self, velocity: Vector3) {
        self.update_frozen(FROZEN_VELOCITY, velocity);
    }

    pub fn unfreeze_velocity(&self) {
        self.unfreeze(FROZEN_VELOCITY);
    }

    pub fn write_angular_velocity(&self, velocity: Vector3, freeze: bool) -> bool {
        self.write_vec3(
            self.instance
                .ctx()
                .offsets
                .primitive
                .assembly_angular_velocity,
            FROZEN_ANGULAR_VELOCITY,
            velocity,
            freeze,
        )
    }

    pub fn unfreeze_angular_velocity(&self) {
        self.unfreeze(FROZEN_ANGULAR_VELOCITY);
    }

    pub fn write_size(&self, size: Vector3) -> bool {
        let Some(address) = self.vec3_address(self.instance.ctx().offsets.primitive.size)
        else {
            return false;
        };
        self.instance
            .ctx()
            .memory
            .write_bytes(address, &size.to_bytes())
    }

    fn write_vec3(&self, offset: u64, key: &'static str, value: Vector3, freeze: bool) -> bool {
        let Some(address) = self.vec3_address(offset) else {
            return false;
        };
        let data = value.to_bytes();

        if freeze {
            self.frozen.lock().insert(key, address);
            self.instance.ctx().freezer.freeze(address, data.to_vec());
            return true;
        }

        self.instance.ctx().memory.write_bytes(address, &data)
    }

    fn update_frozen(&self, key: &str, value: Vector3) {
        let address = self.frozen.lock().get(key).copied();
        if let Some(address) = address {
            self.instance
                .ctx()
                .freezer
                .update(address, &value.to_bytes());
        }
    }

    fn unfreeze(&self, key: &str) {
        let address = self.frozen.lock().remove(key);
        if let Some(address) = address {
            self.instance.ctx().freezer.unfreeze(address);
        }
    }

    // -- primitive flags --------------------------------------------------

    fn primitive_flags(&self) -> Option<i32> {
        let primitive = self.primitive_address();
        if primitive == 0 {
            return None;
        }

        Some(
            self.instance
                .ctx()
                .memory
                .read_int(primitive + self.instance.ctx().offsets.primitive.flags),
        )
    }

    fn set_primitive_flags(&self, flags: i32) -> bool {
        let primitive = self.primitive_address();
        if primitive == 0 {
            return false;
        }

        self.instance
            .ctx()
            .memory
            .write_int(primitive + self.instance.ctx().offsets.primitive.flags, flags)
    }

    pub fn anchored(&self) -> bool {
        self.flag(self.instance.ctx().offsets.primitive_flags.anchored)
    }

    pub fn set_anchored(&self, anchored: bool) -> bool {
        self.set_flag(self.instance.ctx().offsets.primitive_flags.anchored, anchored)
    }

    pub fn can_collide(&self) -> bool {
        self.flag(self.instance.ctx().offsets.primitive_flags.can_collide)
    }

    pub fn set_can_collide(&self, can_collide: bool) -> bool {
        self.set_flag(
            self.instance.ctx().offsets.primitive_flags.can_collide,
            can_collide,
        )
    }

    fn flag(&self, mask: i32) -> bool {
        self.primitive_flags()
            .map(|flags| flags & mask != 0)
            .unwrap_or(false)
    }

    fn set_flag(&self, mask: i32, enabled: bool) -> bool {
        let Some(mut flags) = self.primitive_flags() else {
            return false;
        };

        if enabled {
            flags |= mask;
        } else {
            flags &= !mask;
        }

        self.set_primitive_flags(flags)
    }

    pub fn massless(&self) -> bool {
        let address = self
            .instance
            .address()
            .wrapping_add(self.instance.ctx().offsets.base_part.massless);
        let data = self.instance.ctx().memory.read_bytes(address, 1);
        data.first().is_some_and(|value| *value != 0)
    }

    pub fn set_massless(&self, value: bool) -> bool {
        let address = self
            .instance
            .address()
            .wrapping_add(self.instance.ctx().offsets.base_part.massless);
        self.instance
            .ctx()
            .memory
            .write_bytes(address, &[u8::from(value)])
    }
}

impl Deref for Part {
    type Target = Instance;

    fn deref(&self) -> &Self::Target {
        &self.instance
    }
}
