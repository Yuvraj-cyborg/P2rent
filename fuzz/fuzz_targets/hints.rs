//! A peer supplies the bytes and a verified manifest determines the count.
#![no_main]

use libfuzzer_sys::fuzz_target;
use model::search::ChunkHints;

fuzz_target!(|data: &[u8]| {
    let Some((prefix, bytes)) = data.split_first_chunk::<8>() else {
        return;
    };
    for count in [u64::from_le_bytes(*prefix), bytes.len() as u64 / 8] {
        let hints = ChunkHints::decode(bytes.to_vec(), count);
        assert_eq!(
            hints.is_some(),
            count.checked_mul(8) == Some(bytes.len() as u64)
        );
        if let Some(hints) = hints {
            for (index, raw) in (0..).zip(bytes.as_chunks::<8>().0) {
                assert_eq!(hints.get(index).unwrap().to_le_bytes(), *raw);
            }
            assert_eq!(hints.get(count), None);
            assert_eq!(hints.get(u64::MAX), None);
        }
    }
});
