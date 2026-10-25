//! High-level lookups used by cheats and the explorer.

use std::sync::Arc;

use crate::sdk::roblox::humanoid::Humanoid;
use crate::sdk::roblox::instance::Instance;
use crate::sdk::roblox::part::Part;
use crate::sdk::roblox::player::Player;

pub fn get_local_player(datamodel: &Instance) -> Option<Player> {
    let players = datamodel.find_first_child_of_name("Players")?;
    let offsets = &players.ctx().offsets.player;

    // Preferred: the Players service stores a direct pointer to LocalPlayer.
    let address = players
        .ctx()
        .memory
        .read_ptr(players.address().wrapping_add(offsets.local_player));

    if address != 0 {
        return Some(Player::new(Instance::new(
            address,
            Arc::clone(players.ctx()),
        )));
    }

    // Fallback for offsets without a usable LocalPlayer pointer: the first
    // child of Players is the local player in a live client.
    players
        .get_children()
        .into_iter()
        .next()
        .map(Player::new)
}

pub fn get_local_character(datamodel: &Instance) -> Option<Instance> {
    let player = get_local_player(datamodel)?;

    if let Some(character) = player.character() {
        return Some(character);
    }

    let workspace = datamodel.find_first_child_of_name("Workspace")?;
    let player_name = player.get_name();
    if player_name.is_empty() {
        return None;
    }

    workspace.find_first_child_of_name(&player_name)
}

pub fn get_local_humanoid(datamodel: &Instance) -> Option<Humanoid> {
    let character = get_local_character(datamodel)?;
    character
        .find_first_child_of_class("Humanoid")
        .map(Humanoid::new)
}

pub fn get_local_root_part(datamodel: &Instance) -> Option<Part> {
    let character = get_local_character(datamodel)?;
    character
        .find_first_child_of_name("HumanoidRootPart")
        .map(Part::new)
}
