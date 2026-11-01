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

    /// `Player.Character` (or `Player.ModelInstance` on newer dumps). Both are
    /// direct pointers to the character model; when neither is available this
    /// returns `None` and callers fall back to looking the character up by
    /// player name under Workspace.
    ///
    /// The class check matters when a dump does not document
    /// `ModelInstance`: the fallback offset would otherwise read some other
    /// field as a pointer and hand back a bogus Instance that never falls
    /// through to the name lookup.
    pub fn character(&self) -> Option<Instance> {
        let offsets = &self.instance.ctx().offsets.player;

        for offset in [offsets.model_instance, offsets.character] {
            if offset == 0 {
                continue;
            }

            let character = self
                .instance
                .ctx()
                .memory
                .read_ptr(self.instance.address().wrapping_add(offset));

            if character == 0 {
                continue;
            }

            let instance = Instance::new(character, self.instance.ctx().clone());
            if instance.get_class_name() == "Model" {
                return Some(instance);
            }
        }

        None
    }

    /// Whether this instance is `Players.LocalPlayer`.
    ///
    /// `LocalPlayer` is a field on the Players service, not on a Player, so the
    /// pointer has to be read from this player's parent (`Players`). Reading it
    /// from the Player itself always yielded a bogus pointer, which is why the
    /// ESP's "Highlight Local Player" toggle used to have no effect.
    pub fn is_local_player(&self) -> bool {
        let offsets = &self.instance.ctx().offsets.player;
        let parent = self.instance.parent_address();
        if parent == 0 {
            return false;
        }

        let local = self
            .instance
            .ctx()
            .memory
            .read_ptr(parent.wrapping_add(offsets.local_player));

        local != 0 && local == self.instance.address()
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
