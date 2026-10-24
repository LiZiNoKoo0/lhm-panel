//! The flash region this firmware owns: two sectors, A and B, and the rule that
//! decides which one a reader believes.
//!
//! What this does NOT do: no key/value API, no file system, no wear levelling,
//! no third copy. It is SPEC §6 S-1 in code: write the *inactive* sector,
//! verify what came back, and let the `seq` in the frame header decide who wins.
//! The bytes themselves -- magic, version, length, CRC, the tie-break between
//! two frames -- live in `panel_core::sector`, where they are tested on a PC.
//!
//! Why two sectors at all: with one sector, a power cut between erase and write
//! costs the user their configuration and the board comes up as a hotspot with
//! no explanation. With A/B the worst case is "the save did not happen", and the
//! board still knows the WiFi it knew before. SPEC S-2 forbids "both copies
//! broken", and that is the invariant the order below is built to keep:
//!
//! ```text
//! erase target -> write frame -> read back -> compare -> seq already decides it
//! ```
//!
//! Nothing in here knows what WiFi is: it moves payload bytes for
//! `panel_core::creds` and reports what the flash did.

use alloc::string::String;
use embedded_storage::nor_flash::{NorFlash, ReadNorFlash};
use esp_storage::FlashStorage;
use panel_core::creds::{self, WifiCreds};
use panel_core::sector;

use crate::board;

/// True when a raw 16-byte label field names `want`.
///
/// The field is NUL-padded, and a blank sector is 0xFF-padded, so the name ends
/// at the first NUL or 0xFF. This is `label_as_str` without the unsafe UTF-8
/// claim: a garbage label simply fails to match instead of being reinterpreted.
fn label_is(raw: &[u8], want: &[u8]) -> bool {
    let end = raw
        .iter()
        .position(|b| *b == 0 || *b == 0xFF)
        .unwrap_or(raw.len());
    &raw[..end] == want
}

/// Why the store refused to open. Carries the numbers the log line needs.
#[derive(Debug, Clone, Copy)]
pub enum OpenError {
    /// The partition table does not describe the region `board.rs` assumes.
    Layout { offset: u32, len: u32 },
    /// The table could not be read at all (blank chip, wrong flash mode).
    Unreadable,
}

/// One of the two copies. The letters go into the log, so they are stable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Slot {
    A,
    B,
}

impl Slot {
    /// Sector address of this slot.
    pub fn offset(self) -> u32 {
        match self {
            // A is the sector T3 used, so a board upgraded from T3 finds its
            // credentials exactly where it left them.
            Slot::A => board::CREDS_SECTOR,
            Slot::B => board::CREDS_SECTOR + board::FLASH_SECTOR,
        }
    }

    fn other(self) -> Slot {
        match self {
            Slot::A => Slot::B,
            Slot::B => Slot::A,
        }
    }

    /// The letter that goes on the wire (`slot=A`).
    pub fn letter(self) -> u8 {
        match self {
            Slot::A => b'A',
            Slot::B => b'B',
        }
    }
}

/// Which copy a reader should believe, and how new it is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Copy {
    pub slot: Slot,
    pub seq: u32,
}

/// What `load` found: the payload (if it decoded) plus the copy it came from.
pub struct Opened {
    pub creds: Option<WifiCreds>,
    pub copy: Option<Copy>,
    /// True when `load` had to rewrite a pre-T4 record into the framed layout.
    pub migrated: bool,
}

/// How many bytes of a slot are read or written. Bounded by the only payload
/// this store carries today, and rounded up to a 4-byte boundary because the
/// flash driver writes whole words: an unaligned length is refused by the
/// driver *after* the erase, which is how this store once destroyed the only
/// copy it had ("erase OK, write EINVAL"). Padding bytes are not covered by the
/// frame's CRC -- readers stop at the frame's own length field.
const FRAME_BYTES: usize = sector::HEADER_LEN + creds::RECORD_LEN;
const SLOT_BYTES: usize = (FRAME_BYTES + 3) & !3;

pub struct Store<'d> {
    flash: FlashStorage<'d>,
}

impl<'d> Store<'d> {
    /// Open the flash and prove, from the partition table the board actually
    /// booted with, that the region we are about to write is the `nvs`
    /// partition and not something esp-phy owns.
    ///
    /// Refusing to open is the safe outcome: no credentials means the board
    /// comes up in AP mode, which is recoverable. Writing 4 KB over `phy_init`
    /// because a table changed is not.
    pub fn open(flash: FlashStorage<'d>) -> Result<Self, OpenError> {
        let mut store = Self { flash };
        let mut table_buf = [0u8; esp_bootloader_esp_idf::partitions::PARTITION_TABLE_MAX_LEN];

        let table = match esp_bootloader_esp_idf::partitions::read_partition_table(
            &mut store.flash,
            &mut table_buf,
        ) {
            Ok(table) => table,
            Err(_) => return Err(OpenError::Unreadable),
        };

        for entry in table.iter() {
            // Compare through `label_is`, not `entry.label() == b"nvs"`: the
            // label field is 16 bytes NUL-padded, so comparing it to a 3-byte
            // literal is 16 bytes against 3 and is *always* false. That is how
            // this store reported "layout bad" on a table that was perfectly
            // good (verified by dumping 0x8000 off the chip).
            if label_is(entry.label(), b"nvs") {
                let (offset, len) = (entry.offset(), entry.len());
                // Both slots have to sit inside the partition the table
                // describes, not just slot A.
                let need = (Slot::B.offset() + board::FLASH_SECTOR) - board::NVS_START;
                if offset == board::NVS_START && len >= need {
                    return Ok(store);
                }
                return Err(OpenError::Layout { offset, len });
            }
        }

        Err(OpenError::Layout { offset: 0, len: 0 })
    }

    /// The slot's frame tag -- slot and `seq` only, no payload kept alive.
    fn slot_copy(&mut self, slot: Slot) -> Option<Copy> {
        let mut buf = [0u8; SLOT_BYTES];
        self.flash.read(slot.offset(), &mut buf).ok()?;
        let frame = sector::parse(&buf)?;
        Some(Copy {
            slot,
            seq: frame.seq,
        })
    }

    /// The winning copy, by `seq`. `None` means "nothing here is usable", which
    /// covers a blank chip, two damaged slots, and sectors holding something
    /// that is not ours.
    fn best(&mut self) -> Option<Copy> {
        match (self.slot_copy(Slot::A), self.slot_copy(Slot::B)) {
            (Some(a), Some(b)) => Some(if sector::is_newer(a.seq, b.seq) { a } else { b }),
            (a, b) => a.or(b),
        }
    }

    /// Read both slots, pick the winner, and decode the credentials it carries.
    ///
    /// A missing record, a bad CRC and a blank sector all mean the same thing
    /// to the caller: no credentials, open the hotspot -- except for one case:
    /// a board upgraded from T3 has the bare 108-byte record at the front of
    /// sector A, with no frame header around it. That is not a valid framed
    /// copy, so it would read as "no credentials" and the user would have to
    /// The stored token as an owned string (`""` when nothing is paired).
    ///
    /// The config page needs it when the token field is submitted empty: an
    /// empty field means "keep what is paired", and reading it back is the only
    /// way to honour that without a second copy of the record in RAM.
    pub fn token_owned(&mut self) -> String {
        self.load()
            .creds
            .map(|c| String::from(c.token()))
            .unwrap_or_default()
    }

    /// retype their WiFi password after a firmware upgrade. Instead it is
    /// carried forward once, into the framed layout, and reported as migrated.
    pub fn load(&mut self) -> Opened {
        if let Some(opened) = self.load_framed() {
            return Opened {
                migrated: false,
                ..opened
            };
        }

        let Some(creds) = self.legacy_creds() else {
            return Opened {
                creds: None,
                copy: None,
                migrated: false,
            };
        };

        // Write it back through the same sealing code the web page uses, but
        // into the slot that does *not* hold the legacy record: the migration
        // may fail (a bad sector, a full chip) and if it does, sector A must
        // still be the record it was, so the next boot can try again. Erasing
        // the only copy before the new one is proved is precisely the mistake
        // SPEC S-2 exists to forbid -- and this path made it once, on the
        // bench, taking the user's password with it.
        let _ = self.save_to(Slot::B, 1, &creds);
        Opened {
            creds: Some(creds),
            copy: self.best(),
            migrated: true,
        }
    }

    /// The framed read: `None` when neither slot holds a complete frame.
    fn load_framed(&mut self) -> Option<Opened> {        let copy = self.best()?;

        let mut buf = [0u8; SLOT_BYTES];
        let creds = match self.flash.read(copy.slot.offset(), &mut buf) {
            Ok(()) => sector::parse(&buf).and_then(|frame| {
                WifiCreds::decode(&buf[sector::HEADER_LEN..sector::HEADER_LEN + frame.payload.len()])
            }),
            Err(_) => None,
        };

        Some(Opened {
            creds,
            copy: Some(copy),
            migrated: false,
        })
    }

    /// The T3 layout: a bare record at offset 0 of sector A, no header.
    fn legacy_creds(&mut self) -> Option<WifiCreds> {
        let mut record = [0u8; creds::RECORD_LEN];
        self.flash.read(Slot::A.offset(), &mut record).ok()?;
        WifiCreds::decode(&record)
    }

    /// Write the credentials into the slot that is *not* currently the truth,
    /// then prove the write by reading it back.
    ///
    /// Returns true only if the read-back parses, carries the bytes we sealed,
    /// and is the copy `load` would now pick. Anything else -- erase refused,
    /// write refused, a byte that came back different, a tie-break that still
    /// prefers the other slot -- leaves the old copy in charge and reports
    /// false, because "it said OK" is not evidence.
    pub fn save(&mut self, creds: &WifiCreds) -> bool {
        let active = self.best();
        let target = active.map(|c| c.slot.other()).unwrap_or(Slot::A);
        let seq = active.map(|c| c.seq.wrapping_add(1)).unwrap_or(1);
        self.save_to(target, seq, creds)
    }

    /// The body of `save`, with the slot and sequence number given explicitly:
    /// the migration path needs to write into slot B while slot A still holds
    /// the record it is migrating from.
    fn save_to(&mut self, target: Slot, seq: u32, creds: &WifiCreds) -> bool {
        let mut record = [0u8; creds::RECORD_LEN];
        let len = match creds.encode(&mut record) {
            Some(len) => len,
            None => return false,
        };

        let mut frame = [0u8; SLOT_BYTES];
        let total = match sector::seal(seq, &record[..len], &mut frame) {
            Some(total) => total,
            None => return false,
        };
        debug_assert!(total <= SLOT_BYTES, "frame does not fit a slot");
        // The driver writes whole words: round the write up, let the reader stop
        // at the frame's own `len`.
        let padded = (total + 3) & !3;

        let start = target.offset();
        if self.flash.erase(start, start + board::FLASH_SECTOR).is_err() {
            return false;
        }

        // Bench hook: lose power with the target sector erased and the frame
        // half written -- the exact moment SPEC S-2 is about. The other slot
        // must still answer the next boot. Enabled only by
        // `--features bench-powercut`; see Cargo.toml.
        #[cfg(feature = "bench-powercut")]
        self.lose_power(start, &frame[..padded]);
        if self.flash.write(start, &frame[..padded]).is_err() {
            return false;
        }

        let mut back = [0u8; SLOT_BYTES];
        if self.flash.read(start, &mut back).is_err() {
            return false;
        }
        match sector::parse(&back) {
            Some(landed) if landed.seq == seq && landed.payload == &record[..len] => {}
            _ => return false,
        }

        // And the tie-break has to agree: if `load` would not pick what we just
        // wrote, the save did not succeed in any sense the caller cares about.
        matches!(self.best(), Some(c) if c.slot == target && c.seq == seq)
    }

    /// Write half a frame into `start` and reset, simulating the power cut
    /// SPEC S-2 is about. Only compiled with `--features bench-powercut`.
    ///
    /// A separate method, not an inline block: `software_reset()` never
    /// returns, so putting it in the middle of `save` makes all of `save` look
    /// like dead code to the compiler -- and a warning we learned to ignore is
    /// a warning that stops working.
    #[cfg(feature = "bench-powercut")]
    fn lose_power(&mut self, start: u32, frame: &[u8]) {
        // Word-aligned so the driver really does write it, then stop dead.
        let cut = (frame.len() / 2) & !3;
        let _ = self.flash.write(start, &frame[..cut]);
        esp_hal::system::software_reset();
    }

    /// Bench-only: put a T3-format (headerless) record at the front of slot A,
    /// which is what a board upgraded from T3 looks like. Used to exercise the
    /// migration path on hardware without depending on the user's password.
    #[cfg(feature = "bench-legacy")]
    pub fn write_legacy_for_bench(&mut self, creds: &WifiCreds) -> bool {
        let mut record = [0u8; creds::RECORD_LEN];
        let len = match creds.encode(&mut record) {
            Some(len) => len,
            None => return false,
        };
        let start = Slot::A.offset();
        if self.flash.erase(start, start + board::FLASH_SECTOR).is_err() {
            return false;
        }
        // `erase` + a `RECORD_LEN` write: 108 is already a multiple of 4.
        debug_assert_eq!(len % 4, 0);
        self.flash.write(start, &record[..len]).is_ok()
    }
}
