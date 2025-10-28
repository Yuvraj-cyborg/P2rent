use p2rent::crypto::{default_key_path, load_or_create_keypair};

#[test]
fn load_or_create_roundtrip() {
    // Use default path; ensure it works twice
    let kp1 = load_or_create_keypair().expect("create/load");
    let kp2 = load_or_create_keypair().expect("load again");
    assert_eq!(kp1.verifying.to_bytes(), kp2.verifying.to_bytes());
}

