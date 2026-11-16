//! Requests and responses arrive from another machine. Decoding must not
//! panic, and a set of chunks taken from a status reply must describe
//! exactly the artifact that was asked about.

#![no_main]

use libfuzzer_sys::fuzz_target;
use p2rent_core::schedule::Schedule;
use p2rent_peer::net::proto::{Request, Response};

fuzz_target!(|data: &[u8]| {
    let _ = postcard::from_bytes::<Request>(data);

    let Ok(Response::Status(status)) = postcard::from_bytes::<Response>(data) else {
        return;
    };
    // The first byte picks how many chunks the artifact is taken to have.
    let chunks = u64::from(data.first().copied().unwrap_or(0));
    let Ok(have) = status.have.into_bitfield(chunks) else {
        return;
    };
    assert!(have.is_well_formed(chunks));
    assert!(have.count_ones() <= chunks);
    assert!(!have.get(chunks));

    // A peer's claimed set is what the scheduler works from: claiming
    // against it must stay in range and must end.
    let mut schedule = Schedule::new(&p2rent_core::artifact::Bitfield::new(chunks));
    let mut cursor = chunks / 2;
    let mut claimed = 0;
    while let Some(index) = schedule.claim(&have, &mut cursor) {
        assert!(index < chunks && have.get(index));
        claimed += 1;
        assert!(claimed <= chunks);
    }
    assert_eq!(claimed, have.count_ones());
});
