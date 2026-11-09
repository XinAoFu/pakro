//! Test-only local archive mirror and rejecting proxy. No TLS is terminated here.
use std::{
    collections::BTreeMap,
    io::{Read, Write},
    net::{SocketAddr, TcpListener, TcpStream},
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread::{self, JoinHandle},
    time::Duration,
};

pub(super) struct Server {
    addr: SocketAddr,
    requests: Arc<Mutex<Vec<String>>>,
    stop: Arc<AtomicBool>,
    worker: Option<JoinHandle<()>>,
}

impl Server {
    /// A `None` route map rejects every request, including HTTPS CONNECT.
    pub(super) fn start(routes: Option<BTreeMap<String, PathBuf>>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        listener.set_nonblocking(true).unwrap();
        let requests = Arc::new(Mutex::new(Vec::new()));
        let stop = Arc::new(AtomicBool::new(false));
        let handler_requests = Arc::clone(&requests);
        let handler_stop = Arc::clone(&stop);
        let routes = Arc::new(routes);
        let worker = thread::spawn(move || {
            while !handler_stop.load(Ordering::Relaxed) {
                match listener.accept() {
                    Ok((stream, _)) => {
                        let requests = Arc::clone(&handler_requests);
                        let routes = Arc::clone(&routes);
                        thread::spawn(move || handle(stream, &routes, &requests));
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(10));
                    }
                    Err(error) => panic!("fixture listener failed: {error}"),
                }
            }
        });
        Self {
            addr,
            requests,
            stop,
            worker: Some(worker),
        }
    }

    pub(super) fn url(&self) -> String {
        format!("http://{}", self.addr)
    }

    pub(super) fn requests(&self) -> Vec<String> {
        self.requests.lock().unwrap().clone()
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        self.worker.take().unwrap().join().unwrap();
    }
}

fn handle(
    mut stream: TcpStream,
    routes: &Option<BTreeMap<String, PathBuf>>,
    requests: &Mutex<Vec<String>>,
) {
    stream.set_nonblocking(false).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(10)))
        .unwrap();
    stream
        .set_write_timeout(Some(Duration::from_secs(30)))
        .unwrap();
    let mut bytes = Vec::new();
    let mut byte = [0u8; 1];
    while bytes.len() < 16 * 1024 && !bytes.ends_with(b"\r\n\r\n") {
        if stream.read_exact(&mut byte).is_err() {
            return;
        }
        bytes.push(byte[0]);
    }
    let line = String::from_utf8_lossy(&bytes)
        .lines()
        .next()
        .unwrap_or("")
        .to_owned();
    eprintln!("P8_CONDA_FIXTURE_REQUEST {line}");
    requests.lock().unwrap().push(line.clone());
    let mut parts = line.split_whitespace();
    let method = parts.next().unwrap_or("");
    let path = parts.next().unwrap_or("");
    let simple_json = matches!(
        path,
        "/p9/simple.json" | "/mixed/financial-simple.json" | "/mixed/scipy-simple.json"
    );
    let file = routes
        .as_ref()
        .and_then(|routes| routes.get(path))
        .filter(|_| method == "GET");
    if let Some(path) = file {
        if let Ok(mut file) = std::fs::File::open(path) {
            let len = file.metadata().unwrap().len();
            let content_type = if simple_json {
                "application/vnd.pypi.simple.v1+json"
            } else {
                "application/octet-stream"
            };
            if let Err(error) = write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Type: {content_type}\r\nContent-Length: {len}\r\nConnection: close\r\n\r\n"
            ) {
                eprintln!("P8_CONDA_FIXTURE_HEADER_ERROR: {error}");
                return;
            }
            if let Err(error) = std::io::copy(&mut file, &mut stream) {
                eprintln!("P8_CONDA_FIXTURE_COPY_ERROR: {error}");
            }
            return;
        }
    }
    let _ = stream.write_all(
        b"HTTP/1.1 503 Service Unavailable\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
    );
}

#[test]
fn mirror_transfers_large_body_completely() {
    let directory = tempfile::tempdir().unwrap();
    let file = directory.path().join("body");
    let body: Vec<u8> = (0..2 * 1024 * 1024)
        .map(|index| (index % 251) as u8)
        .collect();
    std::fs::write(&file, &body).unwrap();
    let server = Server::start(Some(BTreeMap::from([("/body".into(), file)])));
    let mut client = TcpStream::connect(server.addr).unwrap();
    client
        .set_read_timeout(Some(Duration::from_secs(10)))
        .unwrap();
    client
        .write_all(b"GET /body HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")
        .unwrap();
    let mut response = Vec::new();
    client.read_to_end(&mut response).unwrap();
    let header_end = response
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .unwrap()
        + 4;
    assert!(response.starts_with(b"HTTP/1.1 200 OK\r\n"));
    assert_eq!(&response[header_end..], body.as_slice());
}
