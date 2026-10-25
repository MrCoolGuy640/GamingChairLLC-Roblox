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
