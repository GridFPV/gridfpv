//! The one place an outgoing HTTP client is made.
//!
//! The Director talks to timers over plain HTTP on the field network, and to a remote
//! server over HTTPS (docs/primitives.md §13.5). Both use `reqwest`, which is built here
//! with its TLS library left for the program to choose (`rustls-no-provider`).
//!
//! **That choice must be made before any client is built, HTTPS or not, or building one
//! panics.** So every client is built through [`builder`] or [`client`], which make the
//! choice first. Do not call `reqwest::Client::new()` or `reqwest::Client::builder()`
//! anywhere else.
//!
//! The choice is `ring`: it builds with a plain C compiler on all three release platforms.

/// Choose the TLS library for this process. Safe to call any number of times, and fine if
/// another part of the program chose already.
pub fn tls_ready() {
    let _ = rustls::crypto::ring::default_provider().install_default();
}

/// A builder for an HTTP client, with TLS ready.
pub fn builder() -> reqwest::ClientBuilder {
    tls_ready();
    reqwest::Client::builder()
}

/// An HTTP client with the usual settings.
pub fn client() -> reqwest::Client {
    builder()
        .build()
        .expect("an HTTP client with no special settings can always be built")
}

#[cfg(test)]
mod tests {
    /// Every Rust file under `dir`, but not build output.
    fn sources(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
        for entry in std::fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                if path.file_name().is_some_and(|name| name != "target") {
                    sources(&path, out);
                }
            } else if path.extension().is_some_and(|ext| ext == "rs") {
                out.push(path);
            }
        }
    }

    #[test]
    fn no_client_is_built_anywhere_but_here() {
        // A client built directly works in a build that never chose TLS and panics in one
        // that did not choose it yet. That is found in the field, not by the compiler.
        let crates = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap();
        let mut files = Vec::new();
        sources(crates, &mut files);
        assert!(files.len() > 20, "the crates were found");
        let here = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/http.rs");
        // Spelled in two parts so this file's own text is not a match.
        let direct = [
            concat!("reqwest::Client", "::new("),
            concat!("reqwest::Client", "::builder("),
            concat!("reqwest::ClientBuilder", "::new("),
            concat!("reqwest::", "get("),
        ];
        for file in files {
            if file == here {
                continue;
            }
            let text = std::fs::read_to_string(&file).unwrap();
            for pattern in direct {
                assert!(
                    !text.contains(pattern),
                    "{} builds an HTTP client with `{pattern}`. Use gridfpv_server::http instead.",
                    file.display()
                );
            }
        }
    }

    #[test]
    fn a_client_can_be_built_first_thing_in_a_thread_that_never_chose_tls() {
        // What used to work by accident and then stopped: a plain client, with nothing
        // set up before it.
        std::thread::spawn(|| {
            let _ = super::client();
            let _ = super::builder().https_only(true).build().unwrap();
        })
        .join()
        .unwrap();
    }
}
