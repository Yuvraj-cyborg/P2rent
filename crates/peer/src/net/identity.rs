use crate::error::{Error, Result};
use model::hex;
use model::ticket::NodeId;
use rcgen::{CertificateParams, KeyPair, PKCS_ED25519};
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};
use std::fmt;
use std::io::Write;
use std::path::{Path, PathBuf};

/// DER prefix of an Ed25519 `SubjectPublicKeyInfo`; the raw key follows it.
const ED25519_SPKI_PREFIX: [u8; 12] = [
    0x30, 0x2a, 0x30, 0x05, 0x06, 0x03, 0x2b, 0x65, 0x70, 0x03, 0x21, 0x00,
];

/// Reads the node id out of a peer's TLS certificate.
///
/// The certificate is parsed properly rather than searched for a byte
/// pattern, so the key returned is the one the TLS handshake was verified
/// against.
pub(crate) fn node_of(certificate: &CertificateDer<'_>) -> Result<NodeId> {
    let parsed = webpki::EndEntityCert::try_from(certificate)
        .map_err(|e| Error::Identity(format!("unreadable peer certificate: {e}")))?;
    let spki = parsed.subject_public_key_info();
    let key = spki
        .strip_prefix(&ED25519_SPKI_PREFIX)
        .and_then(|key| <[u8; 32]>::try_from(key).ok())
        .ok_or_else(|| Error::Identity("peer certificate is not Ed25519".into()))?;
    Ok(NodeId::from_bytes(key))
}

/// A node's long-lived Ed25519 key pair.
pub struct Identity {
    pkcs8: Vec<u8>,
    node_id: NodeId,
}

impl Identity {
    /// Creates a new random key pair.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Identity`] if key generation fails.
    pub fn generate() -> Result<Self> {
        let key = KeyPair::generate_for(&PKCS_ED25519)?;
        Self::from_pkcs8(key.serialize_der())
    }

    /// Loads a key pair from PKCS#8 DER bytes.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Identity`] if the bytes are not an Ed25519 private key.
    pub fn from_pkcs8(pkcs8: Vec<u8>) -> Result<Self> {
        let key = parse_key(&pkcs8)?;
        let public = <[u8; 32]>::try_from(key.public_key_raw())
            .map_err(|_| Error::Identity("unexpected public key length".into()))?;
        Ok(Self {
            pkcs8,
            node_id: NodeId::from_bytes(public),
        })
    }

    /// Loads the key at `path`, creating it on first use.
    ///
    /// A file that exists but cannot be read is an error: silently replacing
    /// it would change the node's identity.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Identity`] if the file exists but does not hold a valid key, and
    /// [`Error::Io`] if it cannot be read or created.
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

    /// `<config dir>/mux/node.key`.
    pub fn default_path() -> PathBuf {
        key_path("mux")
    }

    /// Loads this machine's key from [`Identity::default_path`], creating it
    /// on first use.
    ///
    /// The project was called p2rent before it was mux, and kept the key in
    /// a folder of that name. A machine with a key there and none in the new
    /// place keeps its identity: the key is copied across.
    ///
    /// # Errors
    ///
    /// As for [`Identity::load_or_create`].
    pub fn load_or_create_default() -> Result<Self> {
        Self::load_or_adopt(&Self::default_path(), &key_path("p2rent"))
    }

    /// Loads the key at `path`. If there is none, adopts the one at `former`
    /// by copying it to `path`. If there is none there either, creates a new
    /// key at `path`.
    fn load_or_adopt(path: &Path, former: &Path) -> Result<Self> {
        if path.exists() || !former.exists() {
            return Self::load_or_create(path);
        }
        let identity = Self::load_or_create(former)?;
        identity.save(path)?;
        Ok(identity)
    }

    /// The public half, which names this node.
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
        let certificate = CertificateParams::new(vec!["mux".to_owned()])
            .and_then(|params| params.self_signed(&key))?;
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

/// Where the key lives for a project of the given name.
fn key_path(project: &str) -> PathBuf {
    dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(project)
        .join("node.key")
}

fn parse_key(pkcs8: &[u8]) -> Result<KeyPair> {
    KeyPair::from_pkcs8_der_and_sign_algo(&PrivatePkcs8KeyDer::from(pkcs8), &PKCS_ED25519)
        .map_err(Error::from)
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
    fn a_key_from_the_former_folder_is_adopted() {
        let tmp = tempfile::tempdir().unwrap();
        let former = tmp.path().join("p2rent").join("node.key");
        let path = tmp.path().join("mux").join("node.key");
        let original = Identity::load_or_create(&former).unwrap();

        // No key in the new place yet: the former one is copied across.
        let adopted = Identity::load_or_adopt(&path, &former).unwrap();
        assert_eq!(adopted.node_id(), original.node_id());
        assert!(path.exists());

        // From then on the new place is used, whatever happens to the old.
        std::fs::remove_file(&former).unwrap();
        let again = Identity::load_or_adopt(&path, &former).unwrap();
        assert_eq!(again.node_id(), original.node_id());
    }

    #[test]
    fn a_key_in_the_new_folder_wins_over_the_former_one() {
        let tmp = tempfile::tempdir().unwrap();
        let former = tmp.path().join("p2rent").join("node.key");
        let path = tmp.path().join("mux").join("node.key");
        let old = Identity::load_or_create(&former).unwrap();
        let new = Identity::load_or_create(&path).unwrap();

        let loaded = Identity::load_or_adopt(&path, &former).unwrap();
        assert_eq!(loaded.node_id(), new.node_id());
        assert_ne!(loaded.node_id(), old.node_id());
    }

    #[test]
    fn with_no_key_anywhere_a_new_one_is_created_in_the_new_folder() {
        let tmp = tempfile::tempdir().unwrap();
        let former = tmp.path().join("p2rent").join("node.key");
        let path = tmp.path().join("mux").join("node.key");

        Identity::load_or_adopt(&path, &former).unwrap();
        assert!(path.exists());
        assert!(!former.exists());
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
        assert_eq!(node_of(&certificate).unwrap(), identity.node_id());
        assert_ne!(Identity::generate().unwrap().node_id(), identity.node_id());
    }

    #[test]
    fn node_id_parses_from_hex() {
        let id = Identity::generate().unwrap().node_id();
        assert_eq!(id.to_string().parse::<NodeId>().unwrap(), id);
        assert!("abc".parse::<NodeId>().is_err());
    }
}
