//! What a web server says of a file arrives as header text that may be
//! anything. Reading it must not panic, and what is read must be something
//! a server could have meant.

#![no_main]

use libfuzzer_sys::fuzz_target;
use web::headers::{self, Validator};

fuzz_target!(|data: &[u8]| {
    let Ok(text) = core::str::from_utf8(data) else {
        return;
    };
    // The text is tried as each header, and split to try two at once.
    let (left, right) = text.split_at(text.len() / 2);
    let (left, right) = (left.trim_end_matches('\u{0}'), right);

    if let Some(range) = headers::content_range(text) {
        assert!(range.first <= range.last);
        if let Some(total) = range.total {
            assert!(range.last < total);
        }
    }
    let _ = headers::unsatisfied_total(text);
    let _ = headers::modified(text);
    let _ = headers::sha256(Some(left), Some(right));
    let _ = headers::sha256(Some(text), None);

    for (etag, date) in [
        (Some(text), None),
        (None, Some(text)),
        (Some(left), Some(right)),
    ] {
        if let Some(validator) = Validator::from(etag, date) {
            // A validator holds for the very headers it was read from, and
            // its bytes tell what kind it is.
            assert!(validator.still_holds(etag, date));
            assert!(!validator.as_header().is_empty());
            let bytes = validator.to_bytes();
            assert!(bytes.starts_with(b"etag:") || bytes.starts_with(b"date:"));
        }
    }
    if let Some(name) = headers::name_from(text) {
        assert!(model::artifact::is_safe_component(&name));
    }
});
