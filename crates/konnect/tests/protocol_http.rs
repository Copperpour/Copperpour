//! Streamable HTTP transport tests (MCP spec 2025-06-18).
//!
//! Spawns the real binary with `transport = "http"` and asserts the seven
//! behaviors verified by hand at release time, plus the tools/list_changed
//! notification arriving on the SSE stream after load_toolset.
//!
//! Plain std TcpStream HTTP/1.1 — no client dependency needed for requests
//! this simple.

use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

struct HttpServer {
    child: Child,
    addr: String,
    _config: tempfile::NamedTempFile,
}

impl HttpServer {
    fn spawn() -> Self {
        // The probed port can be taken between the probe and the child's own
        // bind (TOCTOU) — same race that made the konnect-ipc mocks flaky on
        // CI. The child needs a real TCP port (separate process, no inproc
        // option), so handle the race by retrying on a fresh port instead.
        let mut last_err = String::new();
        for _ in 0..5 {
            match Self::try_spawn() {
                Ok(server) => return server,
                Err(e) => last_err = e,
            }
        }
        panic!("server never became ready: {last_err}");
    }

    fn try_spawn() -> Result<Self, String> {
        // Pick a free port by binding then dropping a listener.
        let port = {
            let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            l.local_addr().unwrap().port()
        };
        let addr = format!("127.0.0.1:{port}");

        let mut config = tempfile::Builder::new().suffix(".json").tempfile().unwrap();
        write!(config, r#"{{"transport":"http","http_address":"{addr}"}}"#).unwrap();
        config.flush().unwrap();

        let child = Command::new(env!("CARGO_BIN_EXE_copperpour"))
            .arg("--config")
            .arg(config.path())
            .stdin(Stdio::piped()) // piped stdin so it doesn't detect a TTY
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("failed to spawn konnect binary");

        let mut server = HttpServer {
            child,
            addr,
            _config: config,
        };
        let deadline = Instant::now() + Duration::from_secs(15);
        while Instant::now() < deadline {
            if let Ok((status, _, body)) = server.raw_request("GET", "/health", &[], None) {
                if status == 200 && body == "ok" {
                    return Ok(server);
                }
            }
            // Child already gone: its bind lost the port race (or it crashed).
            if let Ok(Some(status)) = server.child.try_wait() {
                return Err(format!(
                    "server exited ({status}) before becoming ready on {}",
                    server.addr
                ));
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        Err(format!("server did not become ready on {}", server.addr))
    }

    /// Minimal HTTP/1.1 request. Returns (status, headers, body).
    fn raw_request(
        &self,
        method: &str,
        path: &str,
        extra_headers: &[(&str, &str)],
        body: Option<&str>,
    ) -> std::io::Result<(u16, String, String)> {
        let mut stream = TcpStream::connect(&self.addr)?;
        stream.set_read_timeout(Some(Duration::from_secs(10)))?;
        let body = body.unwrap_or("");
        let mut req = format!(
            "{method} {path} HTTP/1.1\r\nHost: {}\r\nConnection: close\r\nContent-Length: {}\r\n",
            self.addr,
            body.len()
        );
        if body.starts_with('{') {
            req.push_str("Content-Type: application/json\r\n");
        }
        for (k, v) in extra_headers {
            req.push_str(&format!("{k}: {v}\r\n"));
        }
        req.push_str("\r\n");
        req.push_str(body);
        stream.write_all(req.as_bytes())?;

        let mut raw = String::new();
        stream.read_to_string(&mut raw)?;
        let (head, body) = raw.split_once("\r\n\r\n").unwrap_or((&raw, ""));
        let status: u16 = head
            .lines()
            .next()
            .and_then(|l| l.split_whitespace().nth(1))
            .and_then(|s| s.parse().ok())
            .unwrap_or(0);
        // Bodies may be chunked; strip chunk framing crudely if present.
        let body = if head.to_lowercase().contains("transfer-encoding: chunked") {
            body.lines()
                .filter(|l| !l.chars().all(|c| c.is_ascii_hexdigit()) || l.contains('{'))
                .collect::<Vec<_>>()
                .join("")
        } else {
            body.to_string()
        };
        Ok((status, head.to_string(), body))
    }

    fn post_mcp(&self, msg: &Value) -> (u16, String, Value) {
        let (status, headers, body) = self
            .raw_request(
                "POST",
                "/mcp",
                &[("Accept", "application/json, text/event-stream")],
                Some(&msg.to_string()),
            )
            .unwrap();
        let parsed = serde_json::from_str(&body).unwrap_or(Value::Null);
        (status, headers, parsed)
    }
}

impl Drop for HttpServer {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn initialize_msg() -> Value {
    json!({
        "jsonrpc": "2.0", "id": 1, "method": "initialize",
        "params": {"protocolVersion": "2025-06-18", "capabilities": {},
                    "clientInfo": {"name": "http-test", "version": "0"}}
    })
}

#[test]
fn streamable_http_conformance() {
    let server = HttpServer::spawn();

    // 1. POST a request → 200 with application/json body.
    let (status, headers, body) = server.post_mcp(&initialize_msg());
    assert_eq!(status, 200);
    assert!(headers.to_lowercase().contains("application/json"));
    assert_eq!(body["result"]["serverInfo"]["name"], "copperpour");

    // 2. POST a notification → 202 Accepted, no body.
    let (status, _, _) = server
        .raw_request(
            "POST",
            "/mcp",
            &[("Accept", "application/json, text/event-stream")],
            Some(r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#),
        )
        .unwrap();
    assert_eq!(status, 202);

    // 3. DELETE /mcp → 405 (stateless server, no session termination).
    let (status, _, _) = server.raw_request("DELETE", "/mcp", &[], None).unwrap();
    assert_eq!(status, 405);

    // 4. Non-localhost Origin → 403 (DNS-rebinding protection is MUST).
    let (status, _, _) = server
        .raw_request(
            "POST",
            "/mcp",
            &[("Origin", "https://evil.example.com")],
            Some(r#"{"jsonrpc":"2.0","id":9,"method":"ping"}"#),
        )
        .unwrap();
    assert_eq!(status, 403);

    // 5. Localhost Origin → allowed.
    let (status, _, _) = server
        .raw_request(
            "POST",
            "/mcp",
            &[("Origin", "http://localhost:3000")],
            Some(r#"{"jsonrpc":"2.0","id":10,"method":"ping"}"#),
        )
        .unwrap();
    assert_eq!(status, 200);
}

#[test]
fn sse_stream_delivers_tools_list_changed() {
    let server = HttpServer::spawn();
    let _ = server.post_mcp(&initialize_msg());

    // 6. GET /mcp with SSE accept → text/event-stream. Keep the socket open
    //    and read events from it while a second connection mutates state.
    let mut sse = TcpStream::connect(&server.addr).unwrap();
    sse.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
    write!(
        sse,
        "GET /mcp HTTP/1.1\r\nHost: {}\r\nAccept: text/event-stream\r\n\r\n",
        server.addr
    )
    .unwrap();

    let mut reader = BufReader::new(sse.try_clone().unwrap());
    // Read response head; must announce an event stream.
    let mut head = String::new();
    loop {
        let mut line = String::new();
        reader.read_line(&mut line).unwrap();
        if line == "\r\n" {
            break;
        }
        head.push_str(&line);
    }
    assert!(
        head.to_lowercase().contains("text/event-stream"),
        "GET /mcp did not open an SSE stream:\n{head}"
    );

    // 7. Trigger a notification: load a toolset over a separate POST.
    let (status, _, resp) = server.post_mcp(&json!({
        "jsonrpc": "2.0", "id": 2, "method": "tools/call",
        "params": {"name": "load_toolset", "arguments": {"name": "templates"}}
    }));
    assert_eq!(status, 200);
    assert_ne!(resp["result"]["isError"], json!(true));

    // The SSE stream must carry notifications/tools/list_changed.
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut saw_notification = false;
    while Instant::now() < deadline {
        let mut line = String::new();
        match reader.read_line(&mut line) {
            Ok(0) => break,
            Ok(_) => {
                if line.contains("notifications/tools/list_changed") {
                    saw_notification = true;
                    break;
                }
            }
            Err(_) => break, // read timeout
        }
    }
    assert!(
        saw_notification,
        "tools/list_changed never arrived on the SSE stream"
    );
}
