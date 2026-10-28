//! `ModuleScript` / `LocalScript` bytecode.
//!
//! The client keeps script bytecode in an encrypted container ("RSB1"): the
//! bytes are XOR-encrypted with a rolling key and the plaintext inside is a zstd
//! stream. This mirrors `decrypt_bytecode` in `GamingChairLLC-Python/
//! decompile.py`, which is also what the lua.expert uploader sends.
//!
//! Layout (see [`crate::sdk::offsets::ScriptOffsets`]):
//!
//! ```text
//! Instance           + 0x120 (ModuleScript) / 0x180 (LocalScript) -> Bytecode*
//! Bytecode           + 0x10  -> encrypted buffer
//! Bytecode           + 0x20  -> buffer size (u64)
//! ```
//!
//! Decrypted buffer:
//!
//! ```text
//! 0x00  "RSB1"
//! 0x04  decompressed size (u32 LE)
//! 0x08  zstd frame
//! ```

use std::io::Read;

use anyhow::{bail, Context, Result};

use crate::sdk::roblox::instance::Instance;

/// Magic bytes of a decrypted container.
const MAGIC: &[u8; 4] = b"RSB1";
/// Roll the key forward by this much per byte (`hash_multiplier` in Python).
const KEY_STEP: u8 = 41;
const HEADER_LEN: usize = 8;
/// Same bound the Python helper and the memory layer use. A real script is a
/// few KiB; anything above this is a nonsense read, not a huge script.
const MAX_BYTECODE: usize = 50 * 1024 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ScriptKind {
    Module,
    Local,
}

impl ScriptKind {
    pub fn from_class_name(class_name: &str) -> Option<Self> {
        if class_name.eq_ignore_ascii_case("ModuleScript") {
            Some(Self::Module)
        } else if class_name.eq_ignore_ascii_case("LocalScript") {
            Some(Self::Local)
        } else {
            None
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Module => "ModuleScript",
            Self::Local => "LocalScript",
        }
    }
}

/// A checked `Instance` that is known to be a `ModuleScript` or `LocalScript`.
pub struct Script {
    instance: Instance,
    kind: ScriptKind,
}

impl Script {
    /// Class-checking constructor. Fails for every other instance type, so a
    /// stale explorer row can never make the app read random memory.
    pub fn new(instance: Instance) -> Result<Self> {
        let class_name = instance.get_class_name();
        let kind = ScriptKind::from_class_name(&class_name).with_context(|| {
            format!("'{class_name}' is not a ModuleScript or LocalScript")
        })?;

        Ok(Self { instance, kind })
    }

    pub fn kind(&self) -> ScriptKind {
        self.kind
    }

    pub fn name(&self) -> String {
        self.instance.get_name()
    }

    pub fn address(&self) -> u64 {
        self.instance.address()
    }

    /// The raw, still-encrypted container exactly as it sits in memory.
    pub fn encrypted_bytecode(&self) -> Result<Vec<u8>> {
        let ctx = self.instance.ctx();
        let offsets = &ctx.offsets.scripts;
        let memory = &ctx.memory;

        let instance_field = match self.kind {
            ScriptKind::Module => offsets.module_script_bytecode,
            ScriptKind::Local => offsets.local_script_bytecode,
        };

        let container = memory.read_ptr(self.instance.address().wrapping_add(instance_field));
        if container == 0 {
            bail!(
                "{} '{}' has no bytecode container (script not loaded yet?)",
                self.kind.as_str(),
                self.name()
            );
        }

        let buffer = memory.read_ptr(container.wrapping_add(offsets.bytecode_buffer));
        let size = memory.read_ptr(container.wrapping_add(offsets.bytecode_size)) as usize;

        if buffer == 0 || size == 0 {
            bail!("{} '{}' has empty bytecode", self.kind.as_str(), self.name());
        }
        if size > MAX_BYTECODE {
            bail!("bytecode size {size} is out of range");
        }

        let data = memory.read_bytes(buffer, size);
        if data.len() != size {
            bail!(
                "could only read {} of {size} bytecode bytes",
                data.len()
            );
        }

        Ok(data)
    }

    /// Decrypted, decompressed Luau bytecode — what the decompiler consumes.
    pub fn bytecode(&self) -> Result<Vec<u8>> {
        decode_container(&self.encrypted_bytecode()?)
            .with_context(|| format!("decoding bytecode of '{}'", self.name()))
    }
}

/// Decrypt an RSB1 container and decompress the zstd payload inside it.
///
/// Kept standalone (and public) so it can be unit-tested without a game
/// process.
pub fn decode_container(encrypted: &[u8]) -> Result<Vec<u8>> {
    let decrypted = decrypt(encrypted)?;
    let expected = u32::from_le_bytes([decrypted[4], decrypted[5], decrypted[6], decrypted[7]])
        as usize;

    if expected == 0 || expected > MAX_BYTECODE {
        bail!("bytecode container declares an invalid size ({expected} bytes)");
    }

    let decoder = ruzstd::decoding::StreamingDecoder::new(&decrypted[HEADER_LEN..])
        .context("invalid zstd stream in bytecode container")?;

    // Trust-but-verify: cap the allocation at what the header promised, and
    // hard-stop one byte past the maximum in case a corrupt frame lies.
    let mut plain = Vec::with_capacity(expected);
    decoder
        .take(MAX_BYTECODE as u64 + 1)
        .read_to_end(&mut plain)
        .context("could not decompress bytecode")?;

    if plain.is_empty() {
        bail!("bytecode decompressed to nothing");
    }
    if plain.len() != expected {
        log::warn!(
            target: "GamingChairLLC.script",
            "Bytecode header says {expected} bytes but {} were decompressed",
            plain.len()
        );
    }
    if plain.len() > MAX_BYTECODE {
        bail!("decompressed bytecode is too large ({} bytes)", plain.len());
    }

    Ok(plain)
}

/// Undo the rolling XOR. Returns the container with the plaintext `RSB1`
/// header, still zstd-compressed.
fn decrypt(encrypted: &[u8]) -> Result<Vec<u8>> {
    if encrypted.len() < HEADER_LEN {
        bail!("bytecode container is too short ({} bytes)", encrypted.len());
    }

    let mut buffer = encrypted.to_vec();
    let mut key = [0u8; 4];

    for (i, slot) in key.iter_mut().enumerate() {
        *slot = (buffer[i] ^ MAGIC[i]).wrapping_sub((i as u8).wrapping_mul(KEY_STEP));
    }

    for (i, byte) in buffer.iter_mut().enumerate() {
        *byte ^= key[i % 4].wrapping_add((i as u8).wrapping_mul(KEY_STEP));
    }

    if !buffer.starts_with(MAGIC) {
        bail!("bytecode decryption failed (missing RSB1 header)");
    }

    Ok(buffer)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Decode a hex string into bytes (for the embedded zstd frame).
    fn unhex(text: &str) -> Vec<u8> {
        let digits: Vec<u8> = text.bytes().filter(u8::is_ascii_hexdigit).collect();
        digits
            .chunks(2)
            .map(|pair| {
                let hi = (pair[0] as char).to_digit(16).unwrap() as u8;
                let lo = (pair[1] as char).to_digit(16).unwrap() as u8;
                (hi << 4) | lo
            })
            .collect()
    }

    /// Encrypt a plaintext container into the shape the client stores.
    fn encode(plain: &[u8]) -> Vec<u8> {
        let mut container = Vec::with_capacity(HEADER_LEN + plain.len());
        container.extend_from_slice(MAGIC);
        container.extend_from_slice(&(plain.len() as u32).to_le_bytes());
        container.extend_from_slice(plain);

        let key = [0x11u8, 0x22, 0x33, 0x44];
        for (i, byte) in container.iter_mut().enumerate() {
            *byte ^= key[i % 4].wrapping_add((i as u8).wrapping_mul(KEY_STEP));
        }
        container
    }

    #[test]
    fn decrypts_a_container_back_to_its_header() {
        let container = encode(b"payload");
        let decrypted = decrypt(&container).expect("decrypts");
        assert_eq!(&decrypted[..4], MAGIC);
        assert_eq!(&decrypted[HEADER_LEN..], b"payload");
    }

    #[test]
    fn rejects_short_containers() {
        assert!(decode_container(b"RSB").is_err());
    }

    #[test]
    fn decodes_a_zstd_container() {
        let payload = b"print('hello from GamingChairLLC')\n".repeat(3);

        // zstd frame produced by Python's `zstandard` (the library the Python
        // tool uses), so this checks the zstd path without needing a game.
        let frame = unhex(
            "28b52ffd20695d010034027072696e74282768656c6c6f2066726f6d2047616d696e6743686169724c4c4327290a01001b83aa27",
        );

        let container = encode(&frame);
        assert_eq!(decode_container(&container).unwrap(), payload);
    }

    #[test]
    fn rejects_containers_with_a_zero_size() {
        // The header always decrypts to `RSB1` (the key is derived from it), so
        // the size field is what rejects a garbage buffer.
        assert!(decode_container(&[0u8; 32]).is_err());
    }

    #[test]
    fn classifies_script_classes() {
        assert_eq!(
            ScriptKind::from_class_name("ModuleScript"),
            Some(ScriptKind::Module)
        );
        assert_eq!(
            ScriptKind::from_class_name("LocalScript"),
            Some(ScriptKind::Local)
        );
        assert_eq!(ScriptKind::from_class_name("Part"), None);
    }
}
