//! Everything an `Instance` needs to talk to the game. Sharing one context
//! keeps offsets and the memory handle in one place instead of threading them
//! through every constructor.

use std::sync::Arc;

use crate::sdk::memory::{Memory, MemoryFreezer};
use crate::sdk::offsets::Offsets;

pub struct GameContext {
    pub memory: Arc<Memory>,
    pub freezer: Arc<MemoryFreezer>,
    pub offsets: Arc<Offsets>,
}

impl GameContext {
    pub fn new(
        memory: Arc<Memory>,
        freezer: Arc<MemoryFreezer>,
        offsets: Arc<Offsets>,
    ) -> Arc<Self> {
        Arc::new(Self {
            memory,
            freezer,
            offsets,
        })
    }
}
