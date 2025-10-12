use crate::error::Result;
use base64::{Engine as _, engine::general_purpose};
use blake3;
use ed25519_dalek::{SECRET_KEY_LENGTH, Signature, Signer, SigningKey, VerifyingKey};
use rand_core::{OsRng, TryRngCore};
use serde::{Deserialize, Serialize};
use std::convert::TryInto;
use std::fs::{self, File};
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

pub type NodeId = String;

#[derive(Clone, Debug)]
pub struct NodeKeypair {
    pub signing: SigningKey,
    pub verifying: VerifyingKey,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SerializableKeypair {
    pub public_b64: String,
    pub private_b64: String,
    pub created_at_unix: u64,
}

impl From<&NodeKeypair> for SerializableKeypair {
    fn from(kp: &NodeKeypair) -> Self {
        SerializableKeypair {
            public_b64: general_purpose::STANDARD.encode(&kp.verifying.to_bytes()),
            private_b64: general_purpose::STANDARD.encode(&kp.signing.to_bytes()),
            created_at_unix: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0),
        }
    }
}

impl SerializableKeypair {
    pub fn to_node_keypair(&self) -> Result<NodeKeypair> {
        let signing_bytes: [u8; 32] =
            general_purpose::STANDARD.decode(&self.private_b64)?[..].try_into()?;
        let verifying_bytes: [u8; 32] =
            general_purpose::STANDARD.decode(&self.public_b64)?[..].try_into()?;

        let signing = SigningKey::from_bytes(&signing_bytes);
        let verifying = VerifyingKey::from_bytes(&verifying_bytes)?;

        Ok(NodeKeypair { signing, verifying })
    }
}

pub fn generate_keypair() -> Result<NodeKeypair> {
    let mut secret_bytes = [0u8; SECRET_KEY_LENGTH];
    OsRng.try_fill_bytes(&mut secret_bytes);

    let signing = SigningKey::from_bytes(&secret_bytes);
    let verifying = VerifyingKey::from(&signing);

    Ok(NodeKeypair { signing, verifying })
}

pub fn save_keypair(kp: &NodeKeypair, path: Option<&Path>) -> Result<()> {
    let path = path
        .map(|p| p.to_path_buf())
        .unwrap_or_else(default_key_path);

    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }

    let json = serde_json::to_string_pretty(&SerializableKeypair::from(kp))?;

    let tmp_path = path.with_extension("tmp");
    {
        let mut f = File::create(&tmp_path)?;
        f.write_all(json.as_bytes())?;
    }
    fs::set_permissions(&tmp_path, fs::Permissions::from_mode(0o600))?;
    fs::rename(tmp_path, path)?;
    Ok(())
}

pub fn load_keypair(path: Option<&Path>) -> Result<NodeKeypair> {
    let path = path
        .map(|p| p.to_path_buf())
        .unwrap_or_else(default_key_path);
    let data = fs::read_to_string(&path)?;
    let ser: SerializableKeypair = serde_json::from_str(&data)?;
    ser.to_node_keypair()
}

pub fn node_id_from_pubkey(pubkey_bytes: &[u8]) -> NodeId {
    let h = blake3::hash(pubkey_bytes);
    h.to_hex().to_string()
}

pub fn node_id(kp: &NodeKeypair) -> NodeId {
    node_id_from_pubkey(&kp.verifying.to_bytes())
}

pub fn sign(kp: &NodeKeypair, msg: &[u8]) -> Result<Vec<u8>> {
    let sig: Signature = kp.signing.sign(msg);
    Ok(sig.to_bytes().to_vec())
}

pub fn verify(pubkey_bytes: &[u8; 32], msg: &[u8], sig_bytes: &[u8; 64]) -> Result<bool> {
    let verifying = VerifyingKey::from_bytes(pubkey_bytes)?;
    let sig = Signature::from_bytes(sig_bytes);
    Ok(verifying.verify_strict(msg, &sig).is_ok())
}

/// Building canonical handshake payload to sign:
/// e.g. b"HANDSHAKE" || node_id_bytes || timestamp_be_bytes
pub fn build_handshake_payload(node_id: &NodeId, timestamp_unix_secs: u64) -> Vec<u8> {
    let mut out = Vec::with_capacity(16 + node_id.len() + 8);
    out.extend_from_slice(b"HANDSHAKE");
    out.extend_from_slice(node_id.as_bytes());
    out.extend_from_slice(&timestamp_unix_secs.to_be_bytes());
    out
}

pub fn default_key_path() -> PathBuf {
    if let Some(mut dir) = dirs::config_dir() {
        dir.push("p2p_sync");
        dir.push("keys.json");
        dir
    } else {
        // fallback ~/.config/p2p_sync/keys.json
        let mut home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));
        home.push(".config/p2p_sync/keys.json");
        home
    }
}
