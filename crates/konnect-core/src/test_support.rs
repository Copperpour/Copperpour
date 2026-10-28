use konnect_ipc::gen::kiapi;
use nng::options::Options;
use prost::Message;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

/// How long the worker waits in `recv` before giving up on its own.
const RECV_TIMEOUT: Duration = Duration::from_secs(10);

/// An already-listening in-process KiCad IPC double.
///
/// The guard owns the endpoint for its entire lifetime. Dropping it closes the
/// shared NNG socket, wakes the receive loop, joins the worker, and releases
/// the endpoint name. Tests therefore never probe and relinquish a TCP port or
/// leave a detached mock thread behind.
pub(crate) struct MockIpcServer {
    address: String,
    control: nng::Socket,
    worker: Option<std::thread::JoinHandle<()>>,
}

impl MockIpcServer {
    pub(crate) fn spawn(
        purpose: &str,
        respond: impl Fn(kiapi::common::ApiRequest) -> kiapi::common::ApiResponse + Send + 'static,
    ) -> Self {
        static NEXT_SERVER: AtomicU64 = AtomicU64::new(0);
        let sequence = NEXT_SERVER.fetch_add(1, Ordering::Relaxed);
        Self::spawn_at(
            format!(
                "inproc://konnect-core-{}-{sequence}-{purpose}",
                std::process::id()
            ),
            respond,
        )
    }

    /// As [`Self::spawn`], on an endpoint the caller names.
    ///
    /// Lets one test replace the KiCad behind a *running* server without
    /// rebuilding it — the only way to exercise a session whose editor changes
    /// under it, since the endpoint is fixed when the server is configured.
    /// The previous guard must be dropped first; it releases the name.
    pub(crate) fn spawn_at(
        address: String,
        respond: impl Fn(kiapi::common::ApiRequest) -> kiapi::common::ApiResponse + Send + 'static,
    ) -> Self {
        let socket = nng::Socket::new(nng::Protocol::Rep0).expect("mock rep socket");
        socket
            .set_opt::<nng::options::RecvTimeout>(Some(RECV_TIMEOUT))
            .expect("mock receive timeout");
        socket.listen(&address).expect("mock listen");

        let control = socket.clone();
        let worker = std::thread::spawn(move || {
            while let Ok(message) = socket.recv() {
                let Ok(request) = kiapi::common::ApiRequest::decode(message.as_slice()) else {
                    break;
                };
                let response = respond(request);
                let output = nng::Message::from(response.encode_to_vec().as_slice());
                if socket.send(output).is_err() {
                    break;
                }
            }
        });

        Self {
            address,
            control,
            worker: Some(worker),
        }
    }

    pub(crate) fn address(&self) -> &str {
        &self.address
    }
}

impl Drop for MockIpcServer {
    fn drop(&mut self) {
        self.control.close();
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nng::options::Options;
    use std::collections::HashSet;
    use std::sync::atomic::AtomicBool;
    use std::sync::Arc;
    use std::time::Instant;

    /// Well under the worker's receive timeout: a drop that relied on that
    /// timeout instead of closing the socket would exceed it.
    const PROMPT_RELEASE: Duration = Duration::from_millis(RECV_TIMEOUT.as_millis() as u64 / 10);

    /// Drops the guard and asserts it finished well under the receive timeout.
    fn drop_promptly(server: MockIpcServer) {
        let started = Instant::now();
        drop(server);
        let elapsed = started.elapsed();
        assert!(
            elapsed < PROMPT_RELEASE,
            "dropping the guard took {elapsed:?}; it must close the socket, not wait out the receive timeout"
        );
    }

    /// Under `PROMPT_RELEASE` and the failure test's 1 s client timeout, so a
    /// joining guard and a panicked worker both set the flag in time.
    const WORKER_EXIT_PAUSE: Duration = Duration::from_millis(200);

    /// Moved into a responder, so the worker drops it only as it exits. The
    /// pause keeps the flag clear for a guard that detaches the worker instead
    /// of joining it.
    struct WorkerExit(Arc<AtomicBool>);

    impl Drop for WorkerExit {
        fn drop(&mut self) {
            std::thread::sleep(WORKER_EXIT_PAUSE);
            self.0.store(true, Ordering::SeqCst);
        }
    }

    /// As [`MockIpcServer::spawn`], with a flag the worker sets as it exits.
    fn spawn_witnessed(
        purpose: &str,
        respond: impl Fn(kiapi::common::ApiRequest) -> kiapi::common::ApiResponse + Send + 'static,
    ) -> (MockIpcServer, Arc<AtomicBool>) {
        let exited = Arc::new(AtomicBool::new(false));
        let witness = WorkerExit(Arc::clone(&exited));
        let server = MockIpcServer::spawn(purpose, move |request| {
            let _ = &witness;
            respond(request)
        });
        (server, exited)
    }

    fn ok_response() -> kiapi::common::ApiResponse {
        kiapi::common::ApiResponse {
            status: Some(kiapi::common::ApiResponseStatus {
                status: kiapi::common::ApiStatusCode::AsOk as i32,
                error_message: String::new(),
            }),
            header: None,
            message: None,
        }
    }

    fn round_trip(server: &MockIpcServer) {
        let socket = nng::Socket::new(nng::Protocol::Req0).expect("mock client socket");
        socket
            .set_opt::<nng::options::RecvTimeout>(Some(Duration::from_secs(1)))
            .expect("client timeout");
        socket.dial(server.address()).expect("server is ready");
        let request = kiapi::common::ApiRequest {
            header: None,
            message: None,
        };
        socket
            .send(nng::Message::from(request.encode_to_vec().as_slice()))
            .expect("request");
        let response = socket.recv().expect("response");
        let response = kiapi::common::ApiResponse::decode(response.as_slice()).expect("decode");
        assert_eq!(
            response.status.expect("status").status,
            kiapi::common::ApiStatusCode::AsOk as i32
        );
    }

    #[test]
    fn returned_server_is_already_bound_and_responsive() {
        let server = MockIpcServer::spawn("ready", |_| ok_response());
        round_trip(&server);
    }

    #[test]
    fn concurrent_servers_own_distinct_endpoints() {
        let servers: Vec<_> = (0..64)
            .map(|_| MockIpcServer::spawn("parallel", |_| ok_response()))
            .collect();
        let addresses: HashSet<_> = servers.iter().map(MockIpcServer::address).collect();
        assert_eq!(addresses.len(), servers.len());
        for server in &servers {
            round_trip(server);
        }
    }

    #[test]
    fn dropping_the_guard_closes_and_releases_its_endpoint() {
        let (server, exited) = spawn_witnessed("release", |_| ok_response());
        let address = server.address().to_string();
        drop_promptly(server);
        assert!(exited.load(Ordering::SeqCst), "the worker must have exited");

        let replacement = nng::Socket::new(nng::Protocol::Rep0).expect("replacement socket");
        replacement
            .listen(&address)
            .expect("the dropped guard releases the in-process endpoint");
    }

    #[test]
    fn responder_failure_still_releases_the_endpoint() {
        let (server, exited) =
            spawn_witnessed("failed-response", |_| panic!("mock responder failed"));
        let address = server.address().to_string();
        let socket = nng::Socket::new(nng::Protocol::Req0).expect("mock client socket");
        socket
            .set_opt::<nng::options::RecvTimeout>(Some(Duration::from_secs(1)))
            .expect("client timeout");
        socket.dial(server.address()).expect("server is ready");
        let request = kiapi::common::ApiRequest {
            header: None,
            message: None,
        };
        socket
            .send(nng::Message::from(request.encode_to_vec().as_slice()))
            .expect("request");
        assert!(socket.recv().is_err(), "failed responder must not reply");
        // The panic ends the worker on its own, before the guard is dropped;
        // only the endpoint check below depends on `Drop` here.
        assert!(
            exited.load(Ordering::SeqCst),
            "the failed worker must have exited"
        );
        drop(socket);
        drop_promptly(server);

        let replacement = nng::Socket::new(nng::Protocol::Rep0).expect("replacement socket");
        replacement
            .listen(&address)
            .expect("a failed worker must not leak its endpoint");
    }

    #[test]
    fn repeated_fixture_lifecycles_remain_ready_and_releasable() {
        for _ in 0..32 {
            let server = MockIpcServer::spawn("repeated", |_| ok_response());
            round_trip(&server);
            let address = server.address().to_string();
            drop_promptly(server);

            let replacement = nng::Socket::new(nng::Protocol::Rep0).expect("replacement socket");
            replacement
                .listen(&address)
                .expect("each fixture lifecycle releases its endpoint");
        }
    }
}
