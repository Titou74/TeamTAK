//! Map sources for ATAK, and the device profiles of TAK Server that hand them out.
//!
//! A map source is a small XML file (`customMapSource`) that tells ATAK where to fetch the
//! tiles of a map: its name, its zoom levels, its tile type and a URL template with `{$z}`,
//! `{$x}` and `{$y}`. ATAK lists it among its maps and fetches the tiles it needs while the map
//! is shown. Nothing of the map itself goes to the phone.
//!
//! The administrators keep a catalogue of map sources; a team's leaders pick the team's maps
//! from it, and an event's organisers the event's maps. TeamTAK then keeps, in TAK Server,
//! one device profile per TAK group that has maps:
//!
//! - `teamtak-maps-<group>`, applied when a device connects: TAK Server hands it to the
//!   devices of that group only (it matches the profile's groups with the groups of the
//!   device's connection). An event's profile exists while the event runs, like its group;
//! - `teamtak-enrollment`, applied when a device enrolls for its certificate, for every
//!   active team's group: it turns on ATAK's "Apply TAK Server profile updates", which is off
//!   by default, so that a device enrolled by QR code or by username and password asks for
//!   the connection profiles. A connection zip turns it on itself.
//!
//! Every map source is checked before it is handed to anyone ([`TileChecker`]): one tile of
//! it must answer with an image. Since a profile cannot take a map back from a phone, the
//! sources are checked again regularly, and one that fails twice in a row is held back until
//! it works again.
//!
//! TAK Server sends a profile again to a device only when it changed since the device's
//! last request (`syncSecago`); each file added or removed marks the profile as changed.
//! A profile adds files to a device and never removes any: a map taken off a team stays on
//! its phones until someone deletes it there.

use std::collections::{BTreeMap, BTreeSet};
use std::io::{Cursor, Read, Write};

use uuid::Uuid;

use crate::names::slugify;
use crate::tak::{DeviceProfile, ProfileFile, ProfileKind};

/// Largest map source file. Real ones are a few hundred bytes.
pub const MAX_SOURCE_BYTES: usize = 16 * 1024;
/// Largest upload (one XML file, or a zip of them).
pub const MAX_UPLOAD_BYTES: usize = 512 * 1024;
/// Files an upload may hold.
pub const MAX_UPLOAD_FILES: usize = 100;
/// Longest name of a map, as ATAK lists it.
pub const MAX_NAME_LEN: usize = 60;
/// Longest file name of a map source, `.xml` included.
pub const MAX_FILE_NAME_LEN: usize = 64;

/// Prefix of the device profiles TeamTAK keeps in TAK Server. A profile without it was made
/// by someone else, in TAK Server's own pages, and is left alone.
pub const PROFILE_PREFIX: &str = "teamtak-";
/// The profile handed to devices when they enroll.
pub const ENROLLMENT_PROFILE: &str = "teamtak-enrollment";
/// File of the enrollment profile that turns on connection profiles in ATAK.
pub const ENROLLMENT_PREF_FILE: &str = "teamtak-device-profiles.pref";
/// ATAK's preference "Apply TAK Server profile updates" (Network preferences, TAK Server
/// connections). Off by default in ATAK; while it is off, ATAK asks for no profile when it
/// connects.
pub const PROFILES_ON_CONNECT_PREF: &str = "deviceProfileEnableOnConnect";

/// Name of the connection profile of a TAK group.
pub fn maps_profile_name(group: &str) -> String {
    format!("{PROFILE_PREFIX}maps-{group}")
}

/// The group of a connection profile named by [`maps_profile_name`].
pub fn group_of_profile(name: &str) -> Option<&str> {
    name.strip_prefix(PROFILE_PREFIX)?.strip_prefix("maps-").filter(|g| !g.is_empty())
}

// ------------------------------------------------------------------------------- map source files

/// Why a file is not accepted as a map source. The code is stable: the UI translates it
/// (`maps.rejected.<code>`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum SourceError {
    #[error("the file is too large")]
    TooLarge,
    #[error("the file is not well-formed XML, or it declares a DTD")]
    NotXml,
    #[error("the root element is not customMapSource")]
    NotMapSource,
    #[error("the map has no name, or one that is too long")]
    BadName,
    #[error("the map has no tile URL, or one that is not http(s) with {{$z}} and {{$x}}/{{$y}} (or {{$q}})")]
    BadUrl,
    #[error("a zoom level is not a number from 0 to 30, or minZoom is above maxZoom")]
    BadZoom,
    #[error("the file is not a map source (.xml) or a zip of them")]
    NotAnUpload,
    #[error("the zip holds too many files, or is damaged")]
    BadZip,
}

impl SourceError {
    pub fn code(self) -> &'static str {
        match self {
            SourceError::TooLarge => "too_large",
            SourceError::NotXml => "not_xml",
            SourceError::NotMapSource => "not_map_source",
            SourceError::BadName => "bad_name",
            SourceError::BadUrl => "bad_url",
            SourceError::BadZoom => "bad_zoom",
            SourceError::NotAnUpload => "not_an_upload",
            SourceError::BadZip => "bad_zip",
        }
    }

    /// Every code, for the check that the UI translates them all.
    pub const ALL: &'static [SourceError] = &[
        SourceError::TooLarge,
        SourceError::NotXml,
        SourceError::NotMapSource,
        SourceError::BadName,
        SourceError::BadUrl,
        SourceError::BadZoom,
        SourceError::NotAnUpload,
        SourceError::BadZip,
    ];
}

/// What the catalogue keeps of a map source, read from its file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MapSource {
    /// The name ATAK lists.
    pub name: String,
    /// The URL template of the tiles.
    pub url: String,
    /// The host of that URL, lowercase: what [`terms_of`] looks at.
    pub host: String,
    pub min_zoom: Option<u8>,
    pub max_zoom: Option<u8>,
    pub tile_type: Option<String>,
    /// What `{$serverpart}` stands for, in turn (`<serverParts>a b c</serverParts>`).
    pub server_parts: Vec<String>,
    /// The rows of tiles count from the bottom (TMS), not from the top.
    pub invert_y: bool,
}

fn child_text<'a>(root: roxmltree::Node<'a, 'a>, tag: &str) -> Option<&'a str> {
    root.children().find(|n| n.is_element() && n.tag_name().name() == tag).and_then(|n| n.text()).map(str::trim)
}

fn zoom(root: roxmltree::Node<'_, '_>, tag: &str) -> Result<Option<u8>, SourceError> {
    match child_text(root, tag) {
        None | Some("") => Ok(None),
        Some(text) => text.parse::<u8>().ok().filter(|z| *z <= 30).map(Some).ok_or(SourceError::BadZoom),
    }
}

/// The host of an `http(s)://` URL, lowercase, without credentials or port.
fn url_host(url: &str) -> Option<String> {
    let rest = url.strip_prefix("https://").or_else(|| url.strip_prefix("http://"))?;
    let authority = rest.split(['/', '?', '#']).next()?;
    let host_port = authority.rsplit('@').next()?;
    let host = match host_port.strip_prefix('[') {
        Some(v6) => v6.split(']').next()?,
        None => host_port.split(':').next()?,
    };
    // A template placeholder in the host (`mt{$serverpart}.example.org`) is kept as it is.
    (!host.is_empty() && !host.contains(char::is_whitespace)).then(|| host.to_ascii_lowercase())
}

/// Reads a map source file and checks what ATAK needs from it. A DTD is refused (so is any
/// entity it could declare): the file goes to phones as it is.
pub fn parse_map_source(bytes: &[u8]) -> Result<MapSource, SourceError> {
    if bytes.len() > MAX_SOURCE_BYTES {
        return Err(SourceError::TooLarge);
    }
    let text = std::str::from_utf8(bytes).map_err(|_| SourceError::NotXml)?;
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    // roxmltree refuses any DTD unless told otherwise.
    let document = roxmltree::Document::parse(text).map_err(|_| SourceError::NotXml)?;
    let root = document.root_element();
    if root.tag_name().name() != "customMapSource" {
        return Err(SourceError::NotMapSource);
    }
    let name = child_text(root, "name").map(|n| n.split_whitespace().collect::<Vec<_>>().join(" ")).unwrap_or_default();
    if name.is_empty() || name.chars().count() > MAX_NAME_LEN || name.chars().any(char::is_control) {
        return Err(SourceError::BadName);
    }
    let url = child_text(root, "url").unwrap_or_default().to_string();
    let placeholders = (url.contains("{$z}") && url.contains("{$x}") && url.contains("{$y}")) || url.contains("{$q}");
    let host = url_host(&url).filter(|_| placeholders).ok_or(SourceError::BadUrl)?;
    let (min_zoom, max_zoom) = (zoom(root, "minZoom")?, zoom(root, "maxZoom")?);
    if let (Some(min), Some(max)) = (min_zoom, max_zoom) {
        if min > max {
            return Err(SourceError::BadZoom);
        }
    }
    let tile_type = child_text(root, "tileType").filter(|t| !t.is_empty()).map(|t| t.chars().take(10).collect());
    let server_parts = child_text(root, "serverParts").unwrap_or_default().split_whitespace().map(str::to_string).collect();
    let invert_y = child_text(root, "invertYCoordinate").is_some_and(|v| v.eq_ignore_ascii_case("true"));
    Ok(MapSource { name, url, host, min_zoom, max_zoom, tile_type, server_parts, invert_y })
}

/// A file name the catalogue can use: the uploaded one when it is plain (`IGN-Plan.xml`),
/// so that a device that already has that file gets it replaced rather than doubled;
/// otherwise one made from the map's name.
pub fn source_file_name(uploaded: Option<&str>, map_name: &str) -> String {
    let plain = |n: &str| {
        let lower = n.to_ascii_lowercase();
        (5..=MAX_FILE_NAME_LEN).contains(&n.len())
            && lower.ends_with(".xml")
            && !n.starts_with(['.', '-'])
            && n.bytes().all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
    };
    match uploaded.map(|n| n.rsplit(['/', '\\']).next().unwrap_or(n)) {
        Some(n) if plain(n) => format!("{}.xml", &n[..n.len() - 4]),
        _ => {
            let stem = slugify(map_name, MAX_FILE_NAME_LEN - 4);
            format!("{}.xml", if stem.is_empty() { "map" } else { &stem })
        }
    }
}

/// What the provider of a map's tiles allows, as far as TeamTAK knows. The code is stable:
/// it is stored and the UI explains it (`maps.terms.<code>`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Terms {
    /// Open data on a public service meant for it (IGN's Géoplateforme).
    Open,
    /// Free to use from an app, without mass downloads (OpenTopoMap, a volunteer server).
    LightUse,
    /// Interactive viewing only: no offline download, attribution on the map
    /// (OpenStreetMap's own tile servers).
    NoOffline,
    /// The provider allows its tiles only through its own APIs and terms (Google, Bing):
    /// never in the suggestions, and flagged in the catalogue.
    Restricted,
    /// A provider TeamTAK knows nothing about: check its terms.
    Unknown,
}

impl Terms {
    pub fn as_str(self) -> &'static str {
        match self {
            Terms::Open => "open",
            Terms::LightUse => "light_use",
            Terms::NoOffline => "no_offline",
            Terms::Restricted => "restricted",
            Terms::Unknown => "unknown",
        }
    }

    pub const ALL: &'static [Terms] = &[Terms::Open, Terms::LightUse, Terms::NoOffline, Terms::Restricted, Terms::Unknown];
}

/// The terms of the provider behind a host.
pub fn terms_of(host: &str) -> Terms {
    let host = host.trim_end_matches('.').to_ascii_lowercase();
    let under = |domain: &str| host == domain || host.ends_with(&format!(".{domain}"));
    // Google and Bing tile servers, under their many names.
    let google = ["google.com", "googleapis.com", "gstatic.com", "google.fr", "googleusercontent.com"];
    let bing = ["virtualearth.net", "bing.com", "bing.net"];
    if google.iter().chain(&bing).any(|d| under(d)) || host.contains(".google.") {
        Terms::Restricted
    } else if under("tile.openstreetmap.org") {
        Terms::NoOffline
    } else if under("opentopomap.org") {
        Terms::LightUse
    } else if under("data.geopf.fr") {
        Terms::Open
    } else {
        Terms::Unknown
    }
}

/// A map source TeamTAK offers to add to an empty catalogue. Only providers whose terms allow
/// a self-hosted platform to hand their URL to a few dozen phones: not OpenStreetMap's own
/// tile servers (no offline use, attribution on the map, which ATAK does not show), never
/// Google or Bing.
#[derive(Debug, Clone, Copy)]
pub struct Suggestion {
    /// Stable identifier, used by the API.
    pub key: &'static str,
    pub file_name: &'static str,
    pub name: &'static str,
    pub url: &'static str,
    pub min_zoom: u8,
    pub max_zoom: u8,
    pub tile_type: &'static str,
}

pub const SUGGESTIONS: &[Suggestion] = &[
    Suggestion {
        key: "ign-plan",
        file_name: "IGN-Plan.xml",
        name: "IGN - Plan IGN",
        url: "https://data.geopf.fr/tms/1.0.0/GEOGRAPHICALGRIDSYSTEMS.PLANIGNV2/{$z}/{$x}/{$y}.png",
        min_zoom: 0,
        max_zoom: 19,
        tile_type: "png",
    },
    Suggestion {
        key: "ign-ortho",
        file_name: "IGN-Photographies-aeriennes.xml",
        name: "IGN - Photographies aeriennes",
        url: "https://data.geopf.fr/tms/1.0.0/ORTHOIMAGERY.ORTHOPHOTOS/{$z}/{$x}/{$y}.jpeg",
        min_zoom: 0,
        max_zoom: 19,
        tile_type: "jpg",
    },
    Suggestion {
        key: "opentopomap",
        file_name: "OpenTopoMap.xml",
        name: "OpenTopoMap",
        url: "https://tile.opentopomap.org/{$z}/{$x}/{$y}.png",
        min_zoom: 0,
        max_zoom: 17,
        tile_type: "png",
    },
];

fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;").replace('\'', "&apos;")
}

impl Suggestion {
    /// The map source file, in the layout ATAK's own exports use.
    pub fn xml(&self) -> String {
        format!(
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<customMapSource>\n    <name>{}</name>\n    <minZoom>{}</minZoom>\n    \
             <maxZoom>{}</maxZoom>\n    <tileType>{}</tileType>\n    <tileUpdate>None</tileUpdate>\n    <url>{}</url>\n    \
             <backgroundColor>#000000</backgroundColor>\n</customMapSource>\n",
            xml_escape(self.name),
            self.min_zoom,
            self.max_zoom,
            xml_escape(self.tile_type),
            xml_escape(self.url)
        )
    }
}

/// One file of an upload, before it is read as a map source.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UploadedFile {
    /// Its name, without any directory.
    pub name: String,
    pub data: Vec<u8>,
}

fn is_zip(bytes: &[u8]) -> bool {
    bytes.starts_with(b"PK\x03\x04")
}

/// The files of an upload: a map source file as it is, or the `.xml` files of a zip (a data
/// package exported by ATAK has its map sources at the root and a `MANIFEST/` directory,
/// which is skipped). Files of a zip that are not `.xml` are skipped too; a file that is too
/// large is kept, so that the caller reports it.
pub fn expand_upload(name: &str, bytes: &[u8]) -> Result<Vec<UploadedFile>, SourceError> {
    if bytes.len() > MAX_UPLOAD_BYTES {
        return Err(SourceError::TooLarge);
    }
    let base = |n: &str| n.rsplit(['/', '\\']).next().unwrap_or(n).to_string();
    if !is_zip(bytes) {
        if !name.to_ascii_lowercase().ends_with(".xml") && !bytes.trim_ascii_start().starts_with(b"<") {
            return Err(SourceError::NotAnUpload);
        }
        return Ok(vec![UploadedFile { name: base(name), data: bytes.to_vec() }]);
    }
    let mut archive = zip::ZipArchive::new(Cursor::new(bytes)).map_err(|_| SourceError::BadZip)?;
    let mut files = Vec::new();
    for i in 0..archive.len() {
        let entry = archive.by_index(i).map_err(|_| SourceError::BadZip)?;
        let path = entry.name().map_err(|_| SourceError::BadZip)?.to_string();
        let skipped = entry.is_dir()
            || path.starts_with("MANIFEST/")
            || path.starts_with("__MACOSX/")
            || !path.to_ascii_lowercase().ends_with(".xml")
            || base(&path).starts_with('.');
        if skipped {
            continue;
        }
        if files.len() == MAX_UPLOAD_FILES {
            return Err(SourceError::BadZip);
        }
        // Read one byte more than allowed: enough to tell it is too large, whatever the
        // archive claims.
        let mut data = Vec::new();
        entry.take(MAX_SOURCE_BYTES as u64 + 1).read_to_end(&mut data).map_err(|_| SourceError::BadZip)?;
        files.push(UploadedFile { name: base(&path), data });
    }
    Ok(files)
}

/// A data package of map sources, for a device that does not take device profiles (iTAK,
/// or ATAK with "Apply TAK Server profile updates" off): imported by hand, it adds the maps.
/// `files` are file names and contents.
pub fn build_maps_package(name: &str, files: &[(String, Vec<u8>)]) -> Result<Vec<u8>, String> {
    let mut manifest = String::from("<MissionPackageManifest version=\"2\">\n  <Configuration>\n");
    manifest.push_str(&format!("    <Parameter name=\"uid\" value=\"{}\"/>\n", Uuid::new_v4()));
    manifest.push_str(&format!("    <Parameter name=\"name\" value=\"{}\"/>\n", xml_escape(name)));
    manifest.push_str("    <Parameter name=\"onReceiveDelete\" value=\"false\"/>\n  </Configuration>\n  <Contents>\n");
    for (file_name, _) in files {
        manifest.push_str(&format!("    <Content ignore=\"false\" zipEntry=\"{}\"/>\n", xml_escape(file_name)));
    }
    manifest.push_str("  </Contents>\n</MissionPackageManifest>\n");
    let mut zip = zip::ZipWriter::new(Cursor::new(Vec::new()));
    let options = zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);
    let err = |e: &dyn std::fmt::Display| e.to_string();
    zip.start_file("MANIFEST/manifest.xml", options).map_err(|e| err(&e))?;
    zip.write_all(manifest.as_bytes()).map_err(|e| err(&e))?;
    for (file_name, data) in files {
        zip.start_file(file_name.as_str(), options).map_err(|e| err(&e))?;
        zip.write_all(data).map_err(|e| err(&e))?;
    }
    Ok(zip.finish().map_err(|e| err(&e))?.into_inner())
}

/// The preference file of the enrollment profile: it turns on ATAK's "Apply TAK Server
/// profile updates" and sets nothing else. Same layout as the preference files TAK Server
/// writes in its own profiles.
pub fn enrollment_pref() -> String {
    format!(
        "<?xml version='1.0' standalone='yes'?>\n<preferences>\n  <preference version=\"1\" name=\"com.atakmap.app.civ_preferences\">\n    \
         <entry key=\"{PROFILES_ON_CONNECT_PREF}\" class=\"class java.lang.Boolean\">true</entry>\n  </preference>\n</preferences>\n"
    )
}

// ------------------------------------------------------------------------------- checking a source

/// Why a map source does not work. The code is stable: it is stored and the UI explains it
/// (`maps.problem.<code>`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum CheckError {
    /// No answer: the name does not resolve, the connection fails or times out.
    #[error("the tile server does not answer")]
    Unreachable,
    /// The tile server answered, but not with a tile.
    #[error("the tile server answered HTTP {0}")]
    HttpStatus(u16),
    /// It answered 200 with something that is not an image (an error page, a vector tile).
    #[error("the tile server answered with something that is not an image")]
    NotAnImage,
    /// The address leads to a private, local or reserved network address: never fetched.
    #[error("the tile address leads to a private or local network address")]
    PrivateAddress,
    /// The URL has a placeholder ATAK does not know, or is not a valid address once filled.
    #[error("the tile address cannot be filled in")]
    UnsupportedUrl,
}

impl CheckError {
    pub fn code(self) -> &'static str {
        match self {
            CheckError::Unreachable => "unreachable",
            CheckError::HttpStatus(_) => "http_status",
            CheckError::NotAnImage => "not_an_image",
            CheckError::PrivateAddress => "private_address",
            CheckError::UnsupportedUrl => "unsupported_url",
        }
    }

    /// The HTTP status, for [`CheckError::HttpStatus`].
    pub fn status(self) -> Option<u16> {
        match self {
            CheckError::HttpStatus(status) => Some(status),
            _ => None,
        }
    }

    /// Every code, for the check that the UI translates them all.
    pub const ALL: &'static [CheckError] = &[
        CheckError::Unreachable,
        CheckError::HttpStatus(404),
        CheckError::NotAnImage,
        CheckError::PrivateAddress,
        CheckError::UnsupportedUrl,
    ];
}

/// Failed checks in a row after which a source is held back.
pub const BROKEN_AFTER_FAILURES: i32 = 2;

/// Whether a source is handed out, after a check: `(failures in a row, broken)`. A source
/// that works is handed out at once; one that worked is held back only once it failed
/// [`BROKEN_AFTER_FAILURES`] times in a row, so that a provider down for a moment does not
/// take a map off the channels.
pub fn after_check(failures: i32, ok: bool) -> (i32, bool) {
    if ok {
        (0, false)
    } else {
        let failures = failures.saturating_add(1);
        (failures, failures >= BROKEN_AFTER_FAILURES)
    }
}

/// The tile containing a point, at a zoom level (the usual "slippy map" numbering).
fn tile_of(lat: f64, lon: f64, z: u8) -> (u32, u32) {
    let n = f64::from(1u32 << z);
    let x = ((lon + 180.0) / 360.0 * n).floor();
    let lat = lat.to_radians();
    let y = ((1.0 - (lat.tan() + 1.0 / lat.cos()).ln() / std::f64::consts::PI) / 2.0 * n).floor();
    let clamp = |v: f64| v.clamp(0.0, n - 1.0) as u32;
    (clamp(x), clamp(y))
}

/// The quadkey of a tile (Bing's `{$q}`).
fn quadkey(z: u8, x: u32, y: u32) -> String {
    (1..=z)
        .rev()
        .map(|i| {
            let mask = 1u32 << (i - 1);
            char::from(b'0' + u8::from(x & mask != 0) + 2 * u8::from(y & mask != 0))
        })
        .collect()
}

/// The address of one tile, with the placeholders ATAK knows filled in; `None` when one is
/// left that it does not know.
pub fn tile_url(source: &MapSource, z: u8, x: u32, y: u32) -> Option<String> {
    let y = if source.invert_y { (1u32 << z) - 1 - y } else { y };
    let server_part = source.server_parts.first().map(String::as_str).unwrap_or("a");
    let url = source
        .url
        .replace("{$z}", &z.to_string())
        .replace("{$x}", &x.to_string())
        .replace("{$y}", &y.to_string())
        .replace("{$q}", &quadkey(z, x, y))
        .replace("{$serverpart}", server_part);
    (!url.contains("{$")).then_some(url)
}

/// Points whose tiles are tried: most maps of this platform cover France; the others the
/// whole world, or America.
const SAMPLE_POINTS: &[(f64, f64)] = &[(46.6, 2.4), (0.0, 0.0), (40.7, -74.0)];

/// The tiles a check tries, in order, until one works: the lowest zoom level the source
/// offers, then a few levels deeper (some providers start lower than they serve), over a few
/// points.
pub fn sample_tiles(source: &MapSource) -> Vec<(u8, u32, u32)> {
    let max = source.max_zoom.unwrap_or(19).min(22);
    let low = source.min_zoom.unwrap_or(0).min(max);
    let deeper = low.max(6).min(max);
    let mut tiles = Vec::new();
    for z in [low, deeper] {
        for (lat, lon) in SAMPLE_POINTS {
            let (x, y) = tile_of(*lat, *lon, z);
            if !tiles.contains(&(z, x, y)) {
                tiles.push((z, x, y));
            }
        }
    }
    tiles
}

/// True for the first bytes of a PNG, JPEG, GIF or WebP image: what ATAK draws as a tile.
pub fn is_image(bytes: &[u8]) -> bool {
    bytes.starts_with(b"\x89PNG\r\n\x1a\n")
        || bytes.starts_with(b"\xff\xd8\xff")
        || bytes.starts_with(b"GIF87a")
        || bytes.starts_with(b"GIF89a")
        || (bytes.len() >= 12 && bytes.starts_with(b"RIFF") && &bytes[8..12] == b"WEBP")
}

/// True for an address on the public Internet. Loopback, private, link-local, shared
/// (carrier-grade NAT), documentation, benchmarking, multicast and reserved ranges are not:
/// a check must never reach the platform's own services (the Docker network is private).
pub fn is_public(ip: std::net::IpAddr) -> bool {
    use std::net::IpAddr;
    match ip {
        IpAddr::V4(v4) => {
            let [a, b, c, _] = v4.octets();
            !(v4.is_loopback()
                || v4.is_private()
                || v4.is_link_local()
                || v4.is_unspecified()
                || v4.is_broadcast()
                || v4.is_multicast()
                || v4.is_documentation()
                || a == 0
                || (a == 100 && (64..128).contains(&b))
                || (a == 192 && b == 0 && c == 0)
                || (a == 198 && (18..20).contains(&b))
                || a >= 240)
        }
        IpAddr::V6(v6) => {
            if let Some(v4) = v6.to_ipv4_mapped() {
                return is_public(IpAddr::V4(v4));
            }
            let first = v6.segments()[0];
            !(v6.is_loopback()
                || v6.is_unspecified()
                || v6.is_multicast()
                || (first & 0xfe00) == 0xfc00
                || (first & 0xffc0) == 0xfe80
                || first == 0x2001 && v6.segments()[1] == 0x0db8
                || first == 0x0064 && v6.segments()[1] == 0xff9b)
        }
    }
}

/// Resolves names to their public addresses only: a name that leads to a private address
/// cannot be reached through it, even after a redirect.
struct PublicOnly;

impl reqwest::dns::Resolve for PublicOnly {
    fn resolve(&self, name: reqwest::dns::Name) -> reqwest::dns::Resolving {
        let host = name.as_str().to_string();
        Box::pin(async move {
            let found: Vec<std::net::SocketAddr> = tokio::net::lookup_host((host.as_str(), 0)).await?.collect();
            let public: Vec<std::net::SocketAddr> = found.iter().copied().filter(|a| is_public(a.ip())).collect();
            if public.is_empty() && !found.is_empty() {
                return Err(Box::new(PrivateAddressError) as Box<dyn std::error::Error + Send + Sync>);
            }
            Ok(Box::new(public.into_iter()) as reqwest::dns::Addrs)
        })
    }
}

#[derive(Debug, thiserror::Error)]
#[error("the name resolves to private addresses only")]
struct PrivateAddressError;

/// True when the host of a URL is an address (not a name) that is not public.
fn private_literal(url: &reqwest::Url) -> bool {
    let host = url.host_str().unwrap_or_default();
    let host = host.strip_prefix('[').and_then(|h| h.strip_suffix(']')).unwrap_or(host);
    host.parse::<std::net::IpAddr>().is_ok_and(|ip| !is_public(ip))
}

/// Largest tile read during a check.
const MAX_TILE_BYTES: usize = 4 * 1024 * 1024;

/// Fetches a tile of a map source, the way a phone would, to tell whether the source works.
pub struct TileChecker {
    http: reqwest::Client,
}

impl TileChecker {
    pub fn new() -> Result<Self, String> {
        let mut roots = rustls::RootCertStore::empty();
        roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
        let tls = rustls::ClientConfig::builder_with_provider(std::sync::Arc::new(rustls::crypto::ring::default_provider()))
            .with_safe_default_protocol_versions()
            .map_err(|e| e.to_string())?
            .with_root_certificates(roots)
            .with_no_client_auth();
        // A few redirects, never to an address on a private network; names are resolved to
        // public addresses only.
        let redirects = reqwest::redirect::Policy::custom(|attempt| {
            if attempt.previous().len() >= 3 || private_literal(attempt.url()) {
                attempt.stop()
            } else {
                attempt.follow()
            }
        });
        let http = reqwest::Client::builder()
            .tls_backend_preconfigured(tls)
            .dns_resolver(std::sync::Arc::new(PublicOnly))
            .redirect(redirects)
            .no_proxy()
            .connect_timeout(std::time::Duration::from_secs(5))
            .timeout(std::time::Duration::from_secs(10))
            .user_agent(concat!("TeamTAK/", env!("CARGO_PKG_VERSION"), " (map source check)"))
            .build()
            .map_err(|e| e.to_string())?;
        Ok(Self { http })
    }

    /// Fetches one tile; `Ok` when it is an image.
    async fn fetch(&self, url: &str) -> Result<(), CheckError> {
        let url = reqwest::Url::parse(url).map_err(|_| CheckError::UnsupportedUrl)?;
        if !matches!(url.scheme(), "http" | "https") {
            return Err(CheckError::UnsupportedUrl);
        }
        if private_literal(&url) {
            return Err(CheckError::PrivateAddress);
        }
        let mut response = self.http.get(url).send().await.map_err(|e| {
            let private = std::iter::successors(std::error::Error::source(&e), |s| s.source())
                .any(|s| s.downcast_ref::<PrivateAddressError>().is_some());
            if private {
                CheckError::PrivateAddress
            } else {
                CheckError::Unreachable
            }
        })?;
        if !response.status().is_success() {
            // A redirect that was not followed (towards a private address) ends here too.
            return Err(if response.status().is_redirection() && private_literal(response.url()) {
                CheckError::PrivateAddress
            } else {
                CheckError::HttpStatus(response.status().as_u16())
            });
        }
        let mut body = Vec::new();
        while let Some(chunk) = response.chunk().await.map_err(|_| CheckError::Unreachable)? {
            body.extend_from_slice(&chunk);
            if body.len() >= 16 || body.len() > MAX_TILE_BYTES {
                break;
            }
        }
        if is_image(&body) {
            Ok(())
        } else {
            Err(CheckError::NotAnImage)
        }
    }

    /// Tries the sample tiles of a source until one is an image. The error is that of the
    /// first tile tried: the most telling one.
    pub async fn check(&self, source: &MapSource) -> Result<(), CheckError> {
        let mut first_error = None;
        for (z, x, y) in sample_tiles(source) {
            let Some(url) = tile_url(source, z, x, y) else { return Err(CheckError::UnsupportedUrl) };
            match self.fetch(&url).await {
                Ok(()) => return Ok(()),
                // Never retried elsewhere: the next tile would lead to the same place.
                Err(e @ (CheckError::PrivateAddress | CheckError::UnsupportedUrl)) => return Err(e),
                Err(e) => {
                    first_error.get_or_insert(e);
                }
            }
        }
        Err(first_error.unwrap_or(CheckError::Unreachable))
    }
}

// ------------------------------------------------------------------------------- device profiles

/// A device profile TAK Server must have.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DesiredProfile {
    pub name: String,
    pub kind: ProfileKind,
    /// The groups whose devices get it.
    pub groups: BTreeSet<String>,
    /// File names and contents.
    pub files: BTreeMap<String, Vec<u8>>,
}

impl DesiredProfile {
    /// The group a connection profile is for; empty for the enrollment profile, which is for
    /// every team.
    pub fn group(&self) -> String {
        group_of_profile(&self.name).unwrap_or_default().to_string()
    }
}

/// The connection profile of a group, with its maps; `None` without any map.
pub fn maps_profile(group: &str, maps: BTreeMap<String, Vec<u8>>) -> Option<DesiredProfile> {
    (!maps.is_empty()).then(|| DesiredProfile {
        name: maps_profile_name(group),
        kind: ProfileKind::Connection,
        groups: BTreeSet::from([group.to_string()]),
        files: maps,
    })
}

/// The enrollment profile, for the groups of every active team; `None` without any.
pub fn enrollment_profile(team_groups: BTreeSet<String>) -> Option<DesiredProfile> {
    (!team_groups.is_empty()).then(|| DesiredProfile {
        name: ENROLLMENT_PROFILE.to_string(),
        kind: ProfileKind::Enrollment,
        groups: team_groups,
        files: BTreeMap::from([(ENROLLMENT_PREF_FILE.to_string(), enrollment_pref().into_bytes())]),
    })
}

/// What a pass does to TAK Server's device profiles.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProfileAction {
    /// Creates the profile, then sets it as wanted and adds its files.
    Create(DesiredProfile),
    /// Sets its kind and groups, and makes it active.
    Set { id: i64, desired: DesiredProfile },
    AddFile { profile: String, file: String, data: Vec<u8> },
    DeleteFile { profile: String, file: String, file_id: i64 },
    Delete { id: i64, name: String },
}

/// Difference between the profiles TeamTAK wants and those TAK Server has. Only profiles
/// named with [`PROFILE_PREFIX`] are looked at; `files` holds the files of each of those, by
/// profile name. A file whose content changed is removed and added again (TAK Server has no
/// call to replace one), removed first so that a device never gets it twice.
pub fn plan_profiles(
    desired: &[DesiredProfile],
    actual: &[DeviceProfile],
    files: &BTreeMap<String, Vec<ProfileFile>>,
) -> Vec<ProfileAction> {
    let ours: BTreeMap<&str, &DeviceProfile> =
        actual.iter().filter(|p| p.name.starts_with(PROFILE_PREFIX)).map(|p| (p.name.as_str(), p)).collect();
    let wanted: BTreeSet<&str> = desired.iter().map(|p| p.name.as_str()).collect();
    let mut actions = Vec::new();
    for want in desired {
        let Some(have) = ours.get(want.name.as_str()) else {
            actions.push(ProfileAction::Create(want.clone()));
            continue;
        };
        if have.kind != Some(want.kind) || have.groups != want.groups || !have.active {
            actions.push(ProfileAction::Set { id: have.id, desired: want.clone() });
        }
        let present = files.get(&want.name).map(Vec::as_slice).unwrap_or_default();
        let mut kept: BTreeSet<&str> = BTreeSet::new();
        for file in present {
            let keep = want.files.get(&file.name).is_some_and(|data| *data == file.data) && kept.insert(file.name.as_str());
            if !keep {
                actions.push(ProfileAction::DeleteFile { profile: want.name.clone(), file: file.name.clone(), file_id: file.id });
            }
        }
        for (name, data) in &want.files {
            if !kept.contains(name.as_str()) {
                actions.push(ProfileAction::AddFile { profile: want.name.clone(), file: name.clone(), data: data.clone() });
            }
        }
    }
    for (name, have) in ours {
        if !wanted.contains(name) {
            actions.push(ProfileAction::Delete { id: have.id, name: name.to_string() });
        }
    }
    actions
}

#[cfg(test)]
mod tests {
    use super::*;

    const IGN_PLAN: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<customMapSource>
    <name>IGN - Plan IGN</name>
    <minZoom>0</minZoom>
    <maxZoom>19</maxZoom>
    <tileType>png</tileType>
    <tileUpdate>None</tileUpdate>
    <url>https://data.geopf.fr/tms/1.0.0/GEOGRAPHICALGRIDSYSTEMS.PLANIGNV2/{$z}/{$x}/{$y}.png</url>
    <backgroundColor>#000000</backgroundColor>
</customMapSource>
"#;

    fn source(name: &str, url: &str) -> String {
        format!("<customMapSource><name>{name}</name><url>{url}</url></customMapSource>")
    }

    #[test]
    fn a_map_source_is_read_as_atak_writes_it() {
        let map = parse_map_source(IGN_PLAN.as_bytes()).unwrap();
        assert_eq!(map.name, "IGN - Plan IGN");
        assert_eq!(map.host, "data.geopf.fr");
        assert_eq!((map.min_zoom, map.max_zoom), (Some(0), Some(19)));
        assert_eq!(map.tile_type.as_deref(), Some("png"));
        // Escaped ampersands, a byte-order mark, a quadkey URL.
        let google = "\u{feff}<customMapSource><name>G</name><url>https://mt.google.com/vt/lyrs=m&amp;x={$x}&amp;y={$y}&amp;z={$z}</url></customMapSource>";
        assert_eq!(parse_map_source(google.as_bytes()).unwrap().url, "https://mt.google.com/vt/lyrs=m&x={$x}&y={$y}&z={$z}");
        let bing = source("Bing", "http://ecn.t0.tiles.virtualearth.net/tiles/a{$q}.jpeg?g=1");
        assert_eq!(parse_map_source(bing.as_bytes()).unwrap().host, "ecn.t0.tiles.virtualearth.net");
    }

    #[test]
    fn what_is_not_a_map_source_is_refused() {
        let refused = |xml: &str| parse_map_source(xml.as_bytes()).unwrap_err();
        assert_eq!(refused("not xml"), SourceError::NotXml);
        assert_eq!(refused("<preferences/>"), SourceError::NotMapSource);
        let dtd = "<!DOCTYPE m [<!ENTITY a \"aaaa\">]><customMapSource><name>&a;</name></customMapSource>";
        assert_eq!(refused(dtd), SourceError::NotXml, "no DTD, so no entity, goes to a phone");
        assert_eq!(refused(&source("", "https://h/{$z}/{$x}/{$y}")), SourceError::BadName);
        assert_eq!(refused(&source(&"x".repeat(61), "https://h/{$z}/{$x}/{$y}")), SourceError::BadName);
        assert_eq!(refused(&source("A", "https://h/{$z}/{$x}")), SourceError::BadUrl);
        assert_eq!(refused(&source("A", "file:///sdcard/{$z}/{$x}/{$y}")), SourceError::BadUrl);
        assert_eq!(refused(&source("A", "")), SourceError::BadUrl);
        let zooms = "<customMapSource><name>A</name><minZoom>12</minZoom><maxZoom>3</maxZoom><url>https://h/{$z}/{$x}/{$y}</url></customMapSource>";
        assert_eq!(refused(zooms), SourceError::BadZoom);
        assert_eq!(parse_map_source(&vec![b' '; MAX_SOURCE_BYTES + 1]).unwrap_err(), SourceError::TooLarge);
    }

    #[test]
    fn server_parts_and_inverted_rows_are_read() {
        let xml = "<customMapSource><name>A</name><serverParts>a b c</serverParts><invertYCoordinate>true</invertYCoordinate>\
                   <url>https://{$serverpart}.tiles.example.org/{$z}/{$x}/{$y}.png</url></customMapSource>";
        let map = parse_map_source(xml.as_bytes()).unwrap();
        assert_eq!(map.server_parts, ["a", "b", "c"]);
        assert!(map.invert_y);
        assert_eq!(tile_url(&map, 2, 1, 0).as_deref(), Some("https://a.tiles.example.org/2/1/3.png"), "row counted from the bottom");
    }

    #[test]
    fn tile_addresses_are_filled_in_as_atak_does() {
        let map = parse_map_source(IGN_PLAN.as_bytes()).unwrap();
        assert_eq!(
            tile_url(&map, 5, 16, 11).as_deref(),
            Some("https://data.geopf.fr/tms/1.0.0/GEOGRAPHICALGRIDSYSTEMS.PLANIGNV2/5/16/11.png")
        );
        let bing = parse_map_source(source("Bing", "https://ecn.t0.tiles.virtualearth.net/tiles/a{$q}.jpeg").as_bytes()).unwrap();
        // The quadkey of tile x=3, y=5 at zoom 3 is "213" (Bing's documentation).
        assert_eq!(tile_url(&bing, 3, 3, 5).as_deref(), Some("https://ecn.t0.tiles.virtualearth.net/tiles/a213.jpeg"));
        let odd = parse_map_source(source("A", "https://h/{$z}/{$x}/{$y}/{$token}").as_bytes()).unwrap();
        assert_eq!(tile_url(&odd, 0, 0, 0), None, "a placeholder ATAK does not know");
    }

    #[test]
    fn sample_tiles_start_at_the_lowest_level_over_france() {
        let map = parse_map_source(IGN_PLAN.as_bytes()).unwrap();
        let tiles = sample_tiles(&map);
        assert_eq!(tiles[0], (0, 0, 0), "one tile covers the world at zoom 0");
        assert!(tiles.contains(&(6, 32, 22)), "France at zoom 6: {tiles:?}");
        let deep = parse_map_source(
            b"<customMapSource><name>A</name><minZoom>12</minZoom><maxZoom>16</maxZoom><url>https://h/{$z}/{$x}/{$y}</url></customMapSource>",
        )
        .unwrap();
        assert!(sample_tiles(&deep).iter().all(|(z, _, _)| *z == 12), "never below the source's lowest level");
        let shallow = parse_map_source(
            b"<customMapSource><name>A</name><minZoom>0</minZoom><maxZoom>3</maxZoom><url>https://h/{$z}/{$x}/{$y}</url></customMapSource>",
        )
        .unwrap();
        assert!(sample_tiles(&shallow).iter().all(|(z, _, _)| *z <= 3), "never above its highest");
    }

    #[test]
    fn only_images_count_as_tiles() {
        assert!(is_image(b"\x89PNG\r\n\x1a\n...."));
        assert!(is_image(b"\xff\xd8\xff\xe0...."));
        assert!(is_image(b"GIF89a...."));
        assert!(is_image(b"RIFF\x00\x00\x00\x00WEBPVP8 "));
        assert!(!is_image(b"<html>Not found</html>"));
        assert!(!is_image(b"\x1a\x05layer"), "a vector tile");
        assert!(!is_image(b""));
    }

    #[test]
    fn private_and_reserved_addresses_are_never_fetched() {
        for ip in ["127.0.0.1", "10.1.2.3", "172.18.0.5", "192.168.1.1", "169.254.169.254", "100.64.0.1", "0.0.0.0", "::1", "fd00::1", "fe80::1", "::ffff:10.0.0.1", "224.0.0.1"] {
            assert!(!is_public(ip.parse().unwrap()), "{ip}");
        }
        for ip in ["193.252.0.1", "8.8.8.8", "2a01:e0a::1"] {
            assert!(is_public(ip.parse().unwrap()), "{ip}");
        }
    }

    #[tokio::test]
    async fn a_check_never_reaches_a_private_address() {
        let checker = TileChecker::new().unwrap();
        let local = |url: &str| parse_map_source(source("A", url).as_bytes()).unwrap();
        assert_eq!(checker.check(&local("http://127.0.0.1:9/{$z}/{$x}/{$y}.png")).await, Err(CheckError::PrivateAddress));
        assert_eq!(checker.check(&local("http://[::1]/{$z}/{$x}/{$y}.png")).await, Err(CheckError::PrivateAddress));
        assert_eq!(checker.check(&local("http://localhost:9/{$z}/{$x}/{$y}.png")).await, Err(CheckError::PrivateAddress), "by name too");
    }

    #[test]
    fn a_source_is_held_back_after_two_failures_in_a_row() {
        assert_eq!(after_check(0, true), (0, false));
        assert_eq!(after_check(0, false), (1, false), "one failure: maybe the provider is down for a moment");
        assert_eq!(after_check(1, false), (2, true));
        assert_eq!(after_check(5, true), (0, false), "it works again");
    }

    #[test]
    fn file_names_keep_a_plain_uploaded_name() {
        assert_eq!(source_file_name(Some("IGN-Plan.xml"), "IGN - Plan IGN"), "IGN-Plan.xml");
        assert_eq!(source_file_name(Some("maps/OpenTopoMap.XML"), "x"), "OpenTopoMap.xml");
        assert_eq!(source_file_name(Some("Mes cartes (1).xml"), "Carte d'état-major"), "carte-d-etat-major.xml");
        assert_eq!(source_file_name(None, "IGN - Photographies aériennes"), "ign-photographies-aeriennes.xml");
        assert_eq!(source_file_name(Some(".hidden.xml"), "!!!"), "map.xml");
    }

    #[test]
    fn providers_are_told_apart() {
        for host in ["mt.google.com", "mt1.google.com", "khms0.googleapis.com", "ecn.t0.tiles.virtualearth.net", "www.google.de"] {
            assert_eq!(terms_of(host), Terms::Restricted, "{host}");
        }
        assert_eq!(terms_of("tile.openstreetmap.org"), Terms::NoOffline);
        assert_eq!(terms_of("a.tile.openstreetmap.org"), Terms::NoOffline);
        assert_eq!(terms_of("tile.opentopomap.org"), Terms::LightUse);
        assert_eq!(terms_of("data.geopf.fr"), Terms::Open);
        assert_eq!(terms_of("tiles.example.org"), Terms::Unknown);
        assert_eq!(terms_of("notgoogle.com"), Terms::Unknown);
    }

    #[test]
    fn suggestions_are_valid_and_never_restricted() {
        for s in SUGGESTIONS {
            let map = parse_map_source(s.xml().as_bytes()).unwrap();
            assert_eq!(map.name, s.name);
            assert_eq!(source_file_name(Some(s.file_name), s.name), s.file_name);
            assert!(matches!(terms_of(&map.host), Terms::Open | Terms::LightUse), "{}", s.key);
        }
    }

    fn zip_of(entries: &[(&str, &[u8])]) -> Vec<u8> {
        let mut zip = zip::ZipWriter::new(Cursor::new(Vec::new()));
        for (name, data) in entries {
            zip.start_file(*name, zip::write::SimpleFileOptions::default()).unwrap();
            zip.write_all(data).unwrap();
        }
        zip.finish().unwrap().into_inner()
    }

    #[test]
    fn an_upload_is_a_file_or_a_data_package() {
        let single = expand_upload("IGN-Plan.xml", IGN_PLAN.as_bytes()).unwrap();
        assert_eq!(single, vec![UploadedFile { name: "IGN-Plan.xml".into(), data: IGN_PLAN.as_bytes().to_vec() }]);
        // The layout of ATAK's exports: map sources at the root, a manifest aside.
        let package = zip_of(&[
            ("MANIFEST/manifest.xml", b"<MissionPackageManifest/>"),
            ("IGN-Plan.xml", IGN_PLAN.as_bytes()),
            ("sub/OpenTopoMap.xml", b"<customMapSource/>"),
            ("readme.txt", b"hello"),
            ("__MACOSX/._IGN-Plan.xml", b"junk"),
        ]);
        let files = expand_upload("maps.zip", &package).unwrap();
        let names: Vec<&str> = files.iter().map(|f| f.name.as_str()).collect();
        assert_eq!(names, ["IGN-Plan.xml", "OpenTopoMap.xml"]);
        assert_eq!(expand_upload("photo.jpg", b"\xff\xd8\xff").unwrap_err(), SourceError::NotAnUpload);
        assert_eq!(expand_upload("x.zip", b"PK\x03\x04broken").unwrap_err(), SourceError::BadZip);
        let big = vec![b'<'; MAX_SOURCE_BYTES + 100];
        let files = expand_upload("big.zip", &zip_of(&[("big.xml", &big)])).unwrap();
        assert_eq!(files[0].data.len(), MAX_SOURCE_BYTES + 1, "read no further than needed to refuse it");
    }

    #[test]
    fn the_maps_package_lists_its_files() {
        let files = vec![("IGN-Plan.xml".to_string(), IGN_PLAN.as_bytes().to_vec())];
        let bytes = build_maps_package("Alpha <maps>", &files).unwrap();
        let mut archive = zip::ZipArchive::new(Cursor::new(bytes)).unwrap();
        let mut manifest = String::new();
        archive.by_name("MANIFEST/manifest.xml").unwrap().read_to_string(&mut manifest).unwrap();
        assert!(manifest.contains(r#"zipEntry="IGN-Plan.xml""#));
        assert!(manifest.contains("Alpha &lt;maps&gt;"));
        let mut xml = String::new();
        archive.by_name("IGN-Plan.xml").unwrap().read_to_string(&mut xml).unwrap();
        assert_eq!(xml, IGN_PLAN);
    }

    #[test]
    fn the_enrollment_profile_sets_one_preference_and_nothing_about_the_user() {
        let pref = enrollment_pref();
        assert!(pref.contains(r#"<entry key="deviceProfileEnableOnConnect" class="class java.lang.Boolean">true</entry>"#));
        assert_eq!(pref.matches("<entry").count(), 1);
        for forbidden in ["callsign", "locationTeam", "atakRoleType", "group"] {
            assert!(!pref.contains(forbidden));
        }
        assert!(enrollment_profile(BTreeSet::new()).is_none());
        let profile = enrollment_profile(["alpha".to_string(), "bravo".to_string()].into()).unwrap();
        assert_eq!((profile.kind, profile.group()), (ProfileKind::Enrollment, String::new()));
    }

    fn files(entries: &[(&str, &str)]) -> BTreeMap<String, Vec<u8>> {
        entries.iter().map(|(n, d)| (n.to_string(), d.as_bytes().to_vec())).collect()
    }

    fn have(id: i64, name: &str, kind: ProfileKind, groups: &[&str]) -> DeviceProfile {
        DeviceProfile { id, name: name.into(), kind: Some(kind), active: true, groups: groups.iter().map(|g| g.to_string()).collect() }
    }

    fn file(id: i64, name: &str, data: &str) -> ProfileFile {
        ProfileFile { id, name: name.into(), data: data.as_bytes().to_vec() }
    }

    #[test]
    fn profile_names_carry_their_group() {
        assert_eq!(maps_profile_name("evt-exercise"), "teamtak-maps-evt-exercise");
        assert_eq!(group_of_profile("teamtak-maps-alpha"), Some("alpha"));
        assert_eq!(group_of_profile(ENROLLMENT_PROFILE), None);
        assert_eq!(group_of_profile("maps-alpha"), None);
        assert!(maps_profile("alpha", BTreeMap::new()).is_none(), "no map, no profile");
    }

    #[test]
    fn profiles_are_created_completed_and_removed() {
        let alpha = maps_profile("alpha", files(&[("a.xml", "A"), ("b.xml", "B")])).unwrap();
        let bravo = maps_profile("bravo", files(&[("a.xml", "A")])).unwrap();
        let actual = vec![
            have(1, "teamtak-maps-alpha", ProfileKind::Connection, &["alpha"]),
            have(2, "teamtak-maps-charlie", ProfileKind::Connection, &["charlie"]),
            have(3, "someone-elses", ProfileKind::Enrollment, &["alpha"]),
        ];
        let present = BTreeMap::from([
            ("teamtak-maps-alpha".to_string(), vec![file(10, "a.xml", "A"), file(11, "b.xml", "old"), file(12, "c.xml", "C")]),
        ]);
        let actions = plan_profiles(&[alpha.clone(), bravo.clone()], &actual, &present);
        assert_eq!(
            actions,
            vec![
                ProfileAction::DeleteFile { profile: alpha.name.clone(), file: "b.xml".into(), file_id: 11 },
                ProfileAction::DeleteFile { profile: alpha.name.clone(), file: "c.xml".into(), file_id: 12 },
                ProfileAction::AddFile { profile: alpha.name.clone(), file: "b.xml".into(), data: b"B".to_vec() },
                ProfileAction::Create(bravo),
                ProfileAction::Delete { id: 2, name: "teamtak-maps-charlie".into() },
            ],
            "a.xml is unchanged; a profile not named by TeamTAK is left alone"
        );
    }

    #[test]
    fn a_profile_changed_in_tak_server_is_set_back() {
        let alpha = maps_profile("alpha", files(&[("a.xml", "A")])).unwrap();
        let present = BTreeMap::from([("teamtak-maps-alpha".to_string(), vec![file(10, "a.xml", "A"), file(13, "a.xml", "A")])]);
        let mut changed = have(1, "teamtak-maps-alpha", ProfileKind::Enrollment, &["alpha", "bravo"]);
        let actions = plan_profiles(std::slice::from_ref(&alpha), std::slice::from_ref(&changed), &present);
        assert_eq!(
            actions,
            vec![
                ProfileAction::Set { id: 1, desired: alpha.clone() },
                ProfileAction::DeleteFile { profile: alpha.name.clone(), file: "a.xml".into(), file_id: 13 },
            ],
            "kind and groups set back, the second copy of a file removed"
        );
        changed = have(1, "teamtak-maps-alpha", ProfileKind::Connection, &["alpha"]);
        changed.active = false;
        let actions = plan_profiles(std::slice::from_ref(&alpha), &[changed], &present);
        assert!(matches!(actions[0], ProfileAction::Set { id: 1, .. }), "made active again");
        let settled = BTreeMap::from([("teamtak-maps-alpha".to_string(), vec![file(10, "a.xml", "A")])]);
        let fine = have(1, "teamtak-maps-alpha", ProfileKind::Connection, &["alpha"]);
        assert!(plan_profiles(&[alpha], &[fine], &settled).is_empty(), "nothing to do");
    }
}
