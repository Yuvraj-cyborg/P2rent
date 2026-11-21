//! Properties of tickets and locations, checked on generated inputs.

use model::Error;
use model::access::Secret;
use model::artifact::Hash;
use model::location::{Location, Protocol};
use model::ticket::{NodeId, Ticket, TicketPeer, Whereabouts};
use proptest::collection::vec;
use proptest::prelude::*;
use std::fmt::Write;
use std::net::IpAddr;

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

fn parse(text: &str) -> Result<Location, Error> {
    Location::parse(text, &EVERY)
}

fn hash() -> impl Strategy<Value = Hash> {
    any::<[u8; 32]>().prop_map(Hash::from_bytes)
}

fn node() -> impl Strategy<Value = NodeId> {
    any::<[u8; 32]>().prop_map(NodeId::from_bytes)
}

/// A host as it may be written in a ticket or a web address: an IP address
/// or a DNS name.
fn host() -> impl Strategy<Value = String> {
    prop_oneof![
        any::<[u8; 4]>().prop_map(|b| IpAddr::from(b).to_string()),
        any::<[u8; 16]>().prop_map(|b| format!("[{}]", IpAddr::from(b))),
        "[a-z][a-z0-9-]{0,12}(\\.[a-z][a-z0-9-]{0,8}){0,2}",
    ]
}

/// Where a ticket may say a peer is: at an address, by way of a relay, or
/// nothing at all.
fn whereabouts() -> impl Strategy<Value = Option<Whereabouts>> {
    let address = (host(), any::<u16>())
        .prop_map(|(host, port)| Whereabouts::Address(format!("{host}:{port}")));
    let relay = (any::<bool>(), host(), proptest::option::of(any::<u16>())).prop_map(
        |(secure, host, port)| {
            let scheme = if secure { "https" } else { "http" };
            let port = port.map_or(String::new(), |port| format!(":{port}"));
            Whereabouts::Relay(format!("{scheme}://{host}{port}/"))
        },
    );
    proptest::option::of(prop_oneof![address, relay])
}

fn ticket() -> impl Strategy<Value = Ticket> {
    let peer = (node(), whereabouts()).prop_map(|(node, at)| TicketPeer { node, at });
    let origins = vec(web_address().prop_map(|(_, kept)| kept), 0..3);
    let access = (
        any::<[u8; 16]>().prop_map(Secret::from_bytes),
        any::<bool>(),
    );
    (hash(), access, vec(peer, 0..5), origins).prop_map(
        |(artifact, (secret, swarm), peers, origins)| Ticket {
            artifact,
            secret,
            swarm,
            peers,
            origins,
        },
    )
}

/// A path as a person types it, with no scheme: relative, absolute, or on a
/// Windows drive.
fn path() -> impl Strategy<Value = String> {
    let start = prop_oneof![
        Just(""),
        Just("/"),
        Just("./"),
        Just("../"),
        Just("C:\\"),
        Just("c:/"),
        Just("\\\\server\\"),
    ];
    let separator = prop_oneof![Just("/"), Just("\\")];
    (start, vec("[^/\\\\:\\x00]{1,8}", 1..4), separator)
        .prop_map(|(start, names, separator)| format!("{start}{}", names.join(separator)))
        .prop_filter("a lone dash is a pipe", |path| path != "-")
}

/// A `file:` URL, with the path it holds.
fn file_url() -> impl Strategy<Value = (String, String)> {
    let authority = prop_oneof![Just("//"), Just("//localhost"), Just("")];
    (authority, vec("[^/:\\x00]{1,6}", 1..4)).prop_map(|(authority, names)| {
        let path = format!("/{}", names.join("/"));
        let mut url = format!("file:{authority}");
        for byte in path.bytes() {
            if byte.is_ascii_alphanumeric() || b"/._~-".contains(&byte) {
                url.push(char::from(byte));
            } else {
                write!(url, "%{byte:02X}").unwrap();
            }
        }
        (url, path)
    })
}

/// A web address, with the text it is kept as.
fn web_address() -> impl Strategy<Value = (String, String)> {
    let scheme = prop_oneof![Just("http"), Just("https"), Just("HTTP"), Just("Https")];
    let port = prop_oneof![
        Just(String::new()),
        any::<u16>().prop_map(|p| format!(":{p}"))
    ];
    let rest = prop_oneof![Just(String::new()), "[/?#][!-~]{0,24}"];
    (scheme, host(), port, rest).prop_map(|(scheme, host, port, rest)| {
        let after = format!("://{host}{port}{rest}");
        (
            format!("{scheme}{after}"),
            format!("{}{after}", scheme.to_ascii_lowercase()),
        )
    })
}

/// Every written form a location has, with the place it names.
fn written() -> impl Strategy<Value = (String, Location)> {
    prop_oneof![
        path().prop_map(|path| (path.clone(), Location::Local(path))),
        file_url().prop_map(|(url, path)| (url, Location::Local(path))),
        web_address().prop_map(|(url, kept)| (url, Location::Web(kept))),
        ticket().prop_map(|ticket| (ticket.to_string(), Location::Ticket(ticket))),
        Just(("peer:".to_owned(), Location::Serve)),
    ]
}

/// Text that starts like a location of some kind and then goes wrong.
fn near_location() -> impl Strategy<Value = String> {
    let start = prop_oneof![
        Just("file:"),
        Just("file://"),
        Just("https://"),
        Just("http:"),
        Just("mux://"),
        Just("peer:"),
        Just("s3://"),
        Just("C:"),
        Just(""),
    ];
    (start, "[ -~%é\\x00]{0,60}").prop_map(|(start, tail)| format!("{start}{tail}"))
}

proptest! {
    #[test]
    fn a_ticket_survives_printing_and_parsing(ticket in ticket()) {
        let text = ticket.to_string();
        prop_assert_eq!(text.parse::<Ticket>().unwrap(), ticket);
    }

    #[test]
    fn parsing_a_ticket_never_panics(text in ".{0,200}") {
        if let Ok(ticket) = text.parse::<Ticket>() {
            // Anything accepted prints to something that parses to the same
            // ticket: nothing is silently dropped or reinterpreted.
            prop_assert_eq!(ticket.to_string().parse::<Ticket>().unwrap(), ticket);
        }
    }

    #[test]
    fn parsing_a_near_ticket_never_panics(artifact in hash(), tail in "[ -~]{0,120}") {
        // With a secret and without: most of what a ticket can hold is
        // read only once it has one.
        let secret = format!("?secret={}", "5e".repeat(16));
        for start in ["", secret.as_str()] {
            let text = format!("mux://{artifact}{start}{tail}");
            if let Ok(ticket) = text.parse::<Ticket>() {
                prop_assert_eq!(ticket.artifact, artifact);
                prop_assert_eq!(ticket.to_string().parse::<Ticket>().unwrap(), ticket);
            }
        }
    }

    #[test]
    fn every_written_form_names_its_place_and_prints_back((text, place) in written()) {
        prop_assert_eq!(parse(&text), Ok(place.clone()), "{:?}", text);
        prop_assert_eq!(parse(&place.to_string()), Ok(place));
    }

    /// Whatever is typed, the answer is a place that prints back to itself
    /// or a sentence that says what is wrong.
    #[test]
    fn reading_a_location_never_panics(text in prop_oneof![".{0,120}", near_location()]) {
        match parse(&text) {
            Ok(place) => prop_assert_eq!(parse(&place.to_string()), Ok(place)),
            Err(error) => prop_assert!(error.to_string().len() > 20, "{}", error),
        }
    }

    /// A build is refused nothing it lists and granted nothing it lacks.
    #[test]
    fn a_build_reads_only_what_it_lists((text, _) in written(), keep in vec(any::<bool>(), 5)) {
        let listed: Vec<Protocol> = EVERY.iter().zip(&keep).filter(|(_, keep)| **keep).map(|(p, _)| *p).collect();
        let scheme = match parse(&text).unwrap() {
            Location::Local(_) => "file",
            Location::Web(url) if url.starts_with("https:") => "https",
            Location::Web(_) => "http",
            Location::Ticket(_) => "mux",
            Location::Serve => "peer",
        };
        let has = listed.iter().any(|protocol| protocol.scheme == scheme);
        prop_assert_eq!(Location::parse(&text, &listed).is_ok(), has);
    }
}
