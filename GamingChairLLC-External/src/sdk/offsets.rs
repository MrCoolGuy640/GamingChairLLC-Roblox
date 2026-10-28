//! Roblox offsets: download for the running game version, cache on disk, and
//! expose the handful of fields the app actually uses as typed integers.
//!
//! Offsets come from <https://rbxoffsets.com>: `/api/v1/{platform}/offsets/
//! {version}/json` returns the full dump wrapped in an `Offsets` object. Keys
//! are not guaranteed to remain stable between game versions, so every section
//! has a sensible fallback and only the data-model pointers are treated as
//! mandatory.

use std::fs;
use std::path::Path;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{anyhow, Context, Result};
use serde_json::Value;

use crate::config::{self, Paths};
use crate::sdk::process;

// ---------------------------------------------------------------------------
// Typed sections
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct InstanceOffsets {
    pub name_container: u64,
    pub name: u64,
    pub children_start: u64,
    pub children_end: u64,
    pub class_descriptor: u64,
    pub class_name: u64,
}

#[derive(Debug, Clone)]
pub struct FakeDataModelOffsets {
    pub pointer: u64,
    pub real_data_model: u64,
}

#[derive(Debug, Clone)]
pub struct HumanoidOffsets {
    pub walk_speed: u64,
    pub walk_speed_check: u64,
    pub jump_power: u64,
    pub health: u64,
    pub max_health: u64,
    /// Offset on the Humanoid that holds the *pointer* to its state object.
    pub state_ptr: u64,
    /// Offset inside that state object that holds the state id.
    pub state_id: u64,
}

#[derive(Debug, Clone)]
pub struct BasePartOffsets {
    pub primitive: u64,
    pub massless: u64,
}

#[derive(Debug, Clone)]
pub struct PrimitiveOffsets {
    pub position: u64,
    pub size: u64,
    pub rotation: u64,
    pub flags: u64,
    pub assembly_linear_velocity: u64,
    pub assembly_angular_velocity: u64,
    /// Back-pointer to the owning Instance. Used to prove that a candidate
    /// really is a BasePart before writing to its Primitive.
    pub owner: u64,
}

#[derive(Debug, Clone)]
pub struct PrimitiveFlagsOffsets {
    pub anchored: i32,
    pub can_collide: i32,
}

#[derive(Debug, Clone)]
pub struct PlayerOffsets {
    pub local_player: u64,
    /// `Player.ModelInstance`, the direct pointer to the character model.
    /// Not present in every offsets release; 0 means "unavailable".
    pub model_instance: u64,
    /// Legacy alias for the character pointer. Kept so a dump that only
    /// documents `Character` still works.
    pub character: u64,
    /// `Player.UserId`.
    pub user_id: u64,
}

#[derive(Debug, Clone)]
pub struct CameraOffsets {
    pub camera_subject: u64,
    pub position: u64,
    /// Row-major 3x3 rotation matrix (36 bytes).
    pub rotation: u64,
    /// `Camera.FieldOfView`, in radians.
    pub field_of_view: u64,
    /// Two floats: width, height.
    pub viewport_size: u64,
}

#[derive(Debug, Clone)]
pub struct WorkspaceOffsets {
    pub current_camera: u64,
    pub world: u64,
    /// `Workspace.DistributedGameTime`, an f64, used to count rendered frames.
    pub distributed_game_time: u64,
}

#[derive(Debug, Clone)]
pub struct WorldOffsets {
    pub fallen_parts_destroy_height: u64,
    pub gravity: u64,
}

#[derive(Debug, Clone)]
pub struct DataModelOffsets {
    pub place_id: u64,
    pub game_loaded: u64,
    pub server_ip: u64,
}

#[derive(Debug, Clone)]
pub struct ChatOffsets {
    /// `ChatInputBarConfiguration.IsFocused`.
    pub is_focused: u64,
}

/// Custom, hardcoded offsets for script bytecode.
///
/// These do **not** come from the offsets API: its bytecode section has been
/// wrong for several client versions. They mirror the `CUSTOM_OFFSETS` table in
/// `GamingChairLLC-Python/utils/roblox.py`:
///
/// * `ModuleScript` / `LocalScript` — offset on the instance that holds the
///   pointer to its bytecode structure.
/// * `bytecode_buffer` / `bytecode_size` — fields inside that structure.
///
/// When the app grows its own offsets API these become fields of a parsed
/// section instead of constants; keeping them in one struct means only
/// `Default` has to change.
#[derive(Debug, Clone)]
pub struct ScriptOffsets {
    pub module_script_bytecode: u64,
    pub local_script_bytecode: u64,
    pub bytecode_buffer: u64,
    pub bytecode_size: u64,
}

impl Default for ScriptOffsets {
    fn default() -> Self {
        Self {
            module_script_bytecode: 0x120,
            local_script_bytecode: 0x178,
            bytecode_buffer: 0x10,
            bytecode_size: 0x20,
        }
    }
}

#[derive(Debug, Clone)]
pub struct Offsets {
    pub instance: InstanceOffsets,
    pub fake_data_model: FakeDataModelOffsets,
    pub humanoid: HumanoidOffsets,
    pub base_part: BasePartOffsets,
    pub primitive: PrimitiveOffsets,
    pub primitive_flags: PrimitiveFlagsOffsets,
    pub player: PlayerOffsets,
    pub data_model: DataModelOffsets,
    pub chat: ChatOffsets,
    pub camera: CameraOffsets,
    pub workspace: WorkspaceOffsets,
    pub world: WorldOffsets,
    /// Hardcoded bytecode offsets (see [`ScriptOffsets`]).
    pub scripts: ScriptOffsets,
}

impl Offsets {
    fn parse(root: &Value) -> Result<Self> {
        let fake = root
            .get("FakeDataModel")
            .ok_or_else(|| anyhow!("offsets JSON has no FakeDataModel section"))?;

        let fake = Some(fake);

        let fake_pointer = number(fake, "Pointer")
            .filter(|value| *value > 0)
            .ok_or_else(|| anyhow!("offsets JSON has no FakeDataModel.Pointer"))?;
        let real_data_model = number(fake, "RealDataModel")
            .filter(|value| *value > 0)
            .ok_or_else(|| anyhow!("offsets JSON has no FakeDataModel.RealDataModel"))?;

        let instance = root.get("Instance");
        let humanoid = root.get("Humanoid");
        let base_part = root.get("BasePart");
        let primitive = root.get("Primitive");
        let flags = root.get("PrimitiveFlags");
        let player = root.get("Player");
        let data_model = root.get("DataModel");
        let chat = root.get("ChatInputBarConfiguration");
        let camera = root.get("Camera");
        let workspace = root.get("Workspace");
        let world = root.get("World");

        // Reading WalkSpeed: newer offset dumps expose `WalkSpeed`, older ones
        // only `Walkspeed`. Both name the same field; the value written by the
        // game is also mirrored into `WalkspeedCheck`.
        let walk_speed = number(humanoid, "WalkSpeed")
            .or_else(|| number(humanoid, "Walkspeed"))
            .unwrap_or(452);

        Ok(Self {
            instance: InstanceOffsets {
                name_container: number(instance, "NameContainer").unwrap_or(104),
                name: number(instance, "Name").unwrap_or(0),
                children_start: number(instance, "ChildrenStart").unwrap_or(112),
                children_end: number(instance, "ChildrenEnd").unwrap_or(8),
                class_descriptor: number(instance, "ClassDescriptor").unwrap_or(24),
                class_name: number(instance, "ClassName").unwrap_or(8),
            },
            fake_data_model: FakeDataModelOffsets {
                pointer: fake_pointer,
                real_data_model,
            },
            humanoid: HumanoidOffsets {
                walk_speed,
                walk_speed_check: number(humanoid, "WalkspeedCheck").unwrap_or(924),
                jump_power: number(humanoid, "JumpPower").unwrap_or(408),
                health: number(humanoid, "Health").unwrap_or(388),
                max_health: number(humanoid, "MaxHealth").unwrap_or(412),
                // Older dumps do not name the state pointer; Python hardcoded 2232.
                state_ptr: number(humanoid, "HumanoidState").unwrap_or(2232),
                state_id: number(humanoid, "HumanoidStateID").unwrap_or(32),
            },
            base_part: BasePartOffsets {
                primitive: number(base_part, "Primitive").unwrap_or(376),
                massless: number(base_part, "Massless").unwrap_or(290),
            },
            primitive: PrimitiveOffsets {
                position: number(primitive, "Position").unwrap_or(212),
                size: number(primitive, "Size").unwrap_or(452),
                rotation: number(primitive, "Rotation").unwrap_or(176),
                flags: number(primitive, "Flags").unwrap_or(446),
                assembly_linear_velocity: number(primitive, "AssemblyLinearVelocity").unwrap_or(224),
                assembly_angular_velocity: number(primitive, "AssemblyAngularVelocity")
                    .unwrap_or(236),
                owner: number(primitive, "Owner").unwrap_or(536),
            },
            primitive_flags: PrimitiveFlagsOffsets {
                anchored: int_number(flags, "Anchored").unwrap_or(2),
                can_collide: int_number(flags, "CanCollide").unwrap_or(8),
            },
            player: PlayerOffsets {
                local_player: number(player, "LocalPlayer").unwrap_or(280),
                model_instance: number(player, "ModelInstance").unwrap_or(656),
                character: number(player, "Character").unwrap_or(0),
                user_id: number(player, "UserId").unwrap_or(184),
            },
            // `DataModel` and `ChatInputBarConfiguration` are part of every
            // rbxoffsets dump; the fallbacks match the layout they have shipped
            // for a while so an older/partial dump still parses.
            data_model: DataModelOffsets {
                place_id: number(data_model, "PlaceId").unwrap_or(384),
                game_loaded: number(data_model, "GameLoaded").unwrap_or(1480),
                server_ip: number(data_model, "ServerIP").unwrap_or(1456),
            },
            chat: ChatOffsets {
                is_focused: number(chat, "IsFocused").unwrap_or(294),
            },
            camera: CameraOffsets {
                camera_subject: number(camera, "CameraSubject").unwrap_or(176),
                position: number(camera, "Position").unwrap_or(228),
                rotation: number(camera, "Rotation").unwrap_or(192),
                field_of_view: number(camera, "FieldOfView").unwrap_or(296),
                viewport_size: number(camera, "ViewportSize").unwrap_or(708),
            },
            workspace: WorkspaceOffsets {
                current_camera: number(workspace, "CurrentCamera").unwrap_or(1184),
                world: number(workspace, "World").unwrap_or(1016),
                distributed_game_time: number(workspace, "DistributedGameTime").unwrap_or(1216),
            },
            world: WorldOffsets {
                fallen_parts_destroy_height: number(world, "FallenPartsDestroyHeight")
                    .unwrap_or(532),
                gravity: number(world, "Gravity").unwrap_or(544),
            },
            scripts: ScriptOffsets::default(),
        })
    }
}

// ---------------------------------------------------------------------------
// Loading / cache
// ---------------------------------------------------------------------------

/// Load offsets for the currently running Roblox client, reusing the on-disk
/// cache when it is fresh and belongs to the same game version.
pub fn load(paths: &Paths) -> Result<Offsets> {
    let version = running_roblox_version()?;
    let now = unix_seconds();

    let cached = read_version_file(&paths.offsets_version_file);
    let version_changed = cached.version.as_deref() != Some(version.as_str());
    let expired = now - cached.last_download >= config::OFFSETS_REFRESH_SECS as f64;
    let cache_exists = paths.offsets_cache.is_file();

    if cache_exists && !version_changed && !expired {
        log::debug!(
            target: "GamingChairLLC.offsets",
            "Using cached offsets for {version}"
        );
        if let Some(offsets) = load_cached(paths) {
            return Ok(offsets);
        }
    }

    match download(&version) {
        Ok(root) => {
            write_cache(paths, &root, &version, now);
            Ok(root)
        }
        Err(err) => {
            if cache_exists && !version_changed {
                log::warn!(
                    target: "GamingChairLLC.offsets",
                    "Download failed ({err}); falling back to cached offsets"
                );
                if let Some(offsets) = load_cached(paths) {
                    return Ok(offsets);
                }
            }
            Err(err.context(format!(
                "failed to download offsets for {version} and no usable cache exists"
            )))
        }
    }
}

fn load_cached(paths: &Paths) -> Option<Offsets> {
    let text = fs::read_to_string(&paths.offsets_cache).ok()?;
    let value: Value = serde_json::from_str(&text).ok()?;
    match Offsets::parse(&value) {
        Ok(offsets) => Some(offsets),
        Err(err) => {
            log::warn!(
                target: "GamingChairLLC.offsets",
                "Cached offsets are invalid: {err}"
            );
            None
        }
    }
}

fn download(version: &str) -> Result<Offsets> {
    let version = if version.starts_with("version-") {
        version.to_string()
    } else {
        format!("version-{version}")
    };
    let url = format!(
        "{}/api/v1/{}/offsets/{version}/json",
        config::OFFSETS_BASE_URL,
        config::OFFSETS_PLATFORM
    );

    log::info!(target: "GamingChairLLC.offsets", "Downloading offsets from {url}");

    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(30))
        .build()
        .context("building HTTP client")?;
    let response = client
        .get(&url)
        .send()
        .and_then(|response| response.error_for_status())
        .context("requesting offsets")?;
    let mut data: Value = response.json().context("decoding offsets JSON")?;

    // rbxoffsets wraps the dump in { "Offsets": { ... }, "Roblox Version": ... }.
    if let Some(inner) = data.get("Offsets") {
        data = inner.clone();
    }

    Offsets::parse(&data)
}

fn write_cache(paths: &Paths, offsets: &Offsets, version: &str, now: f64) {
    let _ = fs::create_dir_all(&paths.data_dir);

    // Re-serialise the typed offsets so a cached file always has a stable shape.
    let value = serde_json::json!({
        "Instance": {
            "NameContainer": offsets.instance.name_container,
            "Name": offsets.instance.name,
            "ChildrenStart": offsets.instance.children_start,
            "ChildrenEnd": offsets.instance.children_end,
            "ClassDescriptor": offsets.instance.class_descriptor,
            "ClassName": offsets.instance.class_name,
        },
        "FakeDataModel": {
            "Pointer": offsets.fake_data_model.pointer,
            "RealDataModel": offsets.fake_data_model.real_data_model,
        },
        "Humanoid": {
            "WalkSpeed": offsets.humanoid.walk_speed,
            "Walkspeed": offsets.humanoid.walk_speed,
            "WalkspeedCheck": offsets.humanoid.walk_speed_check,
            "JumpPower": offsets.humanoid.jump_power,
            "Health": offsets.humanoid.health,
            "MaxHealth": offsets.humanoid.max_health,
            "HumanoidState": offsets.humanoid.state_ptr,
            "HumanoidStateID": offsets.humanoid.state_id,
        },
        "BasePart": {
            "Primitive": offsets.base_part.primitive,
            "Massless": offsets.base_part.massless,
        },
        "Primitive": {
            "Position": offsets.primitive.position,
            "Size": offsets.primitive.size,
            "Rotation": offsets.primitive.rotation,
            "Flags": offsets.primitive.flags,
            "AssemblyLinearVelocity": offsets.primitive.assembly_linear_velocity,
            "AssemblyAngularVelocity": offsets.primitive.assembly_angular_velocity,
            "Owner": offsets.primitive.owner,
        },
        "PrimitiveFlags": {
            "Anchored": offsets.primitive_flags.anchored,
            "CanCollide": offsets.primitive_flags.can_collide,
        },
        "Player": {
            "LocalPlayer": offsets.player.local_player,
            "ModelInstance": offsets.player.model_instance,
            "Character": offsets.player.character,
            "UserId": offsets.player.user_id,
        },
        "DataModel": {
            "PlaceId": offsets.data_model.place_id,
            "GameLoaded": offsets.data_model.game_loaded,
            "ServerIP": offsets.data_model.server_ip,
        },
        "ChatInputBarConfiguration": {
            "IsFocused": offsets.chat.is_focused,
        },
        "Camera": {
            "CameraSubject": offsets.camera.camera_subject,
            "Position": offsets.camera.position,
            "Rotation": offsets.camera.rotation,
            "FieldOfView": offsets.camera.field_of_view,
            "ViewportSize": offsets.camera.viewport_size,
        },
        "Workspace": {
            "CurrentCamera": offsets.workspace.current_camera,
            "World": offsets.workspace.world,
            "DistributedGameTime": offsets.workspace.distributed_game_time,
        },
        "World": {
            "FallenPartsDestroyHeight": offsets.world.fallen_parts_destroy_height,
            "Gravity": offsets.world.gravity,
        },
    });

    match serde_json::to_string_pretty(&value) {
        Ok(text) => {
            if let Err(err) = fs::write(&paths.offsets_cache, text) {
                log::warn!(
                    target: "GamingChairLLC.offsets",
                    "Could not write offsets cache: {err}"
                );
            }
        }
        Err(err) => log::warn!(
            target: "GamingChairLLC.offsets",
            "Could not serialise offsets cache: {err}"
        ),
    }

    let _ = fs::write(
        &paths.offsets_version_file,
        format!("{now}\n{version}"),
    );
}

struct CachedVersion {
    last_download: f64,
    version: Option<String>,
}

fn read_version_file(path: &Path) -> CachedVersion {
    let text = fs::read_to_string(path).unwrap_or_default();
    let mut lines = text.lines();
    let last_download = lines
        .next()
        .and_then(|line| line.trim().parse::<f64>().ok())
        .unwrap_or(0.0);
    let version = lines
        .next()
        .map(|line| line.trim().to_string())
        .filter(|line| !line.is_empty());

    CachedVersion {
        last_download,
        version,
    }
}

/// Folder name of the running Roblox install: `version-<hash>`.
pub fn running_roblox_version() -> Result<String> {
    let process = process::find_process(config::ROBLOX_PROCESS_NAME).ok_or_else(|| {
        anyhow!(
            "could not detect the Roblox version (is {} running?)",
            config::ROBLOX_PROCESS_NAME
        )
    })?;

    let folder = process
        .exe_path
        .as_deref()
        .and_then(Path::parent)
        .and_then(Path::file_name)
        .and_then(|name| name.to_str())
        .map(str::to_string)
        .filter(|name| name.starts_with("version-"));

    folder.ok_or_else(|| {
        anyhow!(
            "could not determine the Roblox install folder (is {} running?)",
            config::ROBLOX_PROCESS_NAME
        )
    })
}

fn unix_seconds() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs_f64()
}

fn number(section: Option<&Value>, key: &str) -> Option<u64> {
    section?.get(key)?.as_u64()
}

fn int_number(section: Option<&Value>, key: &str) -> Option<i32> {
    let value = section?.get(key)?.as_i64()?;
    i32::try_from(value).ok()
}

/// Absolute address of the process base + an offsets pointer.
pub fn absolute(base: u64, offset: u64) -> u64 {
    base.wrapping_add(offset)
}

#[cfg(test)]
mod tests {
    use super::Offsets;
    use serde_json::{json, Value};

    fn document() -> Value {
        json!({
            "Instance": {},
            "FakeDataModel": { "Pointer": 150312976, "RealDataModel": 504 },
            "Humanoid": {},
            "BasePart": {},
            "Primitive": {},
            "PrimitiveFlags": {},
            "Player": { "LocalPlayer": 280, "UserId": 184, "ModelInstance": 656 },
            "DataModel": { "PlaceId": 384, "GameLoaded": 1480, "ServerIP": 1456 },
            "ChatInputBarConfiguration": { "IsFocused": 294 },
            "Camera": {
                "CameraSubject": 176,
                "Position": 228,
                "Rotation": 192,
                "FieldOfView": 296,
                "ViewportSize": 708
            },
            "Workspace": {
                "CurrentCamera": 1184,
                "World": 1016,
                "DistributedGameTime": 1216
            },
            "World": { "FallenPartsDestroyHeight": 532, "Gravity": 544 }
        })
    }

    #[test]
    fn parses_game_and_chat_offsets() {
        let offsets = Offsets::parse(&document()).unwrap();

        assert_eq!(offsets.player.user_id, 184);
        assert_eq!(offsets.player.model_instance, 656);
        assert_eq!(offsets.data_model.place_id, 384);
        assert_eq!(offsets.data_model.game_loaded, 1480);
        assert_eq!(offsets.data_model.server_ip, 1456);
        assert_eq!(offsets.chat.is_focused, 294);
        assert_eq!(offsets.camera.rotation, 192);
        assert_eq!(offsets.workspace.current_camera, 1184);
        assert_eq!(offsets.world.fallen_parts_destroy_height, 532);
        assert_eq!(offsets.primitive.owner, 536);
        assert_eq!(offsets.primitive.assembly_angular_velocity, 236);
    }

    #[test]
    fn missing_sections_fall_back_to_the_known_layout() {
        let mut value = document();
        let object = value.as_object_mut().unwrap();
        object.remove("DataModel");
        object.remove("ChatInputBarConfiguration");
        object.remove("Camera");
        object.remove("Workspace");
        object.remove("World");

        let offsets = Offsets::parse(&value).unwrap();

        assert_eq!(offsets.data_model.place_id, 384);
        assert_eq!(offsets.data_model.game_loaded, 1480);
        assert_eq!(offsets.data_model.server_ip, 1456);
        assert_eq!(offsets.chat.is_focused, 294);
        assert_eq!(offsets.camera.rotation, 192);
        assert_eq!(offsets.workspace.world, 1016);
        assert_eq!(offsets.world.gravity, 544);
    }

    #[test]
    fn a_dump_without_the_data_model_pointers_is_rejected() {
        let mut value = document();
        value.as_object_mut().unwrap().remove("FakeDataModel");

        assert!(Offsets::parse(&value).is_err());
    }
}
