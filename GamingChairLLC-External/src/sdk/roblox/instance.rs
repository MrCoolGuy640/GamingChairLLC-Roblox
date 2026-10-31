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

    pub fn write_float(&self, offset: u64, value: f32) -> bool {
        self.ctx
            .memory
            .write_bytes(self.address + offset, &value.to_le_bytes())
    }

    // -- field reads -----------------------------------------------------

    /// `i64`/pointer-sized field at `offset` from this instance's address.
    pub fn read_long(&self, offset: u64) -> i64 {
        self.ctx
            .memory
            .read_long(self.address.wrapping_add(offset))
    }

    pub fn read_bool(&self, offset: u64) -> bool {
        self.ctx
            .memory
            .read_bool(self.address.wrapping_add(offset))
    }

    /// Read a `std::string` that lives at `address` (not at an offset). Used by
    /// offsets such as `DataModel.ServerIP` that point straight at the string.
    pub fn read_string_at(&self, address: u64) -> String {
        self.fetch_string(address)
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

    /// Set the instance's `Name`.
    ///
    /// This is the write-side counterpart to [`Instance::get_name`] and a port
    /// of the Python SDK: allocate a fresh 0x20-byte `rbx_string` struct in the
    /// target, point `NameContainer` at it, then fill the struct. Inline
    /// strings (<= 15 bytes) live in the struct itself; longer strings get a
    /// separate heap buffer and a data pointer.
    ///
    /// The engine does not own the block `NameContainer` points at afterwards,
    /// so destroying this instance makes it free foreign memory. This matches
    /// the Python behaviour and is only safe for instances whose lifetime the
    /// caller controls.
    pub fn set_name(&self, name: &str) -> bool {
        let offsets = &self.ctx.offsets.instance;
        let allocated = self.ctx.memory.allocate(0x20);
        if allocated == 0 {
            return false;
        }

        if !self
            .ctx
            .memory
            .write_ptr(self.address.wrapping_add(offsets.name_container), allocated)
        {
            return false;
        }

        self.write_string(allocated, name)
    }

    fn write_string(&self, address: u64, value: &str) -> bool {
        let encoded = value.as_bytes();
        let length = encoded.len();

        if length > 15 {
            let existing = self.ctx.memory.read_bytes(address, 0x20);
            let (mut capacity, mut data_pointer) = if existing.len() >= 0x20 {
                (
                    u64::from_le_bytes(existing[0x18..0x20].try_into().unwrap_or([0; 8])),
                    u64::from_le_bytes(existing[0..8].try_into().unwrap_or([0; 8])),
                )
            } else {
                (0, 0)
            };

            while (length as u64) > capacity {
                capacity = capacity * 2 + 1;
                if capacity > 64 * 1024 * 1024 {
                    return false;
                }
            }

            if data_pointer == 0 {
                data_pointer = self.ctx.memory.allocate(capacity as usize);
                if data_pointer == 0 {
                    return false;
                }
            }

            self.ctx
                .memory
                .write_ptr(address, data_pointer)
                && self
                    .ctx
                    .memory
                    .write_bytes(address + 0x10, &(length as u64).to_le_bytes())
                && self
                    .ctx
                    .memory
                    .write_bytes(address + 0x18, &capacity.to_le_bytes())
                && self.ctx.memory.write_bytes(data_pointer, encoded)
        } else {
            self.ctx
                .memory
                .write_bytes(address + 0x10, &(length as u64).to_le_bytes())
                && self
                    .ctx
                    .memory
                    .write_bytes(address + 0x18, &15u64.to_le_bytes())
                && self.ctx.memory.write_bytes(address, encoded)
        }
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

    /// Raw `Instance.Parent` pointer. `0` means the instance is unparented.
    pub fn parent_address(&self) -> u64 {
        self.ctx
            .memory
            .read_ptr(self.address.wrapping_add(self.ctx.offsets.instance.parent))
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
