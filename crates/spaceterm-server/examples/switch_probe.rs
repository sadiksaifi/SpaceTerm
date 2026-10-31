//! A manual probe for #682, not a product binary: SpaceTerm's QUIC carrier across a network
//! switch, without the app. Run `switch_probe server IP:PORT SECONDS` on the server's machine and
//! `switch_probe client IP:PORT SECONDS` on the client's, then switch the client's network.
//! The client sends one Input frame per second on L0, pings after 5 s of silence and suspects
//! its path 2.5 s later, as the link does. The server echoes 400 bytes on L1. Both print wall
//! times (`S` sent, `A` arrived, `E` echoed) and path stats (`T`) to stdout. The client skips
//! certificate verification and presents no device certificate.
use spaceterm_server::connection::carrier::{Carrier, Offer, Priority};
use spaceterm_server::connection::quic::{self, QuicCarrier, QuicStream};
use std::collections::VecDeque;
use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

fn wall() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs_f64()
}

fn frame(kind: u8, body: &[u8]) -> Vec<u8> {
    let mut out = ((body.len() + 1) as u32).to_be_bytes().to_vec();
    out.push(kind);
    out.extend_from_slice(body);
    out
}

#[derive(Debug)]
struct AnyServer;
impl rustls::client::danger::ServerCertVerifier for AnyServer {
    fn verify_server_cert(
        &self,
        _: &rustls::pki_types::CertificateDer<'_>,
        _: &[rustls::pki_types::CertificateDer<'_>],
        _: &rustls::pki_types::ServerName<'_>,
        _: &[u8],
        _: rustls::pki_types::UnixTime,
    ) -> Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
        Ok(rustls::client::danger::ServerCertVerified::assertion())
    }
    fn verify_tls12_signature(
        &self,
        _: &[u8],
        _: &rustls::pki_types::CertificateDer<'_>,
        _: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        Ok(rustls::client::danger::HandshakeSignatureValid::assertion())
    }
    fn verify_tls13_signature(
        &self,
        _: &[u8],
        _: &rustls::pki_types::CertificateDer<'_>,
        _: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        Ok(rustls::client::danger::HandshakeSignatureValid::assertion())
    }
    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        rustls::crypto::ring::default_provider()
            .signature_verification_algorithms
            .supported_schemes()
    }
}

struct Pump {
    carrier: QuicCarrier,
    queue: VecDeque<(Vec<u8>, Priority)>,
}

impl Pump {
    fn send(&mut self, bytes: Vec<u8>, priority: Priority) {
        self.queue.push_back((bytes, priority));
        self.turn();
    }
    fn turn(&mut self) {
        while let Some((bytes, priority)) = self.queue.front() {
            match self.carrier.offer(bytes, *priority).expect("offer") {
                Offer::Accepted => {
                    self.queue.pop_front();
                    self.carrier.flush().expect("flush");
                }
                Offer::Full => break,
            }
        }
        self.carrier.flush().expect("flush");
    }
    fn wait(&self, until: Instant) {
        let r = self.carrier.readiness();
        let mut fds = vec![libc::pollfd {
            fd: r.read,
            events: libc::POLLIN,
            revents: 0,
        }];
        if let Some(fd) = r.write {
            fds.push(libc::pollfd {
                fd,
                events: libc::POLLOUT,
                revents: 0,
            });
        }
        if let Some(fd) = r.signal {
            fds.push(libc::pollfd {
                fd,
                events: libc::POLLIN,
                revents: 0,
            });
        }
        let deadline = r.deadline.map_or(until, |d| d.min(until));
        let ms = deadline
            .saturating_duration_since(Instant::now())
            .as_micros()
            .div_ceil(1000) as i32;
        unsafe { libc::poll(fds.as_mut_ptr(), fds.len() as libc::nfds_t, ms) };
    }
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let seconds: f64 = args[3].parse().unwrap();
    let end = Instant::now() + Duration::from_secs_f64(seconds);
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    if args[1] == "server" {
        let cert = rcgen::generate_simple_self_signed(vec![quic::SERVER_NAME.into()]).unwrap();
        let mut tls = rustls::ServerConfig::builder_with_provider(provider)
            .with_protocol_versions(&[&rustls::version::TLS13])
            .unwrap()
            .with_no_client_auth()
            .with_single_cert(
                vec![cert.cert.der().clone()],
                rustls::pki_types::PrivateKeyDer::Pkcs8(cert.signing_key.serialize_der().into()),
            )
            .unwrap();
        tls.alpn_protocols = vec![quic::ALPN.to_vec()];
        let config = quic::server_config(tls).unwrap();
        let address: SocketAddr = args[2].parse().unwrap();
        let server = quic::QuicListener::bind(address, config).unwrap();
        let mut stream = loop {
            if let Some(stream) = server.accept() {
                break stream;
            }
            assert!(Instant::now() < end, "no client");
            std::thread::sleep(Duration::from_millis(5));
        };
        stream
            .handshake(Instant::now() + Duration::from_secs(5))
            .unwrap();
        stream
            .send_bytes(
                &frame(0x46, b"hello"),
                Instant::now() + Duration::from_secs(5),
            )
            .unwrap();
        let mut pump = Pump {
            carrier: stream.into_carrier(),
            queue: VecDeque::new(),
        };
        let mut next_stats = Instant::now();
        while Instant::now() < end {
            pump.wait(next_stats.min(end));
            while let Some(f) = pump.carrier.receive().expect("receive") {
                match f.kind {
                    0x02 => {
                        let seq = u32::from_be_bytes(f.body[..4].try_into().unwrap());
                        println!("A {:.4} {seq}", wall());
                        let mut body = vec![b'.'; 400];
                        body[..4].copy_from_slice(&f.body[..4]);
                        pump.send(frame(0x43, &body), Priority::L1);
                    }
                    0x08 => pump.send(frame(0x47, &f.body), Priority::L0),
                    _ => {}
                }
            }
            pump.turn();
            if Instant::now() >= next_stats {
                println!("T {:.4} {}", wall(), pump.carrier.probe_stats());
                next_stats += Duration::from_millis(250);
            }
        }
        return;
    }
    let mut tls = rustls::ClientConfig::builder_with_provider(provider)
        .with_protocol_versions(&[&rustls::version::TLS13])
        .unwrap()
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(AnyServer))
        .with_no_client_auth();
    tls.alpn_protocols = vec![quic::ALPN.to_vec()];
    let config = quic::client_config(tls).unwrap();
    let address: SocketAddr = args[2].parse().unwrap();
    let verified = AtomicBool::new(false);
    let stream = QuicStream::connect(
        config,
        &[address],
        Instant::now() + Duration::from_secs(5),
        &verified,
    )
    .unwrap();
    let mut pump = Pump {
        carrier: stream.into_carrier(),
        queue: VecDeque::new(),
    };
    let mut next_line = Instant::now() + Duration::from_secs(1);
    let mut next_stats = Instant::now();
    let mut seq = 0u32;
    let mut last_received = Instant::now();
    let mut pinged: Option<Instant> = None;
    let mut suspected = false;
    while Instant::now() < end {
        pump.wait(next_line.min(next_stats));
        while let Some(f) = pump.carrier.receive().expect("receive") {
            last_received = Instant::now();
            pinged = None;
            suspected = false;
            if f.kind == 0x43 {
                let n = u32::from_be_bytes(f.body[..4].try_into().unwrap());
                println!("E {:.4} {n}", wall());
            }
        }
        let now = Instant::now();
        // The link's liveness: ping after 5 s of silence, suspect the path 2.5 s later.
        if pinged.is_none() && now.duration_since(last_received) >= Duration::from_secs(5) {
            pinged = Some(now);
            println!("P {:.4} ping", wall());
            pump.send(frame(0x08, &[0; 8]), Priority::L0);
        }
        if let Some(at) = pinged
            && !suspected
            && now.duration_since(at) >= Duration::from_millis(2500)
        {
            suspected = true;
            println!("U {:.4} suspect", wall());
            pump.carrier.path_suspect();
        }
        if now >= next_line {
            println!("S {:.4} {seq}", wall());
            pump.send(frame(0x02, &seq.to_be_bytes()), Priority::L0);
            seq += 1;
            next_line += Duration::from_secs(1);
        }
        pump.turn();
        if now >= next_stats {
            println!("T {:.4} {}", wall(), pump.carrier.probe_stats());
            next_stats += Duration::from_millis(250);
        }
    }
}
