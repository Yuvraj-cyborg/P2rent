//! Who may fetch what a machine offers.
//!
//! An offer has a secret, which its ticket carries. A machine that asks for
//! any of the offer shows that it knows the secret, without sending it, and
//! is then let in or not by the rule the offer was made with.

use crate::hex;
use crate::ticket::NodeId;
use alloc::string::String;
use alloc::vec::Vec;
use core::fmt;
use core::str::FromStr;
use serde::{Deserialize, Serialize};

/// The second half of the key a proof is made with, so that a proof is a
/// proof of nothing else the secret may come to be used for.
const PURPOSE: [u8; 16] = *b"mux access proof";

/// The secret of one offer: sixteen random bytes, made when the offer is
/// and written in its ticket. Whoever knows it may ask for the offer.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct Secret([u8; 16]);

impl Secret {
    /// Wraps sixteen bytes, which must come from a source of randomness.
    pub const fn from_bytes(bytes: [u8; 16]) -> Self {
        Self(bytes)
    }

    /// Lowercase hex, 32 characters: how a ticket writes it.
    pub fn to_hex(&self) -> String {
        hex::encode(&self.0)
    }

    /// What `client` shows `server` to prove it knows the secret: BLAKE3
    /// keyed with the secret, over the ids of the two.
    ///
    /// Both ids are proven by the connection itself. So what one machine
    /// was shown is of no use to it with another, and what one machine may
    /// show is of no use to another.
    pub fn proof(&self, server: &NodeId, client: &NodeId) -> Proof {
        let mut key = [0u8; 32];
        key[..16].copy_from_slice(&self.0);
        key[16..].copy_from_slice(&PURPOSE);
        let mut ids = [0u8; 64];
        ids[..32].copy_from_slice(server.as_bytes());
        ids[32..].copy_from_slice(client.as_bytes());
        Proof(*blake3::keyed_hash(&key, &ids).as_bytes())
    }
}

/// Says only that it is a secret: a log line must not give one away.
impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Secret(..)")
    }
}

impl FromStr for Secret {
    type Err = InvalidSecret;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let bytes = hex::decode(s).ok_or(InvalidSecret)?;
        <[u8; 16]>::try_from(bytes)
            .map(Self)
            .ok()
            .ok_or(InvalidSecret)
    }
}

/// The text is not a secret of 32 hex characters.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("expected 32 hex characters")]
pub struct InvalidSecret;

/// What a machine sends with a request to show that it knows the secret of
/// what it asks for. See [`Secret::proof`].
#[derive(Clone, Copy, Serialize, Deserialize)]
pub struct Proof([u8; 32]);

impl Proof {
    /// Wraps a proof made elsewhere.
    pub const fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }
}

/// Compared in a time that does not tell how many bytes were right.
impl PartialEq for Proof {
    fn eq(&self, other: &Self) -> bool {
        blake3::Hash::from_bytes(self.0) == blake3::Hash::from_bytes(other.0)
    }
}

impl Eq for Proof {}

impl fmt::Debug for Proof {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Proof(..)")
    }
}

/// Which machines an offer lets in, of those that know its secret.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Guests {
    /// The machines let in so far, and those named beforehand. Empty when
    /// there is room for any number: none need be remembered then.
    known: Vec<NodeId>,
    /// How many more may be let in. `None` for any number.
    room: Option<usize>,
}

impl Guests {
    /// Every machine that knows the secret.
    pub const fn anyone() -> Self {
        Self {
            known: Vec::new(),
            room: None,
        }
    }

    /// The machines `named`, and no other.
    pub const fn only(named: Vec<NodeId>) -> Self {
        Self {
            known: named,
            room: Some(0),
        }
    }

    /// The first `count` machines that ask, and no other after them.
    pub const fn first(count: usize) -> Self {
        Self {
            known: Vec::new(),
            room: Some(count),
        }
    }

    /// Whether `node` is let in. A machine that is let in stays let in: it
    /// can ask again, and carry on with a copy that was cut short. One let
    /// in for the first time takes a place, while there is one.
    pub fn admit(&mut self, node: &NodeId) -> bool {
        let Some(room) = self.room else {
            return true;
        };
        let mut index = 0;
        while index < self.known.len() {
            if self.known[index] == *node {
                return true;
            }
            index += 1;
        }
        if room == 0 {
            return false;
        }
        self.known.push(*node);
        self.room = Some(room - 1);
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn node(byte: u8) -> NodeId {
        NodeId::from_bytes([byte; 32])
    }

    #[test]
    fn a_secret_is_written_as_hex_and_read_back() {
        let secret = Secret::from_bytes([0xab; 16]);
        assert_eq!(secret.to_hex(), "ab".repeat(16));
        assert_eq!(secret.to_hex().parse::<Secret>(), Ok(secret));
        assert_eq!("AB".repeat(16).parse::<Secret>(), Ok(secret));
        for text in [
            "",
            "ab",
            &"ab".repeat(15),
            &"ab".repeat(17),
            &"zz".repeat(16),
        ] {
            assert_eq!(text.parse::<Secret>(), Err(InvalidSecret), "{text:?}");
        }
    }

    #[test]
    fn neither_a_secret_nor_a_proof_is_printed() {
        let secret = Secret::from_bytes([0xab; 16]);
        assert_eq!(format!("{secret:?}"), "Secret(..)");
        let proof = secret.proof(&node(1), &node(2));
        assert_eq!(format!("{proof:?}"), "Proof(..)");
    }

    #[test]
    fn a_proof_is_for_one_secret_one_server_and_one_client() {
        let secret = Secret::from_bytes([7; 16]);
        let proof = secret.proof(&node(1), &node(2));
        assert_eq!(proof, secret.proof(&node(1), &node(2)));
        // Another secret, another server, another client, and the two the
        // other way round.
        assert_ne!(proof, Secret::from_bytes([8; 16]).proof(&node(1), &node(2)));
        assert_ne!(proof, secret.proof(&node(3), &node(2)));
        assert_ne!(proof, secret.proof(&node(1), &node(3)));
        assert_ne!(proof, secret.proof(&node(2), &node(1)));
        // And it is not the secret.
        assert_ne!(proof, Proof::from_bytes([7; 32]));
    }

    /// The proof is part of what two machines say to each other, so it
    /// must not change from one version to the next.
    #[test]
    fn a_proof_is_blake3_keyed_with_the_secret_over_the_two_ids() {
        let secret = Secret::from_bytes([7; 16]);
        let mut key = [7u8; 32];
        key[16..].copy_from_slice(b"mux access proof");
        let ids = [[1u8; 32], [2u8; 32]].concat();
        let expected = Proof::from_bytes(*blake3::keyed_hash(&key, &ids).as_bytes());
        assert_eq!(secret.proof(&node(1), &node(2)), expected);
    }

    #[test]
    fn anyone_is_let_in_and_nobody_is_remembered() {
        let mut guests = Guests::anyone();
        for byte in 0..=255 {
            assert!(guests.admit(&node(byte)));
        }
        assert_eq!(guests, Guests::anyone());
    }

    #[test]
    fn only_the_machines_that_are_named_are_let_in() {
        let mut guests = Guests::only(vec![node(1), node(2)]);
        assert!(guests.admit(&node(2)));
        assert!(!guests.admit(&node(3)));
        assert!(guests.admit(&node(1)));
        assert!(guests.admit(&node(2)));
        assert!(!guests.admit(&node(3)));
        // Nobody is named: nobody is let in.
        assert!(!Guests::only(Vec::new()).admit(&node(1)));
    }

    #[test]
    fn the_first_to_ask_are_let_in_and_stay_let_in() {
        let mut guests = Guests::first(2);
        assert!(guests.admit(&node(1)));
        assert!(guests.admit(&node(1)));
        assert!(guests.admit(&node(2)));
        assert!(!guests.admit(&node(3)));
        // Those let in before can still ask.
        assert!(guests.admit(&node(1)));
        assert!(guests.admit(&node(2)));
        assert!(!guests.admit(&node(3)));
        assert!(!Guests::first(0).admit(&node(1)));
    }
}

/// Bounded proofs, run by `just kani`. Each unwinds its loops 34 times:
/// comparing two ids is a loop over their 32 bytes, and the rule's own
/// loops are shorter.
///
/// The machines are three, told apart by one byte: the rule only ever
/// compares ids. Which of them asks at each turn is left open.
#[cfg(kani)]
mod proofs {
    use super::*;

    const MACHINES: usize = 3;
    const TURNS: usize = 4;

    fn machine(which: usize) -> NodeId {
        let mut id = [0u8; 32];
        id[0] = which as u8;
        NodeId::from_bytes(id)
    }

    /// Any one of the machines.
    fn any_machine() -> usize {
        let which: usize = kani::any();
        kani::assume(which < MACHINES);
        which
    }

    /// Whatever machines ask and in whatever order, a machine is let in
    /// exactly when it was let in before or there is still room. So no
    /// more are let in than there was room for, and one that was let in is
    /// never turned away afterwards.
    #[kani::proof]
    #[kani::unwind(34)]
    fn the_first_to_ask_are_let_in_and_no_other() {
        // The room is walked through one number at a time: the model
        // checker is far quicker when only who asks is left open.
        let mut room = 0;
        while room < MACHINES {
            let mut guests = Guests::first(room);
            let mut inside = [false; MACHINES];
            let mut count = 0;
            let mut turn = 0;
            while turn < TURNS {
                let which = any_machine();
                let admitted = guests.admit(&machine(which));
                assert!(admitted == (inside[which] || count < room));
                if admitted && !inside[which] {
                    inside[which] = true;
                    count += 1;
                }
                turn += 1;
            }
            assert!(count <= room);
            room += 1;
        }
    }

    /// A machine that is named is always let in, and one that is not named
    /// never is, however often and in whatever order they ask.
    #[kani::proof]
    #[kani::unwind(34)]
    fn only_named_machines_are_let_in() {
        let mut guests = Guests::only(alloc::vec![machine(0), machine(1)]);
        let mut turn = 0;
        while turn < TURNS {
            let which = any_machine();
            assert!(guests.admit(&machine(which)) == (which < 2));
            turn += 1;
        }
    }
}
