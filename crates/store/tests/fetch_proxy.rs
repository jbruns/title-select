//! The fetch client ignores proxies named in the environment, which would carry its requests past the hosts it allows.
//! On its own, as the environment is process-wide.

use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use title_select_store::fetch::{Allowed, Http};

/// A listener that answers every request with `body`, counting the connections it accepts.
fn listen(body: &'static str) -> (String, Arc<AtomicUsize>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let seen = Arc::new(AtomicUsize::new(0));
    let count = seen.clone();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let mut stream = stream.unwrap();
            count.fetch_add(1, Ordering::SeqCst);
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut line = String::new();
            while reader.read_line(&mut line).unwrap() > 0 && line != "\r\n" {
                line.clear();
            }
            let _ = write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
        }
    });
    (url, seen)
}

#[test]
fn a_proxy_named_in_the_environment_is_never_contacted() {
    let (proxy, proxied) = listen("from the proxy");
    let (provider, asked) = listen("from the provider");
    // SAFETY: this test is alone in its binary, so nothing reads the environment meanwhile.
    unsafe {
        for name in [
            "ALL_PROXY",
            "all_proxy",
            "HTTP_PROXY",
            "http_proxy",
            "HTTPS_PROXY",
            "https_proxy",
        ] {
            std::env::set_var(name, &proxy);
        }
        std::env::remove_var("NO_PROXY");
        std::env::remove_var("no_proxy");
    }

    let got = Http::new()
        .get(&format!("{provider}/datfile/psx/"), &Allowed::origin(&provider), None, 1024)
        .unwrap();

    assert_eq!(got.bytes, b"from the provider");
    assert_eq!(asked.load(Ordering::SeqCst), 1);
    assert_eq!(proxied.load(Ordering::SeqCst), 0);
}
