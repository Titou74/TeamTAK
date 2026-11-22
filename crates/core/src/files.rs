//! Team files: what a team sees, downloads, sends and removes among TAK Server's files.
//!
//! TAK Server keeps two kinds of files a team cares about:
//!
//! - **data packages** in Enterprise Sync, sent to the server by a device (or by TeamTAK), each
//!   stored for the groups of its sender, or for the groups named at upload;
//! - the **files of Data Sync missions**, which a mission links by their hash; a mission has
//!   groups too, and may be protected by a password or open on invitation only.
//!
//! The platform reaches TAK Server with an administrator's certificate, which sees every file and
//! every mission and is never asked for a mission's password (checked on 5.8). So the rules are
//! applied here:
//!
//! - a team sees what TAK Server would hand its devices now: the data packages and the missions
//!   whose groups meet the team's groups (its own, and those of the events it takes part in
//!   while they run);
//! - never anything of a password-protected or invite-only mission: such a mission is listed
//!   locked, without its files, and those files are left out of the data packages too; so are
//!   the archives TAK Server makes of deleted missions;
//! - TAK Server lets anyone who shares a group delete a file; here a member removes what they
//!   sent through TeamTAK, and a leader what the team sent (through TeamTAK or with its TAK
//!   accesses), never another team's file;
//! - an upload always names its group: without one, a file sent with the administrator's
//!   certificate would be stored for every group.

use std::collections::{BTreeMap, BTreeSet};
use std::io::{Cursor, Read, Write};

use time::OffsetDateTime;
use uuid::Uuid;

/// Keyword of the data packages ATAK lists among the server's (and sends with).
pub const DATA_PACKAGE_KEYWORD: &str = "missionpackage";
/// Keyword of the archive TAK Server keeps of a deleted mission: never shown.
pub const ARCHIVED_MISSION_KEYWORD: &str = "ARCHIVED_MISSION";
/// Keyword of a file TeamTAK sends to a mission (not a data package of its own).
pub const MISSION_FILE_KEYWORD: &str = "mission";
/// Longest file name kept.
pub const MAX_FILE_NAME_LEN: usize = 120;

/// A file of Enterprise Sync, as the platform's certificate sees it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnterpriseFile {
    /// Identifies this upload among those of the same content: removals use it, never the hash.
    pub primary_key: i64,
    pub hash: String,
    pub name: String,
    pub size: i64,
    /// The TAK account that sent it (`teamtak-api` for the platform).
    pub submitter: String,
    pub creator_uid: String,
    pub submitted_at: Option<OffsetDateTime>,
    pub keywords: Vec<String>,
    pub mime_type: String,
    /// Empty when the listing that carries the groups did not have this upload: such a file
    /// is shown to nobody.
    pub groups: BTreeSet<String>,
}

impl EnterpriseFile {
    pub fn has_keyword(&self, keyword: &str) -> bool {
        self.keywords.iter().any(|k| k.eq_ignore_ascii_case(keyword))
    }
}

/// One row of `GET /Marti/sync/search`: everything but the groups.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchRow {
    pub primary_key: i64,
    pub hash: String,
    pub name: String,
    pub size: i64,
    pub submitter: String,
    pub creator_uid: String,
    pub submitted_at: Option<OffsetDateTime>,
    pub keywords: Vec<String>,
    pub mime_type: String,
}

/// One row of `GET /Marti/api/files/metadata`: the groups, without the primary key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MetadataRow {
    pub hash: String,
    pub name: String,
    /// The submission time as written there (`2026-11-22 10:00:51.568`, UTC).
    pub time: String,
    pub groups: BTreeSet<String>,
}

/// The submission time of a search row in the form `/Marti/api/files/metadata` writes it.
fn metadata_time(at: OffsetDateTime) -> String {
    let at = at.to_offset(time::UtcOffset::UTC);
    format!(
        "{:04}-{:02}-{:02} {:02}:{:02}:{:02}.{:03}",
        at.year(),
        u8::from(at.month()),
        at.day(),
        at.hour(),
        at.minute(),
        at.second(),
        at.millisecond()
    )
}

/// A submission time as `/Marti/api/files/metadata` writes it, to the millisecond: it drops
/// the trailing zeros of the fraction (`10:00:51.45` for 450 ms, `10:00:51.0` for none), as
/// Java's timestamps do.
fn normalized_time(raw: &str) -> String {
    let raw = raw.trim();
    let (seconds, fraction) = raw.split_once('.').unwrap_or((raw, ""));
    let digits: String = fraction.chars().filter(char::is_ascii_digit).chain(std::iter::repeat('0')).take(3).collect();
    format!("{seconds}.{digits}")
}

/// Puts the two listings of Enterprise Sync together: the search has the primary key and the
/// size, the metadata listing the groups. Several uploads can have the same content (same
/// hash): they are told apart by name and submission time.
pub fn join_listings(search: Vec<SearchRow>, metadata: &[MetadataRow]) -> Vec<EnterpriseFile> {
    let times: Vec<String> = metadata.iter().map(|m| normalized_time(&m.time)).collect();
    let groups: BTreeMap<(&str, &str, &str), &BTreeSet<String>> =
        metadata.iter().zip(&times).map(|(m, time)| ((m.hash.as_str(), m.name.as_str(), time.as_str()), &m.groups)).collect();
    search
        .into_iter()
        .map(|row| {
            let time = row.submitted_at.map(metadata_time).unwrap_or_default();
            let groups = groups.get(&(row.hash.as_str(), row.name.as_str(), time.as_str())).map(|g| (*g).clone()).unwrap_or_default();
            EnterpriseFile {
                primary_key: row.primary_key,
                hash: row.hash,
                name: row.name,
                size: row.size,
                submitter: row.submitter,
                creator_uid: row.creator_uid,
                submitted_at: row.submitted_at,
                keywords: row.keywords,
                mime_type: row.mime_type,
                groups,
            }
        })
        .collect()
}

/// A Data Sync mission.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Mission {
    pub name: String,
    pub description: String,
    pub groups: BTreeSet<String>,
    pub password_protected: bool,
    pub invite_only: bool,
    /// Its default role lets a subscriber add files (`MISSION_WRITE`).
    pub writable: bool,
    pub creator_uid: String,
    pub created_at: Option<OffsetDateTime>,
}

impl Mission {
    /// Nothing of it may be shown or touched: protected by a password, or on invitation.
    pub fn locked(&self) -> bool {
        self.password_protected || self.invite_only
    }
}

/// A file of a mission.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MissionFile {
    pub hash: String,
    pub name: String,
    pub size: i64,
    pub submitter: String,
    pub creator_uid: String,
    pub added_at: Option<OffsetDateTime>,
    pub mime_type: String,
}

/// True when two sets of groups share one.
pub fn shares_group(groups: &BTreeSet<String>, team_groups: &BTreeSet<String>) -> bool {
    groups.iter().any(|g| team_groups.contains(g))
}

/// The data packages a team sees: data packages of Enterprise Sync that share a group with the
/// team, except those of a locked mission (`hidden`, by hash) and the archives of missions.
pub fn visible_data_packages<'a>(
    files: &'a [EnterpriseFile],
    team_groups: &BTreeSet<String>,
    hidden: &BTreeSet<String>,
) -> Vec<&'a EnterpriseFile> {
    files
        .iter()
        .filter(|f| f.has_keyword(DATA_PACKAGE_KEYWORD) && !f.has_keyword(ARCHIVED_MISSION_KEYWORD))
        .filter(|f| shares_group(&f.groups, team_groups))
        .filter(|f| !hidden.contains(&f.hash))
        .collect()
}

/// The missions a team sees: those that share a group with it. An invite-only mission is not
/// listed by TAK Server to anyone who was not invited, and not here either.
pub fn visible_missions<'a>(missions: &'a [Mission], team_groups: &BTreeSet<String>) -> Vec<&'a Mission> {
    missions.iter().filter(|m| !m.invite_only && shares_group(&m.groups, team_groups)).collect()
}

/// How the caller relates to a file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FileRef {
    pub caller_is_leader: bool,
    /// The caller sent it through TeamTAK.
    pub sent_by_caller: bool,
    /// Someone of the team sent it, through TeamTAK or with one of the team's TAK accesses.
    pub sent_by_team: bool,
}

/// Whether the caller may remove a file (from Enterprise Sync, or from a mission).
pub fn may_remove(file: FileRef) -> bool {
    file.sent_by_caller || (file.caller_is_leader && file.sent_by_team)
}

/// A file name kept as sent, without directories or control characters, and not too long;
/// `None` when nothing is left.
pub fn clean_file_name(raw: &str) -> Option<String> {
    let base = raw.rsplit(['/', '\\']).next().unwrap_or(raw);
    let name: String = base.chars().filter(|c| !c.is_control() && *c != '"').collect::<String>().trim().to_string();
    let name: String = if name.chars().count() > MAX_FILE_NAME_LEN {
        // Keep the extension.
        let (stem, ext) = match name.rsplit_once('.') {
            Some((stem, ext)) if ext.len() <= 10 => (stem.to_string(), format!(".{ext}")),
            _ => (name.clone(), String::new()),
        };
        stem.chars().take(MAX_FILE_NAME_LEN - ext.chars().count()).collect::<String>() + &ext
    } else {
        name
    };
    (!name.is_empty() && name != "." && name != "..").then_some(name)
}

/// True for a data package: a zip with `MANIFEST/manifest.xml`.
pub fn is_data_package(bytes: &[u8]) -> bool {
    if !bytes.starts_with(b"PK\x03\x04") {
        return false;
    }
    zip::ZipArchive::new(Cursor::new(bytes)).is_ok_and(|mut archive| archive.by_name("MANIFEST/manifest.xml").is_ok())
}

fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;").replace('\'', "&apos;")
}

/// A data package holding one file, so that ATAK imports it: a PDF, a KML, a picture. Its
/// name is the file's, with `.zip`.
pub fn wrap_in_data_package(file_name: &str, data: &[u8]) -> Result<(String, Vec<u8>), String> {
    let stem = file_name.rsplit_once('.').map_or(file_name, |(stem, _)| stem);
    let package_name = format!("{stem}.zip");
    let mut manifest = String::from("<MissionPackageManifest version=\"2\">\n  <Configuration>\n");
    manifest.push_str(&format!("    <Parameter name=\"uid\" value=\"{}\"/>\n", Uuid::new_v4()));
    manifest.push_str(&format!("    <Parameter name=\"name\" value=\"{}\"/>\n", xml_escape(&package_name)));
    manifest.push_str("    <Parameter name=\"onReceiveDelete\" value=\"false\"/>\n  </Configuration>\n  <Contents>\n");
    manifest.push_str(&format!("    <Content ignore=\"false\" zipEntry=\"{}\"/>\n", xml_escape(file_name)));
    manifest.push_str("  </Contents>\n</MissionPackageManifest>\n");
    let err = |e: &dyn std::fmt::Display| e.to_string();
    let mut zip = zip::ZipWriter::new(Cursor::new(Vec::new()));
    let options = zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);
    zip.start_file("MANIFEST/manifest.xml", options).map_err(|e| err(&e))?;
    zip.write_all(manifest.as_bytes()).map_err(|e| err(&e))?;
    zip.start_file(file_name, options.large_file(data.len() as u64 >= u32::MAX as u64)).map_err(|e| err(&e))?;
    zip.write_all(data).map_err(|e| err(&e))?;
    Ok((package_name, zip.finish().map_err(|e| err(&e))?.into_inner()))
}

/// The files of a data package, by name (tests and checks).
pub fn data_package_files(bytes: &[u8]) -> Result<BTreeMap<String, Vec<u8>>, String> {
    let mut archive = zip::ZipArchive::new(Cursor::new(bytes)).map_err(|e| e.to_string())?;
    let mut files = BTreeMap::new();
    for i in 0..archive.len() {
        let mut entry = archive.by_index(i).map_err(|e| e.to_string())?;
        if entry.is_dir() {
            continue;
        }
        let name = entry.name().map_err(|e| e.to_string())?.to_string();
        let mut data = Vec::new();
        entry.read_to_end(&mut data).map_err(|e| e.to_string())?;
        files.insert(name, data);
    }
    Ok(files)
}

#[cfg(test)]
mod tests {
    use super::*;
    use time::macros::datetime;

    fn groups(names: &[&str]) -> BTreeSet<String> {
        names.iter().map(|g| g.to_string()).collect()
    }

    fn search(pk: i64, name: &str, at: OffsetDateTime) -> SearchRow {
        SearchRow {
            primary_key: pk,
            hash: "9aaa".into(),
            name: name.into(),
            size: 480,
            submitter: "teamtak-api".into(),
            creator_uid: "teamtak-alpha".into(),
            submitted_at: Some(at),
            keywords: vec!["missionpackage".into()],
            mime_type: "application/zip".into(),
        }
    }

    #[test]
    fn the_two_listings_are_joined_upload_by_upload() {
        // Shapes observed on TAK Server 5.8: the same content sent twice, to two groups.
        let a = datetime!(2026-11-22 10:00:51.568 UTC);
        let b = datetime!(2026-11-22 10:00:51.636 UTC);
        let metadata = vec![
            MetadataRow { hash: "9aaa".into(), name: "probe-a.zip".into(), time: "2026-11-22 10:00:51.568".into(), groups: groups(&["alpha"]) },
            MetadataRow { hash: "9aaa".into(), name: "probe-b.zip".into(), time: "2026-11-22 10:00:51.636".into(), groups: groups(&["bravo"]) },
        ];
        let files = join_listings(vec![search(6, "probe-a.zip", a), search(7, "probe-b.zip", b), search(8, "lost.zip", b)], &metadata);
        assert_eq!(files[0].groups, groups(&["alpha"]));
        assert_eq!(files[1].groups, groups(&["bravo"]));
        assert!(files[2].groups.is_empty(), "an upload the other listing does not have");
        // Java drops the trailing zeros of the fraction.
        let c = datetime!(2026-11-22 10:11:39.450 UTC);
        let d = datetime!(2026-11-22 10:11:40 UTC);
        let trimmed = vec![
            MetadataRow { hash: "9aaa".into(), name: "c.zip".into(), time: "2026-11-22 10:11:39.45".into(), groups: groups(&["alpha"]) },
            MetadataRow { hash: "9aaa".into(), name: "d.zip".into(), time: "2026-11-22 10:11:40.0".into(), groups: groups(&["alpha"]) },
        ];
        let later = join_listings(vec![search(9, "c.zip", c), search(10, "d.zip", d)], &trimmed);
        assert!(later.iter().all(|f| f.groups.len() == 1), "{later:?}");
        let shown = visible_data_packages(&files, &groups(&["alpha", "evt-x"]), &BTreeSet::new());
        assert_eq!(shown.iter().map(|f| f.primary_key).collect::<Vec<_>>(), [6], "bravo's copy of the same content stays bravo's");
    }

    #[test]
    fn only_data_packages_of_the_team_and_never_a_locked_missions() {
        let at = datetime!(2026-11-22 10:00 UTC);
        let file = |pk: i64, hash: &str, keywords: &[&str], g: &[&str]| EnterpriseFile {
            primary_key: pk,
            hash: hash.into(),
            name: format!("f{pk}"),
            size: 1,
            submitter: "u".into(),
            creator_uid: "c".into(),
            submitted_at: Some(at),
            keywords: keywords.iter().map(|k| k.to_string()).collect(),
            mime_type: "application/zip".into(),
            groups: groups(g),
        };
        let files = vec![
            file(1, "h1", &["missionpackage"], &["alpha"]),
            file(2, "h2", &["missionpackage"], &["bravo"]),
            file(3, "h3", &["ARCHIVED_MISSION"], &["alpha"]),
            file(4, "h4", &["mission"], &["alpha"]),
            file(5, "h5", &["missionpackage"], &["alpha", "bravo"]),
            file(6, "secret", &["missionpackage"], &["alpha"]),
            file(7, "h7", &["MissionPackage"], &["evt-x"]),
        ];
        let shown = visible_data_packages(&files, &groups(&["alpha", "evt-x"]), &groups(&["secret"]));
        assert_eq!(shown.iter().map(|f| f.primary_key).collect::<Vec<_>>(), [1, 5, 7]);
    }

    #[test]
    fn missions_of_the_team_only_and_locked_ones_without_their_files() {
        let mission = |name: &str, g: &[&str], password: bool, invite: bool| Mission {
            name: name.into(),
            description: String::new(),
            groups: groups(g),
            password_protected: password,
            invite_only: invite,
            writable: true,
            creator_uid: String::new(),
            created_at: None,
        };
        let missions = vec![
            mission("open", &["alpha"], false, false),
            mission("locked", &["alpha"], true, false),
            mission("invited", &["alpha"], false, true),
            mission("other", &["bravo"], false, false),
        ];
        let shown: Vec<&str> = visible_missions(&missions, &groups(&["alpha"])).iter().map(|m| m.name.as_str()).collect();
        assert_eq!(shown, ["open", "locked"]);
        assert!(missions[1].locked() && missions[2].locked() && !missions[0].locked());
    }

    #[test]
    fn members_remove_their_own_files_and_leaders_their_teams() {
        let r = |leader, mine, team| may_remove(FileRef { caller_is_leader: leader, sent_by_caller: mine, sent_by_team: team });
        assert!(r(false, true, true), "a member, what they sent");
        assert!(!r(false, false, true), "a member, what someone else of the team sent");
        assert!(r(true, false, true), "a leader, what the team sent");
        assert!(!r(true, false, false), "never another team's file");
    }

    #[test]
    fn file_names_are_kept_without_directories() {
        assert_eq!(clean_file_name("C:\\Users\\me\\Plan \"secours\".pdf").as_deref(), Some("Plan secours.pdf"));
        assert_eq!(clean_file_name("../../etc/passwd").as_deref(), Some("passwd"));
        assert_eq!(clean_file_name(" / "), None);
        let long = format!("{}.kml", "x".repeat(300));
        let kept = clean_file_name(&long).unwrap();
        assert_eq!(kept.chars().count(), MAX_FILE_NAME_LEN);
        assert!(kept.ends_with(".kml"));
    }

    #[test]
    fn a_plain_file_is_wrapped_so_atak_imports_it() {
        let (name, zip) = wrap_in_data_package("Plan <secours>.pdf", b"%PDF-1.7").unwrap();
        assert_eq!(name, "Plan <secours>.zip");
        assert!(is_data_package(&zip));
        let files = data_package_files(&zip).unwrap();
        assert_eq!(files["Plan <secours>.pdf"], b"%PDF-1.7");
        let manifest = String::from_utf8(files["MANIFEST/manifest.xml"].clone()).unwrap();
        assert!(manifest.contains("zipEntry=\"Plan &lt;secours&gt;.pdf\""));
        assert!(!is_data_package(b"%PDF-1.7"));
        let mut bare = zip::ZipWriter::new(Cursor::new(Vec::new()));
        bare.start_file("a.kml", zip::write::SimpleFileOptions::default()).unwrap();
        bare.write_all(b"<kml/>").unwrap();
        assert!(!is_data_package(&bare.finish().unwrap().into_inner()), "a zip without a manifest is not one");
    }
}
