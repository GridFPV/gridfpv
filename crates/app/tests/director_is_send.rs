//! The desktop shell (`src-tauri`) runs the Director on its own async runtime, which needs
//! the Director's future to be `Send`. The shell is not compiled by the Rust gates on a
//! machine without GTK, so nothing else would notice if something held across an await
//! stopped being `Send`. This does.

use std::path::PathBuf;

fn assert_send<T: Send>(_: &T) {}

#[test]
fn the_director_can_run_on_another_thread() {
    let director = gridfpv_app::director::run_director(
        "127.0.0.1:0".parse().unwrap(),
        None,
        PathBuf::from("."),
        |_ready| {},
        async {},
    );
    assert_send(&director);
}

/// The desktop shell binds a fixed port and, if it is taken, tries again on another. So a
/// failed bind must come back as an error, having told nobody it was ready.
#[tokio::test]
async fn a_port_that_is_taken_is_an_error_and_nothing_is_reported_ready() {
    let holder = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let taken = holder.local_addr().unwrap();
    let ready = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let seen = ready.clone();
    let result = gridfpv_app::director::run_director(
        taken,
        None,
        PathBuf::from("."),
        move |_ready| seen.store(true, std::sync::atomic::Ordering::SeqCst),
        async {},
    )
    .await;
    assert!(result.is_err(), "binding a taken port should fail");
    assert!(!ready.load(std::sync::atomic::Ordering::SeqCst));
}
