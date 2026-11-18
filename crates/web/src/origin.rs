use crate::headers::{self, Validator};
use bytes::{Buf, Bytes};
use model::artifact::{Entry, EntryKind, EntryMetadata, FileDigest, Timestamp};
use model::place::{Capabilities, Description, Failure, Foreknown, Pending, Reader, Source};
use reqwest::header::{
    ACCEPT_ENCODING, CONTENT_ENCODING, CONTENT_LENGTH, CONTENT_RANGE, ETAG, HeaderMap, IF_RANGE,
    LAST_MODIFIED, RANGE,
};
use reqwest::{Client, Response, StatusCode, Url};
use rustls_platform_verifier::BuilderVerifierExt;
use std::sync::Arc;
use std::time::Duration;

/// The name a file is stored under when its address gives none.
const UNNAMED: &str = "download";

/// How an origin is asked.
#[derive(Debug, Clone)]
pub struct Options {
    /// How long to wait for a connection.
    pub connect_timeout: Duration,
    /// How long to wait for the next bytes of an answer before taking the
    /// connection for dead.
    pub read_timeout: Duration,
    /// The digest of the whole file, if the user gave one. It is believed
    /// before one the server gives.
    pub digest: Option<FileDigest>,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            connect_timeout: Duration::from_secs(10),
            read_timeout: Duration::from_secs(30),
            digest: None,
        }
    }
}

/// A file at a web address.
///
/// What the server said of the file when it was first asked is kept, and
/// every later answer is held to it: a server that begins to answer with
/// another version of the file is reported as having changed.
#[derive(Debug, Clone)]
pub struct Origin(Arc<Inner>);

#[derive(Debug)]
struct Inner {
    client: Client,
    url: Url,
    found: Found,
    given: Option<FileDigest>,
}

/// What the server said of the file when it was first asked.
#[derive(Debug)]
struct Found {
    size: u64,
    /// Whether the server sends a part of the file when asked for one.
    ranges: bool,
    validator: Option<Validator>,
    modified: Option<Timestamp>,
    digest: Option<FileDigest>,
}

impl Origin {
    /// Finds out what is at `url` by asking for its first byte: how large
    /// the file is, whether the server will send a part of it, and what
    /// tells this version of it from another.
    ///
    /// Redirects are followed, and a proxy named in the environment is
    /// used.
    ///
    /// # Errors
    ///
    /// Returns a failure if `url` is not a web address, if nothing is
    /// there, or if the server does not say how large the file is.
    pub async fn find(url: &str, options: &Options) -> Result<Self, Failure> {
        let url = Url::parse(url)
            .map_err(|e| Failure::Fatal(format!("{url:?} is not a web address: {e}")))?;
        let client = client(options)?;
        let response = get(&client, &url, Some((0, Some(0))), None).await?;
        let found = Found::from(&response, &url)?;
        Ok(Self(Arc::new(Inner {
            client,
            url,
            found,
            given: options.digest,
        })))
    }

    /// The name to store the file under.
    fn name(&self) -> String {
        headers::name_from(self.0.url.path()).unwrap_or_else(|| UNNAMED.to_owned())
    }

    /// Whether a part of the file can be asked for and known to be of the
    /// version that was found. Without both, the file is one stream.
    fn is_random_access(&self) -> bool {
        self.0.found.ranges && self.0.found.validator.is_some()
    }

    /// Whether the bytes of two answers can be put together. Something has
    /// to tell that they are of one version: a validator, which every
    /// answer is held to, or a digest of the whole file, which is checked
    /// at the end.
    fn can_be_joined(&self) -> bool {
        self.0.found.validator.is_some() || self.digest().is_some()
    }

    /// The digest of the whole file: the one the user gave, or else the
    /// one the server sent.
    fn digest(&self) -> Option<FileDigest> {
        self.0.given.or(self.0.found.digest)
    }
}

impl Found {
    fn from(response: &Response, url: &Url) -> Result<Self, Failure> {
        let headers = response.headers();
        let status = response.status();
        let (size, ranges) = match status {
            StatusCode::PARTIAL_CONTENT => {
                let range = text(headers, CONTENT_RANGE).and_then(headers::content_range);
                let total = range.filter(|range| range.first == 0).and_then(|r| r.total);
                (total, true)
            }
            // An empty file has no first byte to send.
            StatusCode::RANGE_NOT_SATISFIABLE => {
                let total = text(headers, CONTENT_RANGE).and_then(headers::unsatisfied_total);
                (total.filter(|&total| total == 0), false)
            }
            StatusCode::OK => (length(headers), false),
            _ => return Err(refused(status, url)),
        };
        let size = size.ok_or_else(|| {
            Failure::Fatal(format!(
                "the server at {url} does not say how large the file is, which mux needs to know"
            ))
        })?;
        Ok(Self {
            size,
            ranges,
            validator: Validator::from(text(headers, ETAG), text(headers, LAST_MODIFIED)),
            modified: text(headers, LAST_MODIFIED).and_then(headers::modified),
            digest: headers::sha256(
                text_named(headers, "repr-digest"),
                text_named(headers, "digest"),
            ),
        })
    }
}

impl Source for Origin {
    fn capabilities(&self) -> Capabilities {
        Capabilities {
            random_access: self.is_random_access(),
            // Each request is a connection to the server.
            multiplexed: false,
        }
    }

    fn describe(&self) -> Pending<'_, Description> {
        let found = &self.0.found;
        let file = Entry {
            path: String::new(),
            kind: EntryKind::File { size: found.size },
        };
        let recorded = EntryMetadata {
            mode: 0o644,
            modified: found.modified,
        };
        let known = Foreknown {
            validator: found
                .validator
                .as_ref()
                .map(Validator::to_bytes)
                .unwrap_or_default(),
            digest: self.digest(),
        };
        Box::pin(std::future::ready(Ok(Description {
            name: self.name(),
            entries: vec![(file, recorded)],
            known: vec![known],
        })))
    }

    fn open(&self, file: usize, offset: u64) -> Pending<'_, Box<dyn Reader>> {
        Box::pin(async move {
            let (inner, found) = (&self.0, &self.0.found);
            if file != 0 || offset >= found.size {
                return Err(Failure::Absent(format!(
                    "{} is one file of {} bytes",
                    inner.url, found.size
                )));
            }
            if offset > 0 && !self.can_be_joined() {
                return Err(Failure::Fatal(format!(
                    "{} cannot be carried on with: the server gives nothing to tell whether \
                     the file is still the one that was begun, and no digest of it is known",
                    inner.url
                )));
            }
            let partial = self.is_random_access();
            let range = partial.then_some((offset, None));
            let condition = found.validator.as_ref().filter(|_| partial);
            let response = get(&inner.client, &inner.url, range, condition).await?;
            let mut body = Body {
                response,
                arrived: Bytes::new(),
            };
            self.check(&body.response, partial, offset)?;
            if !partial {
                // The server sends the whole file or nothing: read past
                // what is not wanted.
                body.skip(offset).await?;
            }
            let reader: Box<dyn Reader> = Box::new(body);
            Ok(reader)
        })
    }
}

impl Origin {
    /// Holds an answer to what was found when the file was first asked
    /// for: the same version, and the part that was asked for.
    fn check(&self, response: &Response, partial: bool, offset: u64) -> Result<(), Failure> {
        let (inner, found) = (&self.0, &self.0.found);
        let (headers, status) = (response.headers(), response.status());
        let changed = || {
            Failure::Changed(format!(
                "{} has changed on the server since the transfer began",
                inner.url
            ))
        };
        let same_version = found
            .validator
            .as_ref()
            .is_none_or(|v| v.still_holds(text(headers, ETAG), text(headers, LAST_MODIFIED)));
        match status {
            // The whole file, where a part was asked for on condition that
            // the file had not changed.
            StatusCode::OK if partial => Err(changed()),
            StatusCode::OK if !same_version || length(headers) != Some(found.size) => {
                Err(changed())
            }
            StatusCode::OK => Ok(()),
            StatusCode::PARTIAL_CONTENT if !partial => Err(Failure::Corrupt(format!(
                "{} sent a part of the file when the whole of it was asked for",
                inner.url
            ))),
            StatusCode::PARTIAL_CONTENT => {
                let range = text(headers, CONTENT_RANGE).and_then(headers::content_range);
                let Some(range) = range else {
                    return Err(Failure::Corrupt(format!(
                        "{} sent a part of the file without saying which",
                        inner.url
                    )));
                };
                if !same_version || range.total.is_some_and(|total| total != found.size) {
                    return Err(changed());
                }
                if range.first != offset || range.last + 1 != found.size {
                    return Err(Failure::Corrupt(format!(
                        "{} was asked for the bytes from {offset} and sent those from {} to {}",
                        inner.url, range.first, range.last
                    )));
                }
                Ok(())
            }
            _ => Err(refused(status, &inner.url)),
        }
    }
}

/// The body of one answer, read in order.
#[derive(Debug)]
struct Body {
    response: Response,
    /// What has arrived and not been handed over yet.
    arrived: Bytes,
}

impl Body {
    /// What has arrived of the body, after waiting for more if there is
    /// none.
    async fn arrived(&mut self) -> Result<&mut Bytes, Failure> {
        while self.arrived.is_empty() {
            self.arrived = match self.response.chunk().await {
                Ok(Some(bytes)) => bytes,
                Ok(None) => {
                    return Err(Failure::Unreachable(format!(
                        "{} closed the connection before the end of the file",
                        self.response.url()
                    )));
                }
                Err(e) => return Err(unreachable(&e)),
            };
        }
        Ok(&mut self.arrived)
    }

    /// Reads past `count` bytes without keeping them.
    async fn skip(&mut self, mut count: u64) -> Result<(), Failure> {
        while count > 0 {
            let arrived = self.arrived().await?;
            let passed = arrived
                .len()
                .min(usize::try_from(count).unwrap_or(usize::MAX));
            arrived.advance(passed);
            count -= passed as u64;
        }
        Ok(())
    }
}

impl Reader for Body {
    fn read(&mut self, len: u32) -> Pending<'_, Vec<u8>> {
        Box::pin(async move {
            // Each byte is copied once, from where it arrived to where it
            // is handed over.
            let len = len as usize;
            let mut read = Vec::with_capacity(len);
            while read.len() < len {
                let arrived = self.arrived().await?;
                let taken = arrived.len().min(len - read.len());
                read.extend_from_slice(&arrived.split_to(taken));
            }
            Ok(read)
        })
    }
}

/// A client that follows redirects, uses a proxy named in the environment,
/// and trusts the certificates this system trusts.
fn client(options: &Options) -> Result<Client, Failure> {
    let cannot =
        |what: &dyn std::fmt::Display| Failure::Fatal(format!("cannot set up TLS: {what}"));
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let tls = rustls::ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .map_err(|e| cannot(&e))?
        .with_platform_verifier()
        .map_err(|e| cannot(&e))?
        .with_no_client_auth();
    Client::builder()
        .tls_backend_preconfigured(tls)
        .connect_timeout(options.connect_timeout)
        .read_timeout(options.read_timeout)
        .user_agent(concat!("mux/", env!("CARGO_PKG_VERSION")))
        .build()
        .map_err(|e| Failure::Fatal(format!("cannot set up a web client: {e}")))
}

/// Asks for the file, or for the part of it from `range`, on condition
/// that it is still the version `validator` stands for.
async fn get(
    client: &Client,
    url: &Url,
    range: Option<(u64, Option<u64>)>,
    validator: Option<&Validator>,
) -> Result<Response, Failure> {
    // Bytes are counted as they are stored, so they must arrive as they
    // are stored and not compressed on the way.
    let mut request = client.get(url.clone()).header(ACCEPT_ENCODING, "identity");
    if let Some((first, last)) = range {
        let last = last.map(|last| last.to_string()).unwrap_or_default();
        request = request.header(RANGE, format!("bytes={first}-{last}"));
    }
    if let Some(validator) = validator {
        request = request.header(IF_RANGE, validator.as_header());
    }
    request.send().await.map_err(|e| unreachable(&e))
}

/// What a status that is not the file means for the transfer.
fn refused(status: StatusCode, url: &Url) -> Failure {
    let said = format!("the server at {url} answered {status}");
    match status {
        StatusCode::NOT_FOUND | StatusCode::GONE => {
            Failure::Absent(format!("{said}: nothing is at that address"))
        }
        StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN => Failure::Fatal(format!(
            "{said}: the file needs a login, which mux does not do"
        )),
        StatusCode::REQUEST_TIMEOUT | StatusCode::TOO_MANY_REQUESTS => Failure::Unreachable(said),
        status if status.is_server_error() => Failure::Unreachable(said),
        _ => Failure::Fatal(said),
    }
}

/// A request that did not get its answer. Most are worth trying again.
fn unreachable(error: &reqwest::Error) -> Failure {
    let said = match error.url() {
        Some(url) => format!("{url} could not be read: {error}"),
        None => format!("the request failed: {error}"),
    };
    if error.is_redirect() || error.is_builder() {
        Failure::Fatal(said)
    } else {
        Failure::Unreachable(said)
    }
}

/// The length of an answer's body, when it is the file as stored.
fn length(headers: &HeaderMap) -> Option<u64> {
    if headers.contains_key(CONTENT_ENCODING) {
        return None;
    }
    text(headers, CONTENT_LENGTH)?.trim().parse().ok()
}

fn text(headers: &HeaderMap, name: reqwest::header::HeaderName) -> Option<&str> {
    headers.get(name)?.to_str().ok()
}

fn text_named<'a>(headers: &'a HeaderMap, name: &str) -> Option<&'a str> {
    headers.get(name)?.to_str().ok()
}
