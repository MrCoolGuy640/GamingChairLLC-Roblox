//! External `Instance.new`, ported from the Python implementation.
//!
//! The engine's own reflection data is used to resolve a class name:
//!
//! * name registry -> interned `RBX::Name*`
//! * creator table (`Name*` -> creator object)
//! * creator vtable (`create` / `isCreatable`)
//!
//! `create` and `isCreatable` run on an engine thread through
//! [`CallGate`](super::call_gate::CallGate), because the engine allocator and
//! name interning must not be driven from a foreign thread. `Parent = x` uses
//! the discovered typed setter instead of editing the tree.
//!
//! The reflection offsets are build-specific. They are embedded from
//! `instance_new_offsets.json` (the output of the Python dumper) and validated
//! against the live PE fingerprint before anything is written, so a stale dump
//! fails closed instead of corrupting the client.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{anyhow, bail, Result};
use parking_lot::Mutex;
use serde::Deserialize;

use crate::sdk::memory::Memory;
use crate::sdk::roblox::call_gate::CallGate;
use crate::sdk::roblox::context::GameContext;
use crate::sdk::roblox::instance::Instance;

/// The dumper output for the build these offsets were discovered on. Replace
/// the file and rebuild when Roblox updates.
const DUMP_JSON: &str = include_str!("instance_new_offsets.json");

/// Methods the engine calls constantly. The first descriptor whose impl slot is
/// observed being called becomes the gate.
const HOT_METHODS: &[&str] = &[
    "FindFirstChild",
    "GetChildren",
    "IsA",
    "WaitForChild",
    "FindFirstChildOfClass",
    "FindFirstChildWhichIsA",
    "GetDescendants",
    "Clone",
    "IsDescendantOf",
];

/// Typed `Parent` setter slot inside the property descriptor vtable.
const PARENT_SET_SLOT: u64 = 0x108;

/// Header offsets inside a reflection table (stable across builds).
const TABLE_START: u64 = 0x00;
const TABLE_END: u64 = 0x08;
const TABLE_EMPTY: u64 = 0x20;

const MAX_TABLE_BYTES: u64 = 0x80_0000;
const SCAN_CHUNK: usize = 8 * 1024 * 1024;
const LISTEN_DWELL: Duration = Duration::from_millis(750);
const INVOKE_TIMEOUT: Duration = Duration::from_secs(5);
/// A failed gate discovery is retried after this long. Without it the ESP would
/// pay the (slow) listening pass on every tick while the client sits in a menu.
const GATE_COOLDOWN: Duration = Duration::from_secs(15);
/// Defensive bound on the fallback descriptor scan: `.data` holds a lot of
/// `{module vtable, heap pointer}` pairs and only a few are method descriptors.
const MAX_CANDIDATE_NAMES: usize = 65_536;

// ---------------------------------------------------------------------------
// Build dump
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Deserialize)]
struct DumpFile {
    build: DumpBuild,
    #[serde(rename = "InstanceNew")]
    instance_new: DumpOffsets,
}

#[derive(Debug, Clone, Copy, Deserialize)]
struct DumpBuild {
    timestamp: u32,
    entry_point: u32,
    size_of_image: u32,
    checksum: u32,
}

#[derive(Debug, Clone, Deserialize)]
struct DumpOffsets {
    #[serde(rename = "Creator_create")]
    creator_create: u64,
    #[serde(rename = "Creator_isCreatable")]
    creator_is_creatable: u64,
    #[serde(rename = "CreatorTable")]
    creator_table: u64,
    #[serde(rename = "NameRegistry")]
    name_registry: u64,
    #[serde(rename = "NameTable")]
    name_table: u64,
    #[serde(rename = "EntryValue")]
    entry_value: u64,
    #[serde(rename = "EntryStride")]
    entry_stride: u64,
    #[serde(rename = "Name_Size")]
    name_size: u64,
    #[serde(rename = "Name_Capacity")]
    name_capacity: u64,
    #[serde(rename = "Name_InlineCapacity")]
    name_inline_capacity: u64,
    #[serde(rename = "ParentDesc")]
    parent_desc: u64,
    #[serde(rename = "ParentSetter")]
    parent_setter: u64,
}

// ---------------------------------------------------------------------------
// Module image
// ---------------------------------------------------------------------------

struct Section {
    start: u64,
    end: u64,
    characteristics: u32,
}

struct ImageInfo {
    base: u64,
    size: u64,
    timestamp: u32,
    entry_point: u32,
    checksum: u32,
    sections: Vec<Section>,
}

impl ImageInfo {
    fn contains(&self, value: u64) -> bool {
        self.base <= value && value < self.base.wrapping_add(self.size)
    }

    fn section_of(&self, value: u64) -> Option<&Section> {
        let rva = value.checked_sub(self.base)?;
        self.sections
            .iter()
            .find(|section| section.start <= rva && rva < section.end)
    }

    fn is_exec(&self, value: u64) -> bool {
        self.section_of(value)
            .map(|section| section.characteristics & 0x2000_0000 != 0)
            .unwrap_or(false)
    }

    fn is_heap(&self, value: u64) -> bool {
        0x10000 <= value && value < 0x7FFF_FFFF_FFFF && !self.contains(value)
    }

    fn data_sections(&self) -> impl Iterator<Item = &Section> {
        self.sections
            .iter()
            .filter(|section| section.characteristics & 0x2000_0000 == 0)
    }
}

fn read_image(memory: &Memory) -> Result<ImageInfo> {
    let base = memory.base_address();

    let dos = memory.read_bytes(base, 0x40);
    if dos.len() < 0x40 || &dos[0..2] != b"MZ" {
        bail!("could not read the client's DOS header");
    }

    let pe = base.wrapping_add(u32::from_le_bytes(
        dos[0x3C..0x40].try_into().unwrap_or([0; 4]),
    ) as u64);

    let header = memory.read_bytes(pe, 0x18);
    if header.len() < 0x18 || &header[0..4] != b"PE\0\0" {
        bail!("could not read the client's PE header");
    }

    let timestamp = u32::from_le_bytes(header[8..12].try_into().unwrap_or([0; 4]));
    let section_count = u16::from_le_bytes(header[6..8].try_into().unwrap_or([0; 2])) as usize;
    let optional_size =
        u16::from_le_bytes(header[20..22].try_into().unwrap_or([0; 2])) as u64;

    let optional = memory.read_bytes(pe.wrapping_add(24), 0x44);
    if optional.len() < 0x44 {
        bail!("could not read the client's optional header");
    }

    let entry_point = u32::from_le_bytes(optional[0x10..0x14].try_into().unwrap_or([0; 4]));
    let size = u32::from_le_bytes(optional[0x38..0x3C].try_into().unwrap_or([0; 4])) as u64;
    let checksum = u32::from_le_bytes(optional[0x40..0x44].try_into().unwrap_or([0; 4]));
    if size == 0 || section_count == 0 {
        bail!("the client's PE headers look incomplete");
    }

    let table = memory.read_bytes(
        pe.wrapping_add(24).wrapping_add(optional_size),
        section_count * 40,
    );
    if table.len() < section_count * 40 {
        bail!("could not read the client's section table");
    }

    let mut sections = Vec::with_capacity(section_count);
    for index in 0..section_count {
        let entry = index * 40;
        let virtual_size =
            u32::from_le_bytes(table[entry + 8..entry + 12].try_into().unwrap_or([0; 4])) as u64;
        let virtual_address =
            u32::from_le_bytes(table[entry + 12..entry + 16].try_into().unwrap_or([0; 4])) as u64;
        let raw_size =
            u32::from_le_bytes(table[entry + 16..entry + 20].try_into().unwrap_or([0; 4])) as u64;
        let characteristics =
            u32::from_le_bytes(table[entry + 36..entry + 40].try_into().unwrap_or([0; 4]));

        sections.push(Section {
            start: virtual_address,
            end: virtual_address
                .wrapping_add(virtual_size.max(raw_size))
                .min(size),
            characteristics,
        });
    }

    Ok(ImageInfo {
        base,
        size,
        timestamp,
        entry_point,
        checksum,
        sections,
    })
}

// ---------------------------------------------------------------------------
// Reflection tables
// ---------------------------------------------------------------------------

struct Table {
    start: u64,
    end: u64,
    empty: u64,
}

impl Table {
    fn span(&self) -> u64 {
        self.end.saturating_sub(self.start)
    }
}

fn u64_at(blob: &[u8], offset: usize) -> u64 {
    u64::from_le_bytes(blob[offset..offset + 8].try_into().unwrap_or([0; 8]))
}

fn read_table(
    memory: &Memory,
    image: &ImageInfo,
    dump: &DumpOffsets,
    at: u64,
    require_empty: bool,
) -> Result<Table> {
    let blob = memory.read_bytes(at, 0x28);
    if blob.len() < TABLE_EMPTY as usize + 8 {
        bail!("table header at {at:#x} is unreadable");
    }

    let table = Table {
        start: u64_at(&blob, TABLE_START as usize),
        end: u64_at(&blob, TABLE_END as usize),
        empty: u64_at(&blob, TABLE_EMPTY as usize),
    };

    let stride = dump.entry_stride;
    if stride == 0 || !image.is_heap(table.start) || !image.is_heap(table.end) {
        bail!(
            "table at {at:#x} does not point into the heap (start {:#x}, end {:#x})",
            table.start,
            table.end
        );
    }

    let span = table.span();
    if table.start >= table.end
        || span % stride != 0
        || span < stride.saturating_mul(8)
        || span > MAX_TABLE_BYTES
    {
        bail!("table at {at:#x} has an implausible span {span:#x}");
    }

    if require_empty && !(table.start <= table.empty && table.empty <= table.end) {
        bail!(
            "table at {at:#x} has an out-of-range empty sentinel {:#x}",
            table.empty
        );
    }

    Ok(table)
}

fn read_span(memory: &Memory, start: u64, length: u64) -> Result<Vec<u8>> {
    let mut data = Vec::with_capacity(length.min(SCAN_CHUNK as u64) as usize);
    let mut offset = 0u64;

    while offset < length {
        let take = (length - offset).min(SCAN_CHUNK as u64) as usize;
        let chunk = memory.read_bytes(start.wrapping_add(offset), take);
        if chunk.is_empty() {
            bail!("failed to read {take} bytes at {:#x}", start + offset);
        }

        let complete = chunk.len() == take;
        data.extend_from_slice(&chunk);
        if !complete {
            break;
        }

        offset += take as u64;
    }

    Ok(data)
}

fn name_text(
    memory: &Memory,
    image: &ImageInfo,
    dump: &DumpOffsets,
    address: u64,
) -> Option<String> {
    if address == 0 {
        return None;
    }

    let blob = memory.read_bytes(address, (dump.name_capacity + 8) as usize);
    if blob.len() < (dump.name_capacity + 8) as usize {
        return None;
    }

    let size = u64_at(&blob, dump.name_size as usize);
    let capacity = u64_at(&blob, dump.name_capacity as usize);

    if !(size > 0 && size < 256 && capacity >= dump.name_inline_capacity && size <= capacity) {
        return None;
    }

    let raw = if capacity == dump.name_inline_capacity {
        blob[..size as usize].to_vec()
    } else {
        let pointer = u64_at(&blob, 0);
        if !image.is_heap(pointer) {
            return None;
        }
        memory.read_bytes(pointer, size as usize)
    };

    if raw.len() != size as usize || !raw.iter().all(|byte| (32..127).contains(byte)) {
        return None;
    }

    String::from_utf8(raw).ok()
}

fn build_name_index(
    memory: &Memory,
    image: &ImageInfo,
    dump: &DumpOffsets,
) -> Result<HashMap<String, u64>> {
    let registry = memory.read_ptr(image.base.wrapping_add(dump.name_registry));
    if registry == 0 {
        bail!("the NameRegistry global is null");
    }

    let table = read_table(
        memory,
        image,
        dump,
        registry.wrapping_add(dump.name_table),
        true,
    )?;
    let blob = read_span(memory, table.start, table.span())?;

    let stride = dump.entry_stride as usize;
    if stride < 16 {
        bail!("entry stride {stride} is too small");
    }

    let mut index = HashMap::new();
    let mut offset = 0usize;
    while offset + stride <= blob.len() {
        let key = u64_at(&blob, offset);
        let value = u64_at(&blob, offset + dump.entry_value as usize);

        if key != 0 && key != table.empty {
            if let Some(text) = name_text(memory, image, dump, key) {
                if !text.is_empty() {
                    index.entry(text).or_insert(value);
                }
            }
        }

        offset += stride;
    }

    log::debug!(
        target: "GamingChairLLC.instance_new",
        "Resolved {} interned names",
        index.len()
    );

    Ok(index)
}

fn creator_table(memory: &Memory, image: &ImageInfo, dump: &DumpOffsets) -> Result<Table> {
    // The creator table does not use the +0x20 empty sentinel.
    read_table(
        memory,
        image,
        dump,
        image.base.wrapping_add(dump.creator_table),
        false,
    )
}

// ---------------------------------------------------------------------------
// Method descriptors
// ---------------------------------------------------------------------------

#[derive(Clone)]
struct DescriptorHit {
    descriptor: u64,
    text: &'static str,
}

fn scan_descriptors(
    memory: &Memory,
    image: &ImageInfo,
    dump: &DumpOffsets,
    names: &HashMap<String, u64>,
) -> Result<Vec<DescriptorHit>> {
    let mut targets: HashMap<u64, &'static str> = HashMap::new();
    for method in HOT_METHODS {
        if let Some(pointer) = names.get(*method) {
            if *pointer != 0 {
                targets.insert(*pointer, *method);
            }
        }
    }

    if targets.is_empty() {
        bail!("none of the hot method names are interned");
    }

    let stride = dump.entry_stride as usize;
    if stride < 16 {
        bail!("entry stride {stride} is too small");
    }

    let mut hits: Vec<DescriptorHit> = Vec::new();
    let mut seen_descriptors: HashSet<u64> = HashSet::new();
    let mut found: HashSet<&'static str> = HashSet::new();

    // Fallback data for names that are not the registry pointer we expect.
    let mut candidate_names: HashSet<u64> = HashSet::new();
    let mut candidate_descriptors: HashMap<u64, Vec<u64>> = HashMap::new();

    for section in image.data_sections() {
        let length = section.end.saturating_sub(section.start);
        if length < 16 {
            continue;
        }

        let mut offset = 0u64;
        while offset + 16 <= length {
            let take = (length - offset).min(SCAN_CHUNK as u64) as usize;
            let chunk = memory.read_bytes(image.base.wrapping_add(section.start + offset), take);
            if chunk.len() < 16 {
                offset += take as u64;
                continue;
            }

            // Keep the 8-byte scan aligned in absolute address space.
            let misplaced = ((image.base + section.start + offset) % 8) as usize;
            let mut position = if misplaced == 0 { 0 } else { 8 - misplaced };

            while position + 16 <= chunk.len() {
                let vtable = u64_at(&chunk, position);
                let name = u64_at(&chunk, position + 8);
                let descriptor = image.base + section.start + offset + position as u64;

                if image.contains(vtable) && !image.is_exec(vtable) {
                    if let Some(text) = targets.get(&name) {
                        if seen_descriptors.insert(descriptor) {
                            hits.push(DescriptorHit {
                                descriptor,
                                text: *text,
                            });
                            found.insert(*text);
                        }
                    } else if image.is_heap(name)
                        && (candidate_names.len() < MAX_CANDIDATE_NAMES
                            || candidate_names.contains(&name))
                    {
                        candidate_names.insert(name);
                        candidate_descriptors
                            .entry(name)
                            .or_default()
                            .push(descriptor);
                    }
                }

                position += 8;
            }

            offset += take as u64;
        }
    }

    // If a method was not found through the registry pointer, decode the
    // remaining candidate names. This is slower, so it only runs when needed.
    if found.len() < targets.len() {
        for name_pointer in candidate_names {
            let Some(text) = name_text(memory, image, dump, name_pointer) else {
                continue;
            };
            let Some(method) = HOT_METHODS.iter().copied().find(|method| *method == text) else {
                continue;
            };
            if found.contains(method) {
                continue;
            }

            if let Some(descriptors) = candidate_descriptors.get(&name_pointer) {
                for descriptor in descriptors {
                    if seen_descriptors.insert(*descriptor) {
                        hits.push(DescriptorHit {
                            descriptor: *descriptor,
                            text: method,
                        });
                    }
                }
            }

            found.insert(method);
            if found.len() == targets.len() {
                break;
            }
        }
    }

    if hits.is_empty() {
        bail!("no hot method descriptors were found in module data");
    }

    Ok(hits)
}

// ---------------------------------------------------------------------------
// Runtime
// ---------------------------------------------------------------------------

pub struct InstanceNew {
    ctx: Arc<GameContext>,
    dump: Option<DumpFile>,
    image: Mutex<Option<Arc<ImageInfo>>>,
    name_index: Mutex<Option<Arc<HashMap<String, u64>>>>,
    descriptors: Mutex<Option<Arc<Vec<DescriptorHit>>>>,
    gate: Mutex<Option<CallGate>>,
    gate_failed_at: Mutex<Option<Instant>>,
}

impl InstanceNew {
    pub fn new(ctx: Arc<GameContext>) -> Self {
        let dump = match serde_json::from_str::<DumpFile>(DUMP_JSON) {
            Ok(dump) => Some(dump),
            Err(err) => {
                log::error!(
                    target: "GamingChairLLC.instance_new",
                    "Embedded instance_new offsets are invalid: {err}"
                );
                None
            }
        };

        Self {
            ctx,
            dump,
            image: Mutex::new(None),
            name_index: Mutex::new(None),
            descriptors: Mutex::new(None),
            gate: Mutex::new(None),
            gate_failed_at: Mutex::new(None),
        }
    }

    /// `Instance.new(class_name)` on an engine thread.
    pub fn new_instance(&self, class_name: &str) -> Result<Instance> {
        let dump = self.dump_offsets()?.clone();
        let image = self.ensure_image()?;
        let memory = Arc::clone(&self.ctx.memory);

        let names = self.name_index()?;
        let name_pointer = names.get(class_name).copied().unwrap_or(0);
        let creator = if name_pointer != 0 {
            self.creator_by_name_ptr(&image, &dump, name_pointer)?
        } else {
            0
        };
        let creator = if creator == 0 {
            self.creator_by_text(&image, &dump, class_name)?
        } else {
            creator
        };

        if creator == 0 {
            bail!("no creator for class '{class_name}'");
        }
        if !image.contains(creator) {
            bail!("creator {creator:#x} for '{class_name}' is outside the module");
        }

        let vtable = memory.read_ptr(creator);
        if !image.contains(vtable) || image.is_exec(vtable) {
            bail!("creator {creator:#x} for '{class_name}' has an implausible vtable");
        }

        let create_fn = memory.read_ptr(vtable.wrapping_add(dump.creator_create));
        let creatable_fn = memory.read_ptr(vtable.wrapping_add(dump.creator_is_creatable));
        if !image.is_exec(create_fn) || !image.is_exec(creatable_fn) {
            bail!("creator {creator:#x} for '{class_name}' has no executable create slots");
        }

        self.ensure_gate()?;
        let mut guard = self.gate.lock();
        let gate = guard
            .as_mut()
            .ok_or_else(|| anyhow!("instance_new gate disappeared"))?;

        let (ok, creatable) = gate.invoke(creatable_fn, creator, 0, 0, 0, 0, INVOKE_TIMEOUT);
        if !ok {
            bail!("isCreatable call failed: {}", gate.last_failure());
        }
        if creatable & 0xFF == 0 {
            bail!("class '{class_name}' is not creatable");
        }

        let scratch = gate.scratch();
        memory.write_bytes(scratch, &[0u8; 16]);

        let (ok, returned) = gate.invoke(create_fn, creator, scratch, 0, 0, 0, INVOKE_TIMEOUT);
        if !ok {
            bail!("create call failed: {}", gate.last_failure());
        }

        let address = memory.read_ptr(scratch);
        if address == 0 {
            bail!("create returned no instance (ret {returned:#x})");
        }

        let instance = Instance::new(address, Arc::clone(&self.ctx));
        let actual = instance.get_class_name();
        if !actual.is_empty() && actual != class_name {
            log::warn!(
                target: "GamingChairLLC.instance_new",
                "Class read-back is '{actual}', expected '{class_name}'"
            );
        }

        Ok(instance)
    }

    /// `Instance.Parent = parent` through the engine's typed setter.
    pub fn set_parent(&self, instance: &Instance, parent: Option<&Instance>) -> Result<()> {
        let dump = self.dump_offsets()?.clone();
        let image = self.ensure_image()?;
        let memory = &self.ctx.memory;

        let descriptor = image.base.wrapping_add(dump.parent_desc);
        let setter = image.base.wrapping_add(dump.parent_setter);

        let name_pointer = memory.read_ptr(descriptor.wrapping_add(8));
        if name_pointer != 0 {
            if let Some(text) = name_text(memory, &image, &dump, name_pointer) {
                if text != "Parent" {
                    bail!("ParentDesc no longer points at a Parent descriptor (found '{text}')");
                }
            }
        }

        let vtable = memory.read_ptr(descriptor);
        let slot = memory.read_ptr(vtable.wrapping_add(PARENT_SET_SLOT));
        if slot != setter {
            bail!(
                "Parent setter moved: vtable+{PARENT_SET_SLOT:#x} = {slot:#x}, dump says {setter:#x}"
            );
        }

        self.ensure_gate()?;
        let mut guard = self.gate.lock();
        let gate = guard
            .as_mut()
            .ok_or_else(|| anyhow!("instance_new gate disappeared"))?;

        let parent_address = parent.map(Instance::address).unwrap_or(0);
        let (ok, _) = gate.invoke(
            setter,
            descriptor,
            instance.address(),
            parent_address,
            0,
            0,
            INVOKE_TIMEOUT,
        );
        if !ok {
            bail!("Parent setter call failed: {}", gate.last_failure());
        }

        Ok(())
    }

    /// Unparent an instance (`Parent = nil`).
    pub fn unparent(&self, instance: &Instance) -> Result<()> {
        self.set_parent(instance, None)
    }

    /// Whether the call gate is installed and ready to run commands. Callers
    /// that only need to undo work should check this instead of triggering a
    /// (slow) gate discovery pass.
    pub fn is_ready(&self) -> bool {
        self.gate.lock().is_some()
    }

    /// Restore the descriptor slot and free the gate's pages.
    pub fn shutdown(&self) {
        if let Some(mut gate) = self.gate.lock().take() {
            gate.remove();
        }
    }

    // -- reflection ------------------------------------------------------

    fn dump_offsets(&self) -> Result<&DumpOffsets> {
        self.dump
            .as_ref()
            .map(|dump| &dump.instance_new)
            .ok_or_else(|| anyhow!("instance_new offsets are not available"))
    }

    fn ensure_image(&self) -> Result<Arc<ImageInfo>> {
        {
            let cached = self.image.lock();
            if let Some(image) = cached.as_ref() {
                return Ok(Arc::clone(image));
            }
        }

        let image = Arc::new(read_image(&self.ctx.memory)?);

        if let Some(dump) = &self.dump {
            if image.timestamp != dump.build.timestamp
                || image.size != dump.build.size_of_image as u64
                || image.entry_point != dump.build.entry_point
                || image.checksum != dump.build.checksum
            {
                bail!(
                    "instance_new offsets are for build timestamp {} / image size {} / entry {} / \
                     checksum {}, but the live client is {} / {} / {} / {}; re-run the dumper for \
                     this build",
                    dump.build.timestamp,
                    dump.build.size_of_image,
                    dump.build.entry_point,
                    dump.build.checksum,
                    image.timestamp,
                    image.size,
                    image.entry_point,
                    image.checksum
                );
            }
        }

        *self.image.lock() = Some(Arc::clone(&image));
        Ok(image)
    }

    fn name_index(&self) -> Result<Arc<HashMap<String, u64>>> {
        {
            let cached = self.name_index.lock();
            if let Some(index) = cached.as_ref() {
                return Ok(Arc::clone(index));
            }
        }

        let dump = self.dump_offsets()?;
        let image = self.ensure_image()?;
        let index = Arc::new(build_name_index(&self.ctx.memory, &image, dump)?);

        *self.name_index.lock() = Some(Arc::clone(&index));
        Ok(index)
    }

    fn descriptor_hits(&self) -> Result<Arc<Vec<DescriptorHit>>> {
        {
            let cached = self.descriptors.lock();
            if let Some(hits) = cached.as_ref() {
                return Ok(Arc::clone(hits));
            }
        }

        let dump = self.dump_offsets()?;
        let image = self.ensure_image()?;
        let names = self.name_index()?;
        let hits = Arc::new(scan_descriptors(&self.ctx.memory, &image, dump, &names)?);

        *self.descriptors.lock() = Some(Arc::clone(&hits));
        Ok(hits)
    }

    fn creator_by_name_ptr(
        &self,
        image: &ImageInfo,
        dump: &DumpOffsets,
        name_pointer: u64,
    ) -> Result<u64> {
        if name_pointer == 0 {
            return Ok(0);
        }

        let table = creator_table(&self.ctx.memory, image, dump)?;
        let blob = read_span(&self.ctx.memory, table.start, table.span())?;

        let stride = dump.entry_stride as usize;
        let mut offset = 0usize;
        while offset + stride <= blob.len() {
            if u64_at(&blob, offset) == name_pointer {
                return Ok(u64_at(&blob, offset + dump.entry_value as usize));
            }
            offset += stride;
        }

        Ok(0)
    }

    fn creator_by_text(
        &self,
        image: &ImageInfo,
        dump: &DumpOffsets,
        class_name: &str,
    ) -> Result<u64> {
        let table = creator_table(&self.ctx.memory, image, dump)?;
        let blob = read_span(&self.ctx.memory, table.start, table.span())?;

        let stride = dump.entry_stride as usize;
        let mut offset = 0usize;
        while offset + stride <= blob.len() {
            let key = u64_at(&blob, offset);
            if key != 0 && key != table.empty {
                if let Some(text) = name_text(&self.ctx.memory, image, dump, key) {
                    if text == class_name {
                        return Ok(u64_at(&blob, offset + dump.entry_value as usize));
                    }
                }
            }
            offset += stride;
        }

        Ok(0)
    }

    // -- gate ------------------------------------------------------------

    fn ensure_gate(&self) -> Result<()> {
        if self.gate.lock().is_some() {
            return Ok(());
        }

        {
            let failed = self.gate_failed_at.lock();
            if let Some(failed_at) = *failed {
                if failed_at.elapsed() < GATE_COOLDOWN {
                    bail!("instance_new gate is unavailable (retrying shortly)");
                }
            }
        }

        let image = self.ensure_image()?;
        let hits = self.descriptor_hits()?;
        let memory = Arc::clone(&self.ctx.memory);

        for method in HOT_METHODS {
            for hit in hits.iter().filter(|hit| hit.text == *method) {
                let candidates = CallGate::impl_candidates(&memory, hit.descriptor, |value| {
                    image.is_exec(value)
                });

                for (offset, implementation) in candidates {
                    let mut listener =
                        CallGate::new(Arc::clone(&memory), hit.descriptor, offset);
                    if !listener.install_listener() {
                        continue;
                    }

                    let before = listener.calls();
                    std::thread::sleep(LISTEN_DWELL);
                    let after = listener.calls();
                    listener.remove();

                    if after <= before {
                        continue;
                    }

                    log::info!(
                        target: "GamingChairLLC.instance_new",
                        "Gate slot: {method} descriptor {:#x} +{offset:#x} -> impl {:#x} \
                         ({} calls while listening)",
                        hit.descriptor - memory.base_address(),
                        implementation - memory.base_address(),
                        after - before
                    );

                    let mut gate = CallGate::new(Arc::clone(&memory), hit.descriptor, offset);
                    if gate.install() {
                        *self.gate.lock() = Some(gate);
                        *self.gate_failed_at.lock() = None;
                        return Ok(());
                    }

                    log::warn!(
                        target: "GamingChairLLC.instance_new",
                        "Gate install failed: {}",
                        gate.last_failure()
                    );
                    break;
                }
            }
        }

        *self.gate_failed_at.lock() = Some(Instant::now());

        Err(anyhow!(
            "no hot method slot responded; is the client in a live place with running scripts?"
        )
        .context("installing the instance_new call gate"))
    }
}

impl Drop for InstanceNew {
    fn drop(&mut self) {
        self.shutdown();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_embedded_dump_parses_and_looks_structural() {
        let dump: DumpFile = serde_json::from_str(DUMP_JSON).expect("embedded dump parses");

        assert!(dump.build.size_of_image > 0);
        assert!(dump.build.timestamp > 0);
        assert!(dump.build.entry_point > 0);
        assert!(dump.build.checksum > 0);

        let offsets = dump.instance_new;
        assert_eq!(offsets.entry_stride, 16);
        assert_eq!(offsets.entry_value, 8);
        assert_eq!(offsets.name_size, 0x10);
        assert_eq!(offsets.name_capacity, 0x18);
        assert_eq!(offsets.name_inline_capacity, 15);
        assert!(offsets.creator_create < offsets.creator_is_creatable);
        assert!(offsets.creator_table > 0);
        assert!(offsets.name_registry > 0);
        assert!(offsets.parent_desc > 0);
        assert!(offsets.parent_setter > 0);
    }

    /// Manual check against a live client: discovery plus a passive listener in
    /// a system-DLL cave. Ignored so CI does not need Roblox running.
    ///
    /// `cargo test --offline live_gate_discovery -- --ignored --nocapture`
    #[test]
    #[ignore = "requires a live Roblox client in a running place"]
    fn live_gate_discovery() {
        let process = crate::sdk::process::find_process("RobloxPlayerBeta.exe")
            .expect("no running Roblox client");
        let memory = Memory::attach(process.pid, "RobloxPlayerBeta.exe").expect("attach");

        let dump: DumpFile = serde_json::from_str(DUMP_JSON).expect("embedded dump parses");
        let offsets = &dump.instance_new;
        let image = read_image(&memory).expect("live image");
        let names = build_name_index(&memory, &image, offsets).expect("name index");
        let hits = scan_descriptors(&memory, &image, offsets, &names).expect("descriptors");

        let mut installed = 0;
        let mut hot: Option<(u64, u64)> = None;
        for method in HOT_METHODS {
            for hit in hits.iter().filter(|hit| hit.text == *method) {
                for (offset, implementation) in
                    CallGate::impl_candidates(&memory, hit.descriptor, |value| image.is_exec(value))
                {
                    let mut gate = CallGate::new(Arc::clone(&memory), hit.descriptor, offset);
                    if !gate.install_listener() {
                        println!(
                            "LISTENER FAIL {method} {:#x} +{offset:#x}: {}",
                            hit.descriptor,
                            gate.last_failure()
                        );
                        continue;
                    }

                    let before = gate.calls();
                    std::thread::sleep(LISTEN_DWELL);
                    let after = gate.calls();
                    gate.remove();

                    println!(
                        "LISTENER {method} desc {:#x} +{offset:#x} -> impl {:#x}: {} calls",
                        hit.descriptor,
                        implementation,
                        after.saturating_sub(before)
                    );

                    if after > before {
                        installed += 1;
                        hot.get_or_insert((hit.descriptor, offset));
                    }
                }
            }
        }

        assert!(installed > 0, "no listener observed any engine call");

        // Exercise the command stub too (the slow path the ESP actually uses):
        // queue a lone `ret` from module code, which cannot do anything, and
        // wait for an engine thread to run it.
        let (descriptor, offset) = hot.expect("a hot slot");
        let ret = image
            .sections
            .iter()
            .find(|section| section.characteristics & 0x2000_0000 != 0)
            .and_then(|section| {
                let blob = memory.read_bytes(image.base + section.start, 0x1000);
                blob.iter()
                    .position(|&byte| byte == 0xC3)
                    .map(|at| image.base + section.start + at as u64)
            })
            .expect("a ret byte in module code");

        let mut gate = CallGate::new(Arc::clone(&memory), descriptor, offset);
        assert!(gate.install(), "install: {}", gate.last_failure());

        let calls_before = gate.calls();
        let (ok, _) = gate.invoke(ret, 0, 0, 0, 0, 0, Duration::from_secs(2));
        println!(
            "INVOKE ret {ret:#x} -> ok={ok} fail={:?} calls={}",
            gate.last_failure(),
            gate.calls().saturating_sub(calls_before)
        );

        gate.remove();
        assert!(ok, "command stub did not run: {}", gate.last_failure());
    }
}
