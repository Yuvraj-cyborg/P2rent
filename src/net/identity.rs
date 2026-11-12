use crate::error::{Error, Result};
use crate::hex;
use rcgen::{CertificateParams, KeyPair, PKCS_ED25519};
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};
use serde::{Deserialize, Serialize};
use std::fmt;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::str::FromStr;

/// DER prefix of an Ed25519 `SubjectPublicKeyInfo`; the raw key follows it.
const ED25519_SPKI_PREFIX: [u8; 12] = [
    0x30, 0x2a, 0x30, 0x05, 0x06, 0x03, 0x2b, 0x65, 0x70, 0x03, 0x21, 0x00,
];

/// A node's Ed25519 public key. This is the only name a node has.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct NodeId([u8; 32]);

impl NodeId {
    pub const fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    pub const fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }

    /// First eight hex digits, for log lines.
    pub fn short(&self) -> String {
        hex::encode(&self.0[..4])
    }

    /// Reads the node id out of a peer's TLS certificate.
    ///
    /// The certificate is parsed properly rather than searched for a byte
    /// pattern, so the key returned is the one the TLS handshake was verified
    /// against.
    pub(crate) fn from_certificate(certificate: &CertificateDer<'_>) -> Result<Self> {
        let parsed = webpki::EndEntityCert::try_from(certificate)
            .map_err(|e| Error::Identity(format!("unreadable peer certificate: {e}")))?;
        let spki = parsed.subject_public_key_info();
        let key = spki
            .strip_prefix(&ED25519_SPKI_PREFIX)
            .and_then(|key| <[u8; 32]>::try_from(key).ok())
            .ok_or_else(|| Error::Identity("peer certificate is not Ed25519".into()))?;
        Ok(Self(key))
    }
}

impl fmt::Display for NodeId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&hex::encode(&self.0))
    }
}

impl fmt::Debug for NodeId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "NodeId({})", self.short())
    }
}

impl FromStr for NodeId {
    type Err = Error;

    fn from_str(s: &str) -> Result<Self> {
        hex::decode_32(s)
            .map(Self)
            .ok_or_else(|| Error::Identity("a node id is 64 hex characters".into()))
    }
}

/// A node's long-lived Ed25519 key pair.
pub struct Identity {
    pkcs8: Vec<u8>,
    node_id: NodeId,
}

impl Identity {
    pub fn generate() -> Result<Self> {
        let key = KeyPair::generate_for(&PKCS_ED25519).map_err(key_error)?;
        Self::from_pkcs8(key.serialize_der())
    }

    pub fn from_pkcs8(pkcs8: Vec<u8>) -> Result<Self> {
        let key = parse_key(&pkcs8)?;
        let public = <[u8; 32]>::try_from(key.public_key_raw())
            .map_err(|_| Error::Identity("unexpected public key length".into()))?;
        Ok(Self {
            pkcs8,
            node_id: NodeId(public),
        })
    }

    /// Loads the key at `path`, creating it on first use.
    ///
    /// A file that exists but cannot be read is an error: silently replacing
    /// it would change the node's identity.
    pub fn load_or_create(path: &Path) -> Result<Self> {
        match std::fs::read_to_string(path) {
            Ok(text) => {
                let pkcs8 = hex::decode(text.trim()).ok_or_else(|| {
                    Error::Identity(format!("{} is not a valid key file", path.display()))
                })?;
                Self::from_pkcs8(pkcs8)
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                let identity = Self::generate()?;
                identity.save(path)?;
                Ok(identity)
            }
            Err(e) => Err(e.into()),
        }
    }

    /// `<config dir>/p2rent/node.key`.
    pub fn default_path() -> PathBuf {
        dirs::config_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join("p2rent")
            .join("node.key")
    }

    pub fn node_id(&self) -> NodeId {
        self.node_id
    }

    fn save(&self, path: &Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let temporary = path.with_extension("tmp");
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create(true).truncate(true);
        #[cfg(unix)]
        std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
        let mut file = options.open(&temporary)?;
        writeln!(file, "{}", hex::encode(&self.pkcs8))?;
        file.sync_all()?;
        std::fs::rename(&temporary, path)?;
        Ok(())
    }

    /// A fresh self-signed certificate for this key. The certificate carries
    /// no meaning beyond transporting the public key.
    pub(crate) fn tls_material(&self) -> Result<(CertificateDer<'static>, PrivateKeyDer<'static>)> {
        let key = parse_key(&self.pkcs8)?;
        let certificate = CertificateParams::new(vec!["p2rent".to_owned()])
            .and_then(|params| params.self_signed(&key))
            .map_err(key_error)?;
        let key = PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(self.pkcs8.clone()));
        Ok((certificate.der().clone(), key))
    }
}

impl fmt::Debug for Identity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Identity")
            .field("node_id", &self.node_id)
            .finish_non_exhaustive()
    }
}

fn parse_key(pkcs8: &[u8]) -> Result<KeyPair> {
    KeyPair::from_pkcs8_der_and_sign_algo(&PrivatePkcs8KeyDer::from(pkcs8), &PKCS_ED25519)
        .map_err(key_error)
}

fn key_error(e: rcgen::Error) -> Error {
    Error::Identity(e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn key_file_round_trips_and_is_private() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("nested").join("node.key");

        let created = Identity::load_or_create(&path).unwrap();
        let loaded = Identity::load_or_create(&path).unwrap();
        assert_eq!(created.node_id(), loaded.node_id());

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
        }
    }

    #[test]
    fn a_corrupt_key_file_is_not_overwritten() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("node.key");
        std::fs::write(&path, "not a key").unwrap();
        assert!(Identity::load_or_create(&path).is_err());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "not a key");
    }

    #[test]
    fn certificate_carries_the_node_id() {
        let identity = Identity::generate().unwrap();
        let (certificate, _) = identity.tls_material().unwrap();
        assert_eq!(
            NodeId::from_certificate(&certificate).unwrap(),
            identity.node_id()
        );
        assert_ne!(Identity::generate().unwrap().node_id(), identity.node_id());
    }

    #[test]
    fn node_id_parses_from_hex() {
        let id = Identity::generate().unwrap().node_id();
        assert_eq!(id.to_string().parse::<NodeId>().unwrap(), id);
        assert!("abc".parse::<NodeId>().is_err());
    }
}
