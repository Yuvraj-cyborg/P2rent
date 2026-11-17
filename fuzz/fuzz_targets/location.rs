//! A location is typed by a person or handed over by another program.
//! Whatever the text is, reading it must not panic. Anything accepted must
//! print back to text that reads as the same place, and anything refused
//! must be answered with a sentence.

#![no_main]

use libfuzzer_sys::fuzz_target;
use model::location::{Location, Protocol};

/// A build with every protocol the model has a rule for.
const EVERY: [Protocol; 5] = [
    protocol("file", true, true),
    protocol("http", true, false),
    protocol("https", true, false),
    protocol("mux", true, false),
    protocol("peer", false, true),
];

const fn protocol(scheme: &'static str, reads: bool, writes: bool) -> Protocol {
    Protocol {
        scheme,
        reads,
        writes,
        about: "",
    }
}

fuzz_target!(|data: &[u8]| {
    let Ok(text) = core::str::from_utf8(data) else {
        return;
    };

    match Location::parse(text, &EVERY) {
        Ok(place) => {
            let again = Location::parse(&place.to_string(), &EVERY);
            assert_eq!(again.as_ref(), Ok(&place), "{text:?}");
        }
        Err(error) => assert!(!error.to_string().is_empty()),
    }
    // A build with fewer protocols refuses more, and never panics either.
    let _ = Location::parse(text, &EVERY[..1]);
    let _ = Location::parse(text, &[]);
});
