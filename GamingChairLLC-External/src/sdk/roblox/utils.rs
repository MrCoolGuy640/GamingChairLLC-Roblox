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

/// A player's character model. Prefers the direct `Player.ModelInstance`
/// pointer and falls back to matching the player's name under Workspace.
pub fn get_character(
    datamodel: &Instance,
    player: &Player,
    workspace: Option<&Instance>,
) -> Option<Instance> {
    if let Some(character) = player.character() {
        if character.address() != 0 {
            return Some(character);
        }
    }

    let workspace = match workspace {
        Some(workspace) => workspace.clone(),
        None => datamodel.find_first_child_of_name("Workspace")?,
    };
    let name = player.get_name();
    if name.is_empty() {
        return None;
    }

    workspace.find_first_child_of_name(&name)
}

pub fn get_character_root(character: &Instance) -> Option<Part> {
    character
        .find_first_child_of_name("HumanoidRootPart")
        .map(Part::new)
}

/// `Humanoid.Health`, or `None` when the character has no Humanoid or the
/// read failed. `None` means "cannot tell" and callers treat it as alive.
pub fn get_humanoid_health(character: &Instance) -> Option<f32> {
    let humanoid = character.find_first_child_of_name("Humanoid")?;
    let offsets = &humanoid.ctx().offsets.humanoid;
    let value = humanoid
        .ctx()
        .memory
        .read_float(humanoid.address().wrapping_add(offsets.health));

    (value.is_finite()).then_some(value)
}

/// Every genuine BasePart of a character, root part first.
///
/// The rig is one physics assembly, so zeroing the root alone is not enough:
/// the limbs keep their own momentum and the solver hands it straight back.
/// Each child is validated structurally through `Part::is_valid`.
pub fn character_parts(character: &Instance, root: Option<&Part>) -> Vec<Part> {
    let mut parts: Vec<Part> = character
        .get_children()
        .into_iter()
        .map(Part::new)
        .filter(Part::is_valid)
        .collect();

    if let Some(root) = root {
        parts.retain(|part| part.instance().address() != root.instance().address());
        parts.insert(0, Part::new(root.instance().clone()));
    }

    parts
}
