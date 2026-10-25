//! `Humanoid` object helpers.

use std::ops::Deref;

use crate::sdk::roblox::constants;
use crate::sdk::roblox::instance::Instance;

pub struct Humanoid {
    instance: Instance,
}

impl Humanoid {
    pub fn new(instance: Instance) -> Self {
        Self { instance }
    }

    pub fn instance(&self) -> &Instance {
        &self.instance
    }

    pub fn walk_speed(&self) -> f32 {
        self.try_walk_speed().unwrap_or(16.0)
    }

    pub fn try_walk_speed(&self) -> Option<f32> {
        let offsets = &self.instance.ctx().offsets.humanoid;
        read_f32(
            &self.instance,
            self.instance.address().wrapping_add(offsets.walk_speed),
        )
    }

    /// Writes both the value and the check-copy the game keeps next to it.
    pub fn set_walk_speed(&self, value: f32) -> bool {
        let offsets = &self.instance.ctx().offsets.humanoid;
        let memory = &self.instance.ctx().memory;
        let address = self.instance.address();

        let value_ok = memory.write_float(address + offsets.walk_speed, value);
        let check_ok = memory.write_float(address + offsets.walk_speed_check, value);

        value_ok && check_ok
    }

    pub fn jump_power(&self) -> f32 {
        let offsets = &self.instance.ctx().offsets.humanoid;
        read_f32(
            &self.instance,
            self.instance.address().wrapping_add(offsets.jump_power),
        )
        .unwrap_or(50.0)
    }

    pub fn set_jump_power(&self, value: f32) -> bool {
        let offsets = &self.instance.ctx().offsets.humanoid;
        self.instance
            .ctx()
            .memory
            .write_float(self.instance.address() + offsets.jump_power, value)
    }

    pub fn health(&self) -> f32 {
        let offsets = &self.instance.ctx().offsets.humanoid;
        read_f32(
            &self.instance,
            self.instance.address().wrapping_add(offsets.health),
        )
        .unwrap_or(0.0)
    }

    pub fn max_health(&self) -> f32 {
        let offsets = &self.instance.ctx().offsets.humanoid;
        read_f32(
            &self.instance,
            self.instance.address().wrapping_add(offsets.max_health),
        )
        .unwrap_or(100.0)
    }

    pub fn state(&self) -> String {
        self.instance
            .get_humanoid_state_id()
            .map(constants::state_name)
            .unwrap_or_else(|| "Unknown".to_string())
    }

    pub fn set_state(&self, state_name: &str) -> bool {
        match constants::state_id(state_name) {
            Some(id) => self.instance.set_humanoid_state_id(id),
            None => false,
        }
    }
}

impl Deref for Humanoid {
    type Target = Instance;

    fn deref(&self) -> &Self::Target {
        &self.instance
    }
}

fn read_f32(instance: &Instance, address: u64) -> Option<f32> {
    let data = instance.ctx().memory.read_bytes(address, 4);
    if data.len() < 4 {
        return None;
    }

    Some(f32::from_le_bytes([data[0], data[1], data[2], data[3]]))
}
