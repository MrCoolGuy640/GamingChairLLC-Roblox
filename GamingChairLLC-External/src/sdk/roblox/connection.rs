//! The live game connection: one process handle, one freezer, one offsets set.

use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};

use crate::config;
use crate::sdk::memory::{Memory, MemoryFreezer};
use crate::sdk::offsets::Offsets;
use crate::sdk::process;
use crate::sdk::roblox::context::GameContext;
use crate::sdk::roblox::humanoid::Humanoid;
use crate::sdk::roblox::instance::Instance;
use crate::sdk::roblox::part::Part;
use crate::sdk::roblox::player::Player;
use crate::sdk::roblox::utils;

/// How long attach waits for the game's DataModel to become readable.
const DATAMODEL_TIMEOUT: Duration = Duration::from_secs(10);
const ATTACH_RETRY: Duration = Duration::from_millis(200);

pub struct Roblox {
    ctx: Arc<GameContext>,
    pid: u32,
}

impl Roblox {
    /// Open a connection to the running client and wait until its DataModel can
    /// be resolved. Returns an error if the game is not running or never loads.
    pub fn connect(offsets: Arc<Offsets>) -> Result<Arc<Self>> {
        let deadline = Instant::now() + DATAMODEL_TIMEOUT;

        let process = loop {
            if let Some(process) = process::find_process(config::ROBLOX_PROCESS_NAME) {
                break process;
            }
            if Instant::now() >= deadline {
                bail!(
                    "{} is not running",
                    config::ROBLOX_PROCESS_NAME
                );
            }
            std::thread::sleep(ATTACH_RETRY);
        };

        let memory = Memory::attach(process.pid, config::ROBLOX_PROCESS_NAME)
            .context("opening the game process")?;
        let freezer = MemoryFreezer::start(Arc::clone(&memory));
        let ctx = GameContext::new(memory, freezer, offsets);

        let roblox = Arc::new(Self {
            ctx,
            pid: process.pid,
        });

        // The DataModel pointer is null while the client is still loading.
        loop {
            if roblox.try_datamodel().is_some() {
                break;
            }
            if Instant::now() >= deadline {
                bail!("the game DataModel did not become readable in time");
            }
            std::thread::sleep(ATTACH_RETRY);
        }

        log::info!(
            target: "GamingChairLLC.roblox",
            "Connected to {} (PID {})",
            config::ROBLOX_PROCESS_NAME,
            process.pid
        );

        Ok(roblox)
    }

    pub fn pid(&self) -> u32 {
        self.pid
    }

    pub fn context(&self) -> &Arc<GameContext> {
        &self.ctx
    }

    pub fn instance(&self, address: u64) -> Instance {
        Instance::new(address, Arc::clone(&self.ctx))
    }

    pub fn get_datamodel(&self) -> Result<Instance> {
        self.try_datamodel()
            .context("could not resolve the game DataModel")
    }

    fn try_datamodel(&self) -> Option<Instance> {
        let memory = &self.ctx.memory;
        let offsets = &self.ctx.offsets.fake_data_model;

        let fake = memory.read_ptr(
            memory
                .base_address()
                .wrapping_add(offsets.pointer),
        );
        if fake == 0 {
            return None;
        }

        let real = memory.read_ptr(fake.wrapping_add(offsets.real_data_model));
        (real != 0).then(|| self.instance(real))
    }

    pub fn get_workspace(&self) -> Option<Instance> {
        self.get_datamodel()
            .ok()
            .and_then(|datamodel| datamodel.find_first_child_of_name("Workspace"))
    }

    pub fn get_players(&self) -> Option<Instance> {
        self.get_datamodel()
            .ok()
            .and_then(|datamodel| datamodel.find_first_child_of_name("Players"))
    }

    pub fn get_local_player(&self) -> Option<Player> {
        utils::get_local_player(&self.get_datamodel().ok()?)
    }

    pub fn get_local_character(&self) -> Option<Instance> {
        utils::get_local_character(&self.get_datamodel().ok()?)
    }

    pub fn get_local_humanoid(&self) -> Option<Humanoid> {
        utils::get_local_humanoid(&self.get_datamodel().ok()?)
    }

    pub fn get_local_root_part(&self) -> Option<Part> {
        utils::get_local_root_part(&self.get_datamodel().ok()?)
    }

    // -- game / player metadata -----------------------------------------

    /// `Player.UserId` of the local player. `0` while there is no character
    /// (menus, loading) or if the offset is unavailable.
    pub fn user_id(&self) -> i64 {
        self.get_local_player()
            .map(|player| player.user_id())
            .unwrap_or(0)
    }

    /// `DataModel.PlaceId`.
    pub fn place_id(&self) -> i64 {
        let Some(datamodel) = self.get_datamodel().ok() else {
            return 0;
        };

        datamodel.read_long(self.ctx.offsets.data_model.place_id)
    }

    /// `DataModel.GameLoaded`: true once the client finished loading the place.
    pub fn game_loaded(&self) -> bool {
        let Some(datamodel) = self.get_datamodel().ok() else {
            return false;
        };

        datamodel.read_bool(self.ctx.offsets.data_model.game_loaded)
    }

    /// `DataModel.ServerIP` as a string (empty when unavailable).
    pub fn server_ip(&self) -> String {
        let Some(datamodel) = self.get_datamodel().ok() else {
            return String::new();
        };

        datamodel.read_string_at(
            datamodel
                .address()
                .wrapping_add(self.ctx.offsets.data_model.server_ip),
        )
    }

    /// Whether the in-game chat input bar currently has keyboard focus.
    ///
    /// `TextChatService.ChatInputBarConfiguration.IsFocused` is read straight
    /// from memory. Missing service/configuration (older games, main menu)
    /// counts as "not focused".
    pub fn is_chat_focused(&self) -> bool {
        let Some(datamodel) = self.get_datamodel().ok() else {
            return false;
        };
        let Some(service) = datamodel.find_first_child_of_name("TextChatService") else {
            return false;
        };

        let bar = service
            .find_first_child_of_class("ChatInputBarConfiguration")
            .or_else(|| service.find_first_child_of_name("ChatInputBarConfiguration"))
            .or_else(|| service.find_first_descendant("ChatInputBarConfiguration"));

        let Some(bar) = bar else {
            return false;
        };

        bar.read_bool(self.ctx.offsets.chat.is_focused)
    }
}

impl Drop for Roblox {
    fn drop(&mut self) {
        self.ctx.freezer.stop();
        log::debug!(
            target: "GamingChairLLC.roblox",
            "Connection to PID {} closed",
            self.pid
        );
    }
}
