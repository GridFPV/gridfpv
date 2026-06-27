//! The downloadable **GridFPV RotorHazard plugin bundle** (RH plugin design D16, Slice 1).
//!
//! The Director's required-with-guided-install UX (§5) offers a one-step install: download this
//! bundle, drop the `gridfpv/` folder into RotorHazard's `plugins/` dir, and restart RH. The
//! plugin source is **embedded at compile time** ([`include_str!`]) so the served bundle is always
//! the exact build this Director speaks to (its `DIRECTOR_PROTOCOL_VERSION`) — never a drifting
//! out-of-band copy.
//!
//! The archive is a **STORE-only ZIP** (no compression) built by hand: the payload is three tiny
//! text files, so a dependency-free writer beats pulling in a zip crate (the repo keeps its
//! dependency surface small — cf. `xtask`'s hand-rolled HTTP). The entries are nested under a
//! top-level `gridfpv/` folder, so unzipping yields exactly the folder to drop into `plugins/`.

/// One embedded plugin file: its path inside the zip and its bytes.
struct BundleFile {
    /// Path within the archive (under the top-level `gridfpv/` folder).
    name: &'static str,
    /// File contents, embedded from the in-repo plugin at build time.
    bytes: &'static [u8],
}

/// The plugin files, embedded from `plugins/gridfpv/` at compile time. Keep this in step with the
/// plugin folder — a new file there must be added here to ship in the bundle.
const FILES: &[BundleFile] = &[
    BundleFile {
        name: "gridfpv/__init__.py",
        bytes: include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../plugins/gridfpv/__init__.py"
        )),
    },
    BundleFile {
        name: "gridfpv/manifest.json",
        bytes: include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../plugins/gridfpv/manifest.json"
        )),
    },
    BundleFile {
        name: "gridfpv/README.md",
        bytes: include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../plugins/gridfpv/README.md"
        )),
    },
];

/// The download filename offered to the browser.
pub const BUNDLE_FILENAME: &str = "gridfpv-plugin.zip";

/// Build the GridFPV plugin bundle as a STORE-only ZIP byte vector. Deterministic (no timestamps;
/// DOS date/time are zeroed), so the same plugin source always yields the same bytes.
pub fn plugin_zip() -> Vec<u8> {
    let mut out = Vec::new();
    // (offset of each local header, crc32, size) for the central directory pass.
    let mut entries: Vec<(u32, u32, u32, &'static str)> = Vec::with_capacity(FILES.len());

    for file in FILES {
        let offset = out.len() as u32;
        let crc = crc32(file.bytes);
        let size = file.bytes.len() as u32;
        let name = file.name.as_bytes();

        // Local file header.
        out.extend_from_slice(&0x0403_4b50u32.to_le_bytes()); // signature
        out.extend_from_slice(&20u16.to_le_bytes()); // version needed
        out.extend_from_slice(&0u16.to_le_bytes()); // flags
        out.extend_from_slice(&0u16.to_le_bytes()); // compression: 0 = store
        out.extend_from_slice(&0u16.to_le_bytes()); // mod time (zeroed)
        out.extend_from_slice(&0u16.to_le_bytes()); // mod date (zeroed)
        out.extend_from_slice(&crc.to_le_bytes());
        out.extend_from_slice(&size.to_le_bytes()); // compressed size == uncompressed (store)
        out.extend_from_slice(&size.to_le_bytes()); // uncompressed size
        out.extend_from_slice(&(name.len() as u16).to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes()); // extra length
        out.extend_from_slice(name);
        out.extend_from_slice(file.bytes);

        entries.push((offset, crc, size, file.name));
    }

    // Central directory.
    let cd_offset = out.len() as u32;
    for (offset, crc, size, name) in &entries {
        let name = name.as_bytes();
        out.extend_from_slice(&0x0201_4b50u32.to_le_bytes()); // signature
        out.extend_from_slice(&20u16.to_le_bytes()); // version made by
        out.extend_from_slice(&20u16.to_le_bytes()); // version needed
        out.extend_from_slice(&0u16.to_le_bytes()); // flags
        out.extend_from_slice(&0u16.to_le_bytes()); // compression: store
        out.extend_from_slice(&0u16.to_le_bytes()); // mod time
        out.extend_from_slice(&0u16.to_le_bytes()); // mod date
        out.extend_from_slice(&crc.to_le_bytes());
        out.extend_from_slice(&size.to_le_bytes()); // compressed size
        out.extend_from_slice(&size.to_le_bytes()); // uncompressed size
        out.extend_from_slice(&(name.len() as u16).to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes()); // extra length
        out.extend_from_slice(&0u16.to_le_bytes()); // comment length
        out.extend_from_slice(&0u16.to_le_bytes()); // disk number start
        out.extend_from_slice(&0u16.to_le_bytes()); // internal attrs
        out.extend_from_slice(&0u32.to_le_bytes()); // external attrs
        out.extend_from_slice(&offset.to_le_bytes()); // local header offset
        out.extend_from_slice(name);
    }
    let cd_size = out.len() as u32 - cd_offset;

    // End of central directory.
    let count = entries.len() as u16;
    out.extend_from_slice(&0x0605_4b50u32.to_le_bytes()); // signature
    out.extend_from_slice(&0u16.to_le_bytes()); // disk number
    out.extend_from_slice(&0u16.to_le_bytes()); // disk with CD
    out.extend_from_slice(&count.to_le_bytes()); // entries on this disk
    out.extend_from_slice(&count.to_le_bytes()); // total entries
    out.extend_from_slice(&cd_size.to_le_bytes());
    out.extend_from_slice(&cd_offset.to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes()); // comment length

    out
}

/// CRC-32 (IEEE 802.3 polynomial `0xEDB88320`, the variant ZIP uses), computed bitwise. The
/// payload is a few KB of text, so a table-free loop is plenty and keeps this self-contained.
fn crc32(bytes: &[u8]) -> u32 {
    let mut crc: u32 = 0xFFFF_FFFF;
    for &byte in bytes {
        crc ^= byte as u32;
        for _ in 0..8 {
            let mask = (crc & 1).wrapping_neg();
            crc = (crc >> 1) ^ (0xEDB8_8320 & mask);
        }
    }
    !crc
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crc32_matches_known_vector() {
        // The canonical CRC-32 of "123456789" is 0xCBF43926.
        assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
    }

    #[test]
    fn zip_has_signatures_and_all_files() {
        let zip = plugin_zip();
        // Starts with a local file header signature and ends with the EOCD signature.
        assert_eq!(&zip[0..4], &0x0403_4b50u32.to_le_bytes());
        assert!(zip.len() > 22);
        assert_eq!(
            &zip[zip.len() - 22..zip.len() - 18],
            &0x0605_4b50u32.to_le_bytes()
        );
        // Every embedded file's path appears in the archive.
        for file in FILES {
            let needle = file.name.as_bytes();
            assert!(
                zip.windows(needle.len()).any(|w| w == needle),
                "bundle is missing {}",
                file.name
            );
        }
        // The EOCD total-entry count equals the number of embedded files.
        let count = u16::from_le_bytes([zip[zip.len() - 12], zip[zip.len() - 11]]);
        assert_eq!(count as usize, FILES.len());
    }
}
