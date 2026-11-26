//! Explanations of the certificate errors of downloads: which certificates the server presented
//! instead of one the system trusts, fetched again over a connection that accepts any.

use std::io::{Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use base64::prelude::{Engine, BASE64_STANDARD};
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use rustls::{CertificateError, DigitallySignedStruct, SignatureScheme};
use x509_cert::der::Decode;

const TIMEOUT: Duration = Duration::from_secs(15);

/// If `error` is a certificate of the server at `url` failing verification, what the system
/// expected and which certificates the server presents, reached through `proxy` unless the
/// proxy excludes that host, like the failed request.
pub fn explain(error: &ureq::Error, url: &str, proxy: Option<&ureq::Proxy>) -> Option<String> {
    let reason = certificate_error(error)?;
    let uri: ureq::http::Uri = url.parse().ok()?;
    let host = uri.host()?;
    let port = uri.port_u16().unwrap_or(443);
    let proxy = proxy.filter(|proxy| !proxy.is_no_proxy(&uri));

    let mut text =
        format!("\n  expected: a certificate for {host} from an authority this system trusts");
    match presented_chain(host, port, proxy) {
        Ok(chain) => {
            text.push_str("\n  received: ");
            text.push_str(&issuer_path(&chain).join("\n            issued by "));
        }
        Err(e) => text.push_str(&format!(
            "\n  received: unknown, fetching the certificates again failed: {e}"
        )),
    }
    if matches!(reason, CertificateError::UnknownIssuer) {
        text.push_str(
            "\n  This usually means that a proxy or security software of your organization \
             intercepts TLS connections, but it can also be a man-in-the-middle attack. \
             Only if you know that the last authority above belongs to your organization, \
             install its certificate in the system's certificate store \
             (on Linux, SSL_CERT_FILE or SSL_CERT_DIR replace that store when set); \
             if you are not sure, ask your IT department. \
             Turning verification off, as `curl -k` does, would hide an attack as well.",
        );
    }
    Some(text)
}

fn certificate_error(error: &ureq::Error) -> Option<&CertificateError> {
    let error = match error {
        ureq::Error::Rustls(error) => error,
        ureq::Error::Io(error) => error.get_ref()?.downcast_ref::<rustls::Error>()?,
        _ => return None,
    };
    match error {
        rustls::Error::InvalidCertificate(reason) => Some(reason),
        _ => None,
    }
}

/// The subject of the first certificate of `chain`, followed by the issuer of each: the path
/// from the server to the authority the chain claims to come from.
fn issuer_path(chain: &[CertificateDer<'_>]) -> Vec<String> {
    let mut names = Vec::new();
    for (index, der) in chain.iter().enumerate() {
        let Ok(certificate) = x509_cert::Certificate::from_der(der) else {
            names.push("a certificate that could not be parsed".to_owned());
            break;
        };
        if index == 0 {
            names.push(certificate.tbs_certificate.subject.to_string());
        }
        names.push(certificate.tbs_certificate.issuer.to_string());
    }
    names
}

/// The certificates the server at `host`:`port` presents, without verifying them.
fn presented_chain(
    host: &str,
    port: u16,
    proxy: Option<&ureq::Proxy>,
) -> Result<Vec<CertificateDer<'static>>, String> {
    let mut stream = match proxy {
        Some(proxy) => tunnel(proxy, host, port)?,
        None => connect(host, port)?,
    };
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let recorder = Arc::new(Recorder {
        chain: Mutex::new(None),
        schemes: provider
            .signature_verification_algorithms
            .supported_schemes(),
    });
    let config = rustls::ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .map_err(|e| e.to_string())?
        .dangerous()
        .with_custom_certificate_verifier(recorder.clone())
        .with_no_client_auth();
    let name = ServerName::try_from(host.to_owned()).map_err(|e| e.to_string())?;
    let mut connection =
        rustls::ClientConnection::new(Arc::new(config), name).map_err(|e| e.to_string())?;
    // The recorder ends the handshake as soon as it has the certificates.
    let mut result = Ok(());
    while result.is_ok() && connection.is_handshaking() {
        result = connection.complete_io(&mut stream).map(|_| ());
    }
    let chain = recorder.chain.lock().unwrap().take();
    chain.ok_or_else(|| match result {
        Err(e) => e.to_string(),
        Ok(()) => "the server presented no certificate".to_owned(),
    })
}

fn connect(host: &str, port: u16) -> Result<TcpStream, String> {
    let fail = |e: std::io::Error| format!("could not connect to {host}:{port}: {e}");
    let mut last_error = None;
    for address in (host, port).to_socket_addrs().map_err(fail)? {
        match TcpStream::connect_timeout(&address, TIMEOUT) {
            Ok(stream) => {
                stream.set_read_timeout(Some(TIMEOUT)).map_err(fail)?;
                stream.set_write_timeout(Some(TIMEOUT)).map_err(fail)?;
                return Ok(stream);
            }
            Err(e) => last_error = Some(e),
        }
    }
    Err(fail(last_error.unwrap_or_else(|| {
        std::io::Error::other("the name resolved to no address")
    })))
}

/// A connection to `host`:`port` through the HTTP CONNECT `proxy`, as ureq makes it.
fn tunnel(proxy: &ureq::Proxy, host: &str, port: u16) -> Result<TcpStream, String> {
    if proxy.protocol() != ureq::ProxyProtocol::Http {
        return Err(format!(
            "the {:?} proxy at {} is not supported",
            proxy.protocol(),
            proxy.host()
        ));
    }
    let mut stream = connect(proxy.host(), proxy.port())?;
    let mut request = format!("CONNECT {host}:{port} HTTP/1.1\r\nHost: {host}:{port}\r\n");
    if proxy.username().is_some() || proxy.password().is_some() {
        let credentials = format!(
            "{}:{}",
            proxy.username().unwrap_or_default(),
            proxy.password().unwrap_or_default()
        );
        let credentials = BASE64_STANDARD.encode(credentials);
        request.push_str(&format!("Proxy-Authorization: Basic {credentials}\r\n"));
    }
    request.push_str("\r\n");
    let fail = |e: std::io::Error| format!("could not talk to the proxy at {}: {e}", proxy.host());
    stream.write_all(request.as_bytes()).map_err(fail)?;

    // Read byte by byte, so that nothing after the response head is consumed.
    let mut head = Vec::new();
    while !head.ends_with(b"\r\n\r\n") {
        if head.len() > 64 * 1024 {
            return Err(format!(
                "the proxy at {} sent no response head",
                proxy.host()
            ));
        }
        let mut byte = [0];
        match stream.read(&mut byte).map_err(fail)? {
            0 => {
                return Err(format!(
                    "the proxy at {} closed the connection",
                    proxy.host()
                ))
            }
            _ => head.push(byte[0]),
        }
    }
    let head = String::from_utf8_lossy(&head);
    let status = head.lines().next().unwrap_or_default();
    if status.split(' ').nth(1) != Some("200") {
        return Err(format!("the proxy at {} answered `{status}`", proxy.host()));
    }
    Ok(stream)
}

/// Accepts no certificate, but keeps the ones it was asked to verify.
#[derive(Debug)]
struct Recorder {
    chain: Mutex<Option<Vec<CertificateDer<'static>>>>,
    schemes: Vec<SignatureScheme>,
}

impl ServerCertVerifier for Recorder {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        let chain = std::iter::once(end_entity)
            .chain(intermediates)
            .map(|certificate| certificate.clone().into_owned())
            .collect();
        *self.chain.lock().unwrap() = Some(chain);
        Err(rustls::Error::General(
            "recorded the certificates".to_owned(),
        ))
    }

    fn verify_tls12_signature(
        &self,
        _message: &[u8],
        _cert: &CertificateDer<'_>,
        _dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        Err(rustls::Error::General("not verifying".to_owned()))
    }

    fn verify_tls13_signature(
        &self,
        _message: &[u8],
        _cert: &CertificateDer<'_>,
        _dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        Err(rustls::Error::General("not verifying".to_owned()))
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.schemes.clone()
    }
}

#[cfg(test)]
mod tests {
    use std::net::TcpListener;

    use rustls::pki_types::pem::PemObject;
    use rustls::pki_types::PrivateKeyDer;

    use super::*;

    /// A leaf certificate for localhost, issued by an intermediate CA, which a root CA issued.
    const CHAIN: &str = "\
-----BEGIN CERTIFICATE-----
MIIB0DCCAXagAwIBAgIUeaPLxw/vlfOTkwhKMRwpJYoKivcwCgYIKoZIzj0EAwIw
QDEWMBQGA1UECgwNRXhhbXBsZSBQcm94eTEmMCQGA1UEAwwdRXhhbXBsZSBQcm94
eSBJbnRlcm1lZGlhdGUgQ0EwIBcNMjYxMTI2MTM0ODAyWhgPMjEyNjExMDIxMzQ4
MDJaMBQxEjAQBgNVBAMMCWxvY2FsaG9zdDBZMBMGByqGSM49AgEGCCqGSM49AwEH
A0IABGYmKOUOUTYX/WA5BNVhVzUUQDEj/rOU+9NUyKQQ8I0ilfLcpNBPlC9vhCfV
YhSQksM4xal//FPipzdujxzOfSGjeDB2MBQGA1UdEQQNMAuCCWxvY2FsaG9zdDAT
BgNVHSUEDDAKBggrBgEFBQcDATAJBgNVHRMEAjAAMB0GA1UdDgQWBBS8O+X7HiUE
sdm1WXgABppo6ezc+TAfBgNVHSMEGDAWgBQrlin4HSQ1m5QQY8axPJm2O2rmkjAK
BggqhkjOPQQDAgNIADBFAiAYfn0c5iHm7WHdjgnZBIbC6rl4hYz+VNFA5K9G94Ls
tAIhAJKSbJNiDlj9z43PXKLfEz6Mz2OLT9baWiSAgsryOv8f
-----END CERTIFICATE-----
-----BEGIN CERTIFICATE-----
MIIB3jCCAYWgAwIBAgIUBxFQZQuT6y4R1uBOLI3M3f6vw3MwCgYIKoZIzj0EAwIw
ODEWMBQGA1UECgwNRXhhbXBsZSBQcm94eTEeMBwGA1UEAwwVRXhhbXBsZSBQcm94
eSBSb290IENBMCAXDTI2MTEyNjEzNDgwMloYDzIxMjYxMTAyMTM0ODAyWjBAMRYw
FAYDVQQKDA1FeGFtcGxlIFByb3h5MSYwJAYDVQQDDB1FeGFtcGxlIFByb3h5IElu
dGVybWVkaWF0ZSBDQTBZMBMGByqGSM49AgEGCCqGSM49AwEHA0IABFysjBKj5u1q
YanilYSPEOh47RDmHvA8c4dStrWXUks1bK0zwCWJZfXLVA4LYgQ1337pEJ2+1k90
7UH3sMZgF4yjYzBhMA8GA1UdEwEB/wQFMAMBAf8wDgYDVR0PAQH/BAQDAgIEMB0G
A1UdDgQWBBQrlin4HSQ1m5QQY8axPJm2O2rmkjAfBgNVHSMEGDAWgBQsyoB4hsBQ
t+gClyO1IuVN1VQI6zAKBggqhkjOPQQDAgNHADBEAiBbeeHPOq4cEkBhHr5vdwYk
q791ED8ZmQnJWLob/1XCygIgNBQ0v4lkFe25uDzb2jbJxYQF6/0UYKOCqmsq2c2g
tDs=
-----END CERTIFICATE-----
";
    const KEY: &str = "\
-----BEGIN PRIVATE KEY-----
MIGHAgEAMBMGByqGSM49AgEGCCqGSM49AwEHBG0wawIBAQQgYyOta/k8jTQo+oNU
jiiDQSAn0QKHyHRK6krdNMmP4cGhRANCAARmJijlDlE2F/1gOQTVYVc1FEAxI/6z
lPvTVMikEPCNIpXy3KTQT5Qvb4Qn1WIUkJLDOMWpf/xT4qc3bo8czn0h
-----END PRIVATE KEY-----
";

    fn chain() -> Vec<CertificateDer<'static>> {
        CertificateDer::pem_slice_iter(CHAIN.as_bytes())
            .collect::<Result<_, _>>()
            .unwrap()
    }

    /// The port of a TLS server presenting `chain()` to one client.
    fn serve() -> u16 {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let key = PrivateKeyDer::from_pem_slice(KEY.as_bytes()).unwrap();
        let config = rustls::ServerConfig::builder_with_provider(Arc::new(
            rustls::crypto::ring::default_provider(),
        ))
        .with_safe_default_protocol_versions()
        .unwrap()
        .with_no_client_auth()
        .with_single_cert(chain(), key)
        .unwrap();
        std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut connection = rustls::ServerConnection::new(Arc::new(config)).unwrap();
            while connection.is_handshaking() && connection.complete_io(&mut stream).is_ok() {}
        });
        port
    }

    /// The port of an HTTP CONNECT proxy for one client, which tunnels to `upstream` whatever
    /// the client asks for and sends the request head it got through the returned receiver.
    fn proxy(upstream: u16) -> (u16, std::sync::mpsc::Receiver<String>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let (sender, receiver) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let (mut client, _) = listener.accept().unwrap();
            let mut head = Vec::new();
            while !head.ends_with(b"\r\n\r\n") {
                let mut byte = [0];
                client.read_exact(&mut byte).unwrap();
                head.push(byte[0]);
            }
            sender.send(String::from_utf8(head).unwrap()).unwrap();
            let mut server = TcpStream::connect(("127.0.0.1", upstream)).unwrap();
            client
                .write_all(b"HTTP/1.1 200 Connection established\r\n\r\n")
                .unwrap();
            let (mut client_reader, mut server_writer) =
                (client.try_clone().unwrap(), server.try_clone().unwrap());
            std::thread::spawn(move || std::io::copy(&mut client_reader, &mut server_writer));
            let _ = std::io::copy(&mut server, &mut client);
        });
        (port, receiver)
    }

    fn unknown_issuer() -> ureq::Error {
        ureq::Error::Io(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            rustls::Error::InvalidCertificate(CertificateError::UnknownIssuer),
        ))
    }

    #[test]
    fn lists_the_issuers_from_the_server_up() {
        assert_eq!(
            issuer_path(&chain()),
            [
                "CN=localhost",
                "CN=Example Proxy Intermediate CA,O=Example Proxy",
                "CN=Example Proxy Root CA,O=Example Proxy",
            ]
        );
        assert_eq!(
            issuer_path(&[CertificateDer::from(&b"junk"[..])]),
            ["a certificate that could not be parsed"]
        );
    }

    #[test]
    fn fetches_the_presented_chain_directly() {
        let port = serve();
        assert_eq!(presented_chain("localhost", port, None).unwrap(), chain());
    }

    #[test]
    fn fetches_the_presented_chain_through_the_proxy() {
        let (proxy_port, request) = proxy(serve());
        let proxy =
            ureq::Proxy::new(&format!("http://user:secret@127.0.0.1:{proxy_port}")).unwrap();

        let presented = presented_chain("github.com", 443, Some(&proxy)).unwrap();

        assert_eq!(presented, chain());
        assert_eq!(
            request.recv().unwrap(),
            "CONNECT github.com:443 HTTP/1.1\r\nHost: github.com:443\r\n\
             Proxy-Authorization: Basic dXNlcjpzZWNyZXQ=\r\n\r\n"
        );
    }

    #[test]
    fn explains_an_unknown_issuer_with_the_presented_chain() {
        let port = serve();
        let text = explain(
            &unknown_issuer(),
            &format!("https://localhost:{port}/a"),
            None,
        )
        .unwrap();

        assert!(
            text.starts_with(
                "\n  expected: a certificate for localhost from an authority this system trusts\
                 \n  received: CN=localhost\
                 \n            issued by CN=Example Proxy Intermediate CA,O=Example Proxy\
                 \n            issued by CN=Example Proxy Root CA,O=Example Proxy\
                 \n  This usually means that a proxy"
            ),
            "{text}"
        );
        for part in ["man-in-the-middle", "ask your IT department", "`curl -k`"] {
            assert!(text.contains(part), "{text}");
        }
    }

    #[test]
    fn says_why_the_chain_is_unknown() {
        let port = TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port();
        let expired =
            ureq::Error::Rustls(rustls::Error::InvalidCertificate(CertificateError::Expired));

        let text = explain(&expired, &format!("https://127.0.0.1:{port}/"), None).unwrap();

        assert!(
            text.contains("\n  received: unknown, fetching the certificates again failed: could not connect to 127.0.0.1:"),
            "{text}"
        );
        assert!(!text.contains("intercepts"), "{text}");
    }

    #[test]
    fn leaves_other_errors_alone() {
        let url = "https://github.com/";
        assert!(explain(&ureq::Error::Io(std::io::Error::other("reset")), url, None).is_none());
        assert!(explain(&ureq::Error::Rustls(rustls::Error::DecryptError), url, None).is_none());
        assert!(explain(&ureq::Error::Timeout(ureq::Timeout::Connect), url, None).is_none());
    }
}
