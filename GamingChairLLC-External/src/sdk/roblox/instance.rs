//! `Instance`: the base object every Roblox object lives on. All reads are
//! direct memory reads; a failed read yields an empty value rather than an
//! error, matching the Python SDK's forgiving helpers.

use std::sync::Arc;

use crate::sdk::roblox::context::GameContext;

/// `std::string` stores its size at +0x18 in the layout Roblox ships. This is
/// what the Python SDK reads, and a length of 0 is handled as "read until NUL".
const STRING_LENGTH_OFFSET: u64 = 0x18;
const CHILD_STRIDE: u64 = 0x10;
/// Defensive bound: a corrupt children list must never spin forever.
const MAX_CHILDREN: usize = 100_000;

#[derive(Clone)]
pub struct Instance {
    address: u64,
    ctx: Arc<GameContext>,
}

impl Instance {
    pub fn new(address: u64, ctx: Arc<GameContext>) -> Self {
        Self { address, ctx }
    }

    pub fn address(&self) -> u64 {
        self.address
    }

    pub fn ctx(&self) -> &Arc<GameContext> {
        &self.ctx
    }

    pub fn write_bool(&self, offset: u64, value: bool) -> bool {
        self.ctx
            .memory
            .write_bytes(self.address + offset, &[u8::from(value)])
    }

    // -- nametag ---------------------------------------------------------

    pub fn get_name(&self) -> String {
        let offsets = &self.ctx.offsets.instance;
        let container = self
            .ctx
            .memory
            .read_ptr(self.address.wrapping_add(offsets.name_container));

        if container == 0 {
            return String::new();
        }

        self.fetch_string(container.wrapping_add(offsets.name))
    }

    pub fn get_class_name(&self) -> String {
        let offsets = &self.ctx.offsets.instance;
        let descriptor = self
            .ctx
            .memory
            .read_ptr(self.address.wrapping_add(offsets.class_descriptor));

        if descriptor == 0 {
            return String::new();
        }

        let class_name = self
            .ctx
            .memory
            .read_ptr(descriptor.wrapping_add(offsets.class_name));

        self.fetch_string(class_name)
    }

    // -- hierarchy -------------------------------------------------------

    pub fn get_children(&self) -> Vec<Instance> {
        let offsets = &self.ctx.offsets.instance;
        let memory = &self.ctx.memory;

        let list_start = memory.read_ptr(self.address.wrapping_add(offsets.children_start));
        if list_start == 0 {
            return Vec::new();
        }

        let list_end = memory.read_ptr(list_start.wrapping_add(offsets.children_end));
        let mut current = memory.read_ptr(list_start);

        let mut children = Vec::new();
        while current > 0 && current < list_end {
            let address = memory.read_ptr(current);
            if address > 0 {
                children.push(Instance::new(address, Arc::clone(&self.ctx)));
                if children.len() >= MAX_CHILDREN {
                    break;
                }
            }
            current = current.wrapping_add(CHILD_STRIDE);
        }

        children
    }

    pub fn find_first_child(&self) -> Option<Instance> {
        self.get_children().into_iter().next()
    }

    pub fn find_first_child_of_name(&self, name: &str) -> Option<Instance> {
        self.get_children()
            .into_iter()
            .find(|child| child.get_name() == name)
    }

    pub fn find_first_child_of_class(&self, class_name: &str) -> Option<Instance> {
        self.get_children()
            .into_iter()
            .find(|child| child.get_class_name() == class_name)
    }

    /// Walk a dotted path such as `Workspace.CurrentCamera`.
    pub fn find_first_child_from_path(&self, path: &str) -> Option<Instance> {
        let mut current = self.clone();

        for part in path.split('.') {
            let part = part.trim();
            if part.is_empty() {
                return None;
            }
            current = current.find_first_child_of_name(part)?;
        }

        Some(current)
    }

    pub fn find_first_descendant(&self, name: &str) -> Option<Instance> {
        for child in self.get_children() {
            if child.get_name() == name {
                return Some(child);
            }
            if let Some(found) = child.find_first_descendant(name) {
                return Some(found);
            }
        }
        None
    }

    // -- humanoid state --------------------------------------------------

    pub fn get_humanoid_state_id(&self) -> Option<i32> {
        let offsets = &self.ctx.offsets.humanoid;
        let state = self
            .ctx
            .memory
            .read_ptr(self.address.wrapping_add(offsets.state_ptr));

        if state == 0 {
            return None;
        }

        Some(
            self.ctx
                .memory
                .read_int(state.wrapping_add(offsets.state_id)),
        )
    }

    pub fn set_humanoid_state_id(&self, state_id: i32) -> bool {
        let offsets = &self.ctx.offsets.humanoid;
        let state = self
            .ctx
            .memory
            .read_ptr(self.address.wrapping_add(offsets.state_ptr));

        if state == 0 {
            return false;
        }

        self.ctx
            .memory
            .write_int(state.wrapping_add(offsets.state_id), state_id)
    }

    // -- internals -------------------------------------------------------

    fn fetch_string(&self, address: u64) -> String {
        if address == 0 {
            return String::new();
        }

        let length = self
            .ctx
            .memory
            .read_int(address.wrapping_add(STRING_LENGTH_OFFSET));

        if length >= 16 {
            let pointer = address; // the inline buffer *is* the pointer slot
            let string_pointer = self.ctx.memory.read_ptr(pointer);
            if string_pointer == 0 {
                return String::new();
            }
            self.ctx
                .memory
                .read_string(string_pointer, length as usize)
        } else if length > 0 {
            self.ctx
                .memory
                .read_string(address, length as usize)
        } else {
            String::new()
        }
    }
}

impl std::fmt::Debug for Instance {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "<Instance address={:#x}>", self.address)
    }
}
