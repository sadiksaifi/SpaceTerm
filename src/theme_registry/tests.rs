use std::sync::Mutex;

use serde_json::json;

use super::*;

/// Answers every request with one prepared result and records what was asked.
struct FakeTransport {
    response: Result<Vec<u8>, TransportError>,
    requests: Mutex<Vec<(String, usize)>>,
}

impl FakeTransport {
    fn new(response: Result<Vec<u8>, TransportError>) -> Arc<Self> {
        Arc::new(Self {
            response,
            requests: Mutex::new(Vec::new()),
        })
    }
}

impl RegistryTransport for FakeTransport {
    fn get(&self, url: &str, limit: usize) -> Result<Vec<u8>, TransportError> {
        self.requests.lock().unwrap().push((url.to_owned(), limit));
        self.response.clone()
    }
}

fn listed(id: &str, downloads: u64) -> serde_json::Value {
    json!({
        "id": id,
        "name": format!("{id} themes"),
        "version": "1.2.0",
        "description": "A theme",
        "authors": ["Ada <ada@example.com>"],
        "download_count": downloads,
        "provides": ["themes"],
        "repository": "https://example.com",
    })
}

fn extension(id: &str, version: &str) -> RegistryExtension {
    RegistryExtension {
        id: id.to_owned(),
        name: id.to_owned(),
        version: version.to_owned(),
        description: None,
        authors: Vec::new(),
        downloads: 0,
    }
}

enum Entry<'a> {
    File(&'a str, &'a [u8]),
    Directory(&'a str),
    Symlink(&'a str, &'a str),
}

fn archive(entries: &[Entry<'_>]) -> Vec<u8> {
    let encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
    let mut builder = tar::Builder::new(encoder);
    for entry in entries {
        let mut header = tar::Header::new_gnu();
        header.set_mode(0o644);
        match entry {
            Entry::File(path, bytes) => {
                header.set_entry_type(tar::EntryType::Regular);
                header.set_size(bytes.len() as u64);
                header.set_cksum();
                builder.append_data(&mut header, path, *bytes).unwrap();
            }
            Entry::Directory(path) => {
                header.set_entry_type(tar::EntryType::Directory);
                header.set_size(0);
                header.set_cksum();
                builder.append_data(&mut header, path, &[][..]).unwrap();
            }
            Entry::Symlink(path, target) => {
                header.set_entry_type(tar::EntryType::Symlink);
                header.set_size(0);
                builder.append_link(&mut header, path, target).unwrap();
            }
        }
    }
    builder.into_inner().unwrap().finish().unwrap()
}

#[test]
fn listing_requests_theme_extensions_this_build_understands() {
    let body = serde_json::to_vec(&json!({ "data": [] })).unwrap();
    let transport = FakeTransport::new(Ok(body));
    let registry = ZedThemeRegistry::new(transport.clone());

    assert_eq!(registry.list(), Ok(Vec::new()));
    assert_eq!(
        *transport.requests.lock().unwrap(),
        [(
            String::from("https://api.zed.dev/extensions?provides=themes&max_schema_version=1"),
            MAX_LISTING_BYTES
        )]
    );
}

#[test]
fn listing_keeps_presentable_theme_extensions_most_downloaded_first() {
    let mut not_themes = listed("grammar", 900);
    not_themes["provides"] = json!(["languages"]);
    let mut bad_id = listed("../escape", 800);
    bad_id["id"] = json!("../escape");
    let mut bad_version = listed("version", 700);
    bad_version["version"] = json!("1.0/2");
    let mut blank_name = listed("blank", 600);
    blank_name["name"] = json!("  ");
    let mut control_description = listed("described", 5);
    control_description["description"] = json!("line\nbreak");
    let body = serde_json::to_vec(&json!({
        "data": [
            listed("alpha", 10),
            listed("zeta", 50),
            listed("beta", 10),
            not_themes,
            bad_id,
            bad_version,
            blank_name,
            control_description,
        ]
    }))
    .unwrap();

    let extensions = parse_listing(&body).unwrap();
    let ids = extensions
        .iter()
        .map(|extension| extension.id.as_str())
        .collect::<Vec<_>>();

    assert_eq!(ids, ["zeta", "alpha", "beta", "described"]);
    assert_eq!(extensions[0].authors, ["Ada"]);
    assert_eq!(extensions[0].description.as_deref(), Some("A theme"));
    assert_eq!(extensions[0].downloads, 50);
    assert_eq!(extensions[3].description, None);
}

#[test]
fn listing_rejects_malformed_and_oversized_responses() {
    assert_eq!(parse_listing(b"{"), Err(RegistryError::InvalidResponse));
    assert_eq!(
        parse_listing(br#"{"items":[]}"#),
        Err(RegistryError::InvalidResponse)
    );
    let entries = vec![listed("same", 1); MAX_LISTED_EXTENSIONS + 1];
    let body = serde_json::to_vec(&json!({ "data": entries })).unwrap();
    assert_eq!(parse_listing(&body), Err(RegistryError::TooLarge));
}

#[test]
fn transport_failures_keep_their_classification() {
    for (transport, registry) in [
        (TransportError::Unreachable, RegistryError::Unreachable),
        (TransportError::Refused, RegistryError::Refused),
        (TransportError::TooLarge, RegistryError::TooLarge),
    ] {
        let registry_client = ZedThemeRegistry::new(FakeTransport::new(Err(transport)));
        assert_eq!(registry_client.list(), Err(registry));
        assert_eq!(
            registry_client.download(&extension("theme", "1.0.0")),
            Err(registry)
        );
    }
}

#[test]
fn download_requests_the_listed_version_and_reads_only_theme_families() {
    let body = archive(&[
        Entry::File("./extension.toml", b"id = \"vague\""),
        Entry::Directory("./themes/"),
        Entry::File("./themes/vague.json", b"{\"dark\":true}"),
        Entry::File("themes/vague-light.json", b"{\"light\":true}"),
        Entry::File("./themes/nested/deep.json", b"{}"),
        Entry::File("./themes/notes.md", b"notes"),
        Entry::File("./themes/.json", b"{}"),
        Entry::File("./languages/themes/other.json", b"{}"),
        Entry::Symlink("./themes/link.json", "../extension.toml"),
    ]);
    let transport = FakeTransport::new(Ok(body));
    let registry = ZedThemeRegistry::new(transport.clone());

    let downloaded = registry.download(&extension("vague", "1.2.0")).unwrap();

    assert_eq!(downloaded.id, "vague");
    assert_eq!(downloaded.version, "1.2.0");
    assert_eq!(
        downloaded.families,
        [b"{\"dark\":true}".to_vec(), b"{\"light\":true}".to_vec()]
    );
    assert_eq!(
        *transport.requests.lock().unwrap(),
        [(
            String::from("https://api.zed.dev/extensions/vague/1.2.0/download"),
            MAX_ARCHIVE_BYTES
        )]
    );
}

#[test]
fn download_refuses_an_extension_it_cannot_address() {
    let transport = FakeTransport::new(Ok(Vec::new()));
    let registry = ZedThemeRegistry::new(transport.clone());

    for (id, version) in [("../x", "1.0.0"), ("x", "1/0"), ("", "1.0.0"), (".x", "1")] {
        assert_eq!(
            registry.download(&extension(id, version)),
            Err(RegistryError::InvalidResponse)
        );
    }
    assert!(transport.requests.lock().unwrap().is_empty());
}

#[test]
fn an_archive_without_theme_families_installs_nothing() {
    let body = archive(&[Entry::File("./extension.toml", b"id = \"x\"")]);
    assert_eq!(theme_families(&body), Err(RegistryError::NoThemes));
}

#[test]
fn a_malformed_archive_is_invalid() {
    assert_eq!(
        theme_families(b"not gzip"),
        Err(RegistryError::InvalidArchive)
    );
    let mut truncated = archive(&[Entry::File("./themes/a.json", &[b' '; 4096])]);
    truncated.truncate(truncated.len() / 2);
    assert_eq!(
        theme_families(&truncated),
        Err(RegistryError::InvalidArchive)
    );
}

#[test]
fn an_oversized_theme_family_is_refused() {
    let oversized = vec![b' '; MAX_FAMILY_BYTES + 1];
    let body = archive(&[Entry::File("./themes/huge.json", &oversized)]);
    assert_eq!(theme_families(&body), Err(RegistryError::TooLarge));
}

#[test]
fn an_extension_with_too_many_families_is_refused() {
    let names = (0..=MAX_EXTENSION_FAMILIES)
        .map(|index| format!("./themes/{index}.json"))
        .collect::<Vec<_>>();
    let entries = names
        .iter()
        .map(|name| Entry::File(name, b"{}"))
        .collect::<Vec<_>>();
    assert_eq!(
        theme_families(&archive(&entries)),
        Err(RegistryError::InvalidArchive)
    );
}
