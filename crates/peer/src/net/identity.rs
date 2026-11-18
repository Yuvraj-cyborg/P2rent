use crate::error::{Error, Result};
use iroh::SecretKey;
use model::hex;
use model::ticket::NodeId;
use std::fmt;
use std::io::Write;
use std::path::{Path, PathBuf};

/// What comes before the 32 bytes of an Ed25519 private key in PKCS#8: the
/// form with the key alone, which is the one written, and the form that
/// goes on to give the public key as well, which earlier versions wrote.
const KEY_ALONE: [u8; 16] = [
    0x30, 0x2e, 0x02, 0x01, 0x00, 0x30, 0x05, 0x06, 0x03, 0x2b, 0x65, 0x70, 0x04, 0x22, 0x04, 0x20,
];
const KEY_WITH_PUBLIC: [u8; 16] = [
    0x30, 0x51, 0x02, 0x01, 0x01, 0x30, 0x05, 0x06, 0x03, 0x2b, 0x65, 0x70, 0x04, 0x22, 0x04, 0x20,
];
/// What comes between the private key and the public one, in that form.
const PUBLIC_FOLLOWS: [u8; 3] = [0x81, 0x21, 0x00];

/// The private key in PKCS#8 bytes, and the public key if they give one.
fn read_pkcs8(pkcs8: &[u8]) -> Option<([u8; 32], Option<[u8; 32]>)> {
    let (form, rest) = pkcs8.split_first_chunk::<16>()?;
    let (secret, rest) = rest.split_first_chunk::<32>()?;
    if *form == KEY_ALONE && rest.is_empty() {
        return Some((*secret, None));
    }
    let public = rest.strip_prefix(&PUBLIC_FOLLOWS)?;
    let public = <[u8; 32]>::try_from(public).ok()?;
    (*form == KEY_WITH_PUBLIC).then_some((*secret, Some(public)))
}

/// A node's long-lived Ed25519 key pair.
pub struct Identity {
    secret: SecretKey,
    node_id: NodeId,
}

impl Identity {
    /// Creates a new random key pair.
    pub fn generate() -> Self {
        Self::of(SecretKey::generate())
    }

    fn of(secret: SecretKey) -> Self {
        let node_id = NodeId::from_bytes(*secret.public().as_bytes());
        Self { secret, node_id }
    }

    /// Loads a key pair from PKCS#8 DER bytes.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Identity`] if the bytes are not an Ed25519 private
    /// key, or give a public key that is not the one the private key has.
    pub fn from_pkcs8(pkcs8: &[u8]) -> Result<Self> {
        let (secret, public) = read_pkcs8(pkcs8)
            .ok_or_else(|| Error::Identity("not an Ed25519 private key".into()))?;
        let identity = Self::of(SecretKey::from_bytes(&secret));
        if public.is_some_and(|public| public != *identity.node_id.as_bytes()) {
            return Err(Error::Identity(
                "the public key in the file is not the one its private key has".into(),
            ));
        }
        Ok(identity)
    }

    /// The key pair as PKCS#8 DER bytes.
    fn to_pkcs8(&self) -> Vec<u8> {
        [&KEY_ALONE[..], &self.secret.to_bytes()].concat()
    }

    /// The private half, which this node proves itself with.
    pub(crate) fn secret(&self) -> &SecretKey {
        &self.secret
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
                Self::from_pkcs8(&pkcs8)
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                let identity = Self::generate();
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
        writeln!(file, "{}", hex::encode(&self.to_pkcs8()))?;
        file.sync_all()?;
        std::fs::rename(&temporary, path)?;
        Ok(())
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

    /// A key file as version 0.2.0 wrote it, and the id of the node it
    /// was the key of.
    const FORMER_KEY: &str = "3051020101300506032b657004220420\
        a44b6672ba09904d92234c220c7c815aabca1d82fb61dcc0e276730b9be1daa4\
        812100\
        43640c5cddbb915279ef24e1f6ca113d5c1a278b856f99943a652ae5748e9f18";
    const FORMER_NODE: &str = "43640c5cddbb915279ef24e1f6ca113d5c1a278b856f99943a652ae5748e9f18";

    #[test]
    fn a_key_file_of_an_earlier_version_keeps_its_node_id() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("node.key");
        std::fs::write(&path, format!("{FORMER_KEY}\n")).unwrap();

        let loaded = Identity::load_or_create(&path).unwrap();
        assert_eq!(loaded.node_id().to_string(), FORMER_NODE);
        // Loading leaves the file as it was.
        let kept = std::fs::read_to_string(&path).unwrap();
        assert_eq!(kept.trim(), FORMER_KEY);
    }

    #[test]
    fn a_key_is_written_alone_and_read_back_as_the_same_node() {
        let identity = Identity::generate();
        let written = identity.to_pkcs8();
        assert_eq!(written.len(), 48);
        let read = Identity::from_pkcs8(&written).unwrap();
        assert_eq!(read.node_id(), identity.node_id());
        assert_eq!(read.secret().to_bytes(), identity.secret().to_bytes());
        assert_ne!(Identity::generate().node_id(), identity.node_id());
    }

    #[test]
    fn what_is_not_a_key_is_not_read_as_one() {
        let former = hex::decode(FORMER_KEY).unwrap();
        let alone = Identity::generate().to_pkcs8();
        let changed = |bytes: &[u8], at: usize| {
            let mut bytes = bytes.to_vec();
            bytes[at] ^= 1;
            bytes
        };
        for (bytes, why) in [
            (Vec::new(), "nothing"),
            (alone[..47].to_vec(), "cut short"),
            ([&alone[..], &[0]].concat(), "a byte too many"),
            (changed(&alone, 0), "another kind of thing"),
            (changed(&alone, 9), "another kind of key"),
            (former[..82].to_vec(), "cut short, with a public key"),
            (
                [&former[..], &[0]].concat(),
                "a byte too many, with a public key",
            ),
            (
                changed(&former, 49),
                "no public key where one is said to follow",
            ),
            (changed(&former, 82), "a public key of another private key"),
            (changed(&former, 20), "a private key of another public key"),
            // The form with a public key, of the length without one.
            (
                [&former[..16], &alone[16..]].concat(),
                "the wrong form for its length",
            ),
            (
                [&alone[..16], &former[16..]].concat(),
                "the wrong form for its length",
            ),
        ] {
            assert!(Identity::from_pkcs8(&bytes).is_err(), "{why}");
        }
    }

    #[test]
    fn node_id_parses_from_hex() {
        let id = Identity::generate().node_id();
        assert_eq!(id.to_string().parse::<NodeId>().unwrap(), id);
        assert!("abc".parse::<NodeId>().is_err());
    }
}
