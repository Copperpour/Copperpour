//! A KiCad double for the IPC client tests: an in-process NNG rep0 socket that
//! decodes each `ApiRequest` and answers with a canned `ApiResponse`.
//!
//! The guard owns the socket. The worker has no receive timeout, so it keeps
//! answering however long a loaded runner idles before the first request
//! (Copperpour/Copperpour#272); dropping the guard closes the socket, which
//! wakes the worker, and joins it. `copperpour-core` has the same guard as
//! `test_support::MockIpcServer`, which these tests cannot reach.

use copperpour_ipc::gen::kiapi;
use prost::Message;
use std::sync::atomic::{AtomicUsize, Ordering};

pub struct MockKicad {
    pub url: String,
    /// Read by `mock_kicad_test.rs`, which checks the guard itself.
    pub(crate) control: nng::Socket,
    worker: Option<std::thread::JoinHandle<()>>,
}

/// Answers each request with `respond`. `None` plays a wedged KiCad: the
/// request is accepted and never answered, so the client waits out its own
/// receive timeout.
pub fn spawn_mock<F>(respond: F) -> MockKicad
where
    F: Fn(kiapi::common::ApiRequest) -> Option<kiapi::common::ApiResponse> + Send + 'static,
{
    // inproc:// needs no port, so there is no bind-a-TcpListener-then-relisten
    // window for another process to claim the port in.
    static NEXT_MOCK: AtomicUsize = AtomicUsize::new(0);
    let url = format!(
        "inproc://mock-kicad-{}",
        NEXT_MOCK.fetch_add(1, Ordering::Relaxed)
    );

    // Listen before returning: the client dials as soon as this returns, and
    // NNG's dial fails at once if nothing is bound yet.
    let socket = nng::Socket::new(nng::Protocol::Rep0).expect("mock rep socket");
    socket.listen(&url).expect("mock listen");

    let control = socket.clone();
    let worker = std::thread::spawn(move || {
        while let Ok(message) = socket.recv() {
            let Ok(request) = kiapi::common::ApiRequest::decode(message.as_slice()) else {
                break;
            };
            // Without a reply, the next `recv` drops the request unanswered.
            let Some(response) = respond(request) else {
                continue;
            };
            let output = nng::Message::from(response.encode_to_vec().as_slice());
            if socket.send(output).is_err() {
                break;
            }
        }
    });

    MockKicad {
        url,
        control,
        worker: Some(worker),
    }
}

impl Drop for MockKicad {
    fn drop(&mut self) {
        self.control.close();
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}
