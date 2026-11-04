//! The shared KiCad double in `mock_kicad` checks itself here, once, rather
//! than in every test binary that includes it.

mod mock_kicad;

use copperpour_ipc::gen::kiapi;
use mock_kicad::spawn_mock;
use nng::options::Options;
use prost::Message;
use std::time::Duration;

/// Checks the option, since no idle test can outwait every finite timeout.
#[test]
fn mock_worker_has_no_idle_deadline() {
    let mock = spawn_mock(|_| None);
    assert_eq!(
        mock.control
            .get_opt::<nng::options::RecvTimeout>()
            .expect("receive timeout"),
        None,
        "the worker must wait for requests until the guard closes its socket"
    );
}

/// The guard ends the worker on drop, even while a request it never
/// answers is pending.
#[test]
fn dropping_a_wedged_mock_joins_its_worker_promptly() {
    let mock = spawn_mock(|_| None);
    let client = nng::Socket::new(nng::Protocol::Req0).expect("client socket");
    client.dial(&mock.url).expect("mock is listening");
    client
        .send(nng::Message::from(
            kiapi::common::ApiRequest::default()
                .encode_to_vec()
                .as_slice(),
        ))
        .expect("request");

    let (done, finished) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        drop(mock);
        let _ = done.send(());
    });
    assert!(
        finished.recv_timeout(Duration::from_secs(10)).is_ok(),
        "dropping the guard must close the socket and join the worker"
    );
}
