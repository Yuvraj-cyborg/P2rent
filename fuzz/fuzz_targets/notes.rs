//! Notes are read back from a disk, where they may have been cut short or
//! damaged, or may never have been notes. Whatever the bytes are, reading
//! them must not panic, and what is read must be something notes can say.

#![no_main]

use libfuzzer_sys::fuzz_target;
use model::notes::{MAX_VALIDATOR_BYTES, Notes};

fuzz_target!(|data: &[u8]| {
    let Some(notes) = Notes::read(data) else {
        return;
    };

    // What was read begins as the bytes did, and reads back the same.
    let header = notes.header();
    assert!(data.starts_with(&header));
    assert!(header.len() <= 23 + MAX_VALIDATOR_BYTES);
    assert_eq!(Notes::read(data).as_ref(), Some(&notes));

    // A digest is given only for a chunk whose slot is wholly in the bytes.
    let slots = ((data.len() - header.len()) / 33) as u64;
    for chunk in [0, 1, slots.saturating_sub(1), slots, slots + 1, u64::MAX] {
        if let Some(digest) = notes.digest(chunk) {
            assert!(chunk < slots);
            let (offset, slot) = notes.slot(chunk, digest).expect("a slot that was read");
            let start = usize::try_from(offset).expect("it is within the bytes");
            assert_eq!(&data[start..start + slot.len()], &slot[..]);
        }
    }
});
