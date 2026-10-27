//! `Player` object helpers.

use std::ops::Deref;

use crate::sdk::roblox::instance::Instance;

pub struct Player {
    instance: Instance,
}

impl Player {
    pub fn new(instance: Instance) -> Self {
        Self { instance }
    }

    pub fn instance(&self) -> &Instance {
        &self.instance
    }

    /// `Player.Character` is not exposed by every offsets release; when the
    /// offset is unavailable this returns `None` and callers fall back to
    /// looking the character up by player name under Workspace.
    pub fn character(&self) -> Option<Instance> {
        let offsets = &self.instance.ctx().offsets.player;
        if offsets.character == 0 {
            return None;
        }

        let character = self
            .instance
            .ctx()
            .memory
            .read_ptr(self.instance.address().wrapping_add(offsets.character));

        (character != 0).then(|| Instance::new(character, self.instance.ctx().clone()))
    }

    pub fn is_local_player(&self) -> bool {
        let offsets = &self.instance.ctx().offsets.player;
        let local = self
            .instance
            .ctx()
            .memory
            .read_ptr(self.instance.address().wrapping_add(offsets.local_player));

        local == self.instance.address()
    }

    /// `Player.UserId`. `0` when the offset is unavailable or the read failed.
    pub fn user_id(&self) -> i64 {
        let offsets = &self.instance.ctx().offsets.player;
        if offsets.user_id == 0 {
            return 0;
        }

        self.instance
            .ctx()
            .memory
            .read_long(self.instance.address().wrapping_add(offsets.user_id))
    }
}

impl Deref for Player {
    type Target = Instance;

    fn deref(&self) -> &Self::Target {
        &self.instance
    }
}
