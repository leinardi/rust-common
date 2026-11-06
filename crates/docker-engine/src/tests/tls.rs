/*
 * Copyright (c) 2026 Roberto Leinardi
 *
 * Licensed under either of
 *
 *  * Apache License, Version 2.0 (LICENSE-APACHE or https://www.apache.org/licenses/LICENSE-2.0)
 *  * MIT license (LICENSE-MIT or https://opensource.org/licenses/MIT)
 *
 * at your option.
 *
 * SPDX-License-Identifier: MIT OR Apache-2.0
 */

//! TLS from `DOCKER_CERT_PATH` (`tls`), against a daemon that requires a client certificate:
//! the material is made at run time, as the Go tests make theirs.
//!
//! Extracted from swarm-scheduler-exporter/src/docker/tests.rs at c9e79afa8810.

use std::path::Path;
use std::sync::Arc;

use rcgen::{
    BasicConstraints, CertificateParams, DnType, ExtendedKeyUsagePurpose, IsCa, Issuer, KeyPair,
    KeyUsagePurpose, SanType,
};
use rustls::RootCertStore;
use rustls::server::WebPkiClientVerifier;
use rustls_pki_types::PrivateKeyDer;
use rustls_pki_types::pem::PemObject as _;
use tokio_rustls::TlsAcceptor;

use super::tcp::env;
use super::{FakeDaemon, TempDir, engine, list_containers};
use crate::{ErrorKind, PathError, Settings};

/// The TLS material of a test: a CA, a client certificate in `dir`, and a server configuration
/// for 127.0.0.1 that requires a client certificate from the same CA (Go's
/// `writeTestTLSMaterial`).
fn write_test_tls_material(dir: &Path) -> TlsAcceptor {
    let ca_key = KeyPair::generate().unwrap();
    let mut ca_params = CertificateParams::new(Vec::new()).unwrap();
    ca_params
        .distinguished_name
        .push(DnType::CommonName, "test CA");
    ca_params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
    ca_params.key_usages = vec![KeyUsagePurpose::KeyCertSign];
    let ca = ca_params.self_signed(&ca_key).unwrap();
    let issuer = Issuer::new(ca_params, ca_key);

    let client_key = KeyPair::generate().unwrap();
    let mut client_params = CertificateParams::new(Vec::new()).unwrap();
    client_params
        .distinguished_name
        .push(DnType::CommonName, "test client");
    client_params.extended_key_usages = vec![ExtendedKeyUsagePurpose::ClientAuth];
    let client = client_params.signed_by(&client_key, &issuer).unwrap();

    let server_key = KeyPair::generate().unwrap();
    let mut server_params = CertificateParams::new(Vec::new()).unwrap();
    server_params
        .distinguished_name
        .push(DnType::CommonName, "127.0.0.1");
    server_params.subject_alt_names = vec![SanType::IpAddress([127, 0, 0, 1].into())];
    server_params.extended_key_usages = vec![ExtendedKeyUsagePurpose::ServerAuth];
    let server = server_params.signed_by(&server_key, &issuer).unwrap();

    std::fs::write(dir.join("ca.pem"), ca.pem()).unwrap();
    std::fs::write(dir.join("cert.pem"), client.pem()).unwrap();
    std::fs::write(dir.join("key.pem"), client_key.serialize_pem()).unwrap();

    let mut client_roots = RootCertStore::empty();
    client_roots.add(ca.der().clone()).unwrap();
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let verifier =
        WebPkiClientVerifier::builder_with_provider(Arc::new(client_roots), Arc::clone(&provider))
            .build()
            .unwrap();
    let config = rustls::ServerConfig::builder_with_provider(provider)
        .with_protocol_versions(&[&rustls::version::TLS13, &rustls::version::TLS12])
        .unwrap()
        .with_client_cert_verifier(verifier)
        .with_single_cert(
            vec![server.der().clone()],
            PrivateKeyDer::from_pem_slice(server_key.serialize_pem().as_bytes()).unwrap(),
        )
        .unwrap();
    TlsAcceptor::from(Arc::new(config))
}

/// Go: `TestNewDockerClient_TLSFromEnv`: the daemon requires a client certificate signed by the
/// CA and the client verifies the daemon against `ca.pem`, so the call only succeeds with both
/// halves of the configuration.
#[tokio::test]
async fn tls_from_env() {
    let certs = TempDir::new();
    let acceptor = write_test_tls_material(certs.path());
    let daemon = FakeDaemon::tcp_with(Some(acceptor), Arc::new(engine(Some("1.56"))));
    let client = daemon.client_env(&[
        ("DOCKER_CERT_PATH", certs.str()),
        ("DOCKER_TLS_VERIFY", "1"),
    ]);
    assert_eq!(list_containers(&client).await.unwrap(), "[]");
    // Over TLS the Go transport still asks for gzip.
    assert_eq!(daemon.requests()[1].header("accept-encoding"), ["gzip"]);
}

/// Without the client certificate, or with a CA that did not sign the daemon's, the handshake
/// fails: verification is on.
#[tokio::test]
async fn tls_verifies_both_sides() {
    let certs = TempDir::new();
    let acceptor = write_test_tls_material(certs.path());
    let daemon = FakeDaemon::tcp_with(Some(acceptor), Arc::new(engine(Some("1.56"))));
    let other = TempDir::new();
    let _ = write_test_tls_material(other.path());
    // The other CA did not sign the daemon's certificate.
    std::fs::copy(
        other.path().join("ca.pem"),
        certs.path().join("ca-other.pem"),
    )
    .unwrap();
    let mixed = TempDir::new();
    for name in ["cert.pem", "key.pem"] {
        std::fs::copy(certs.path().join(name), mixed.path().join(name)).unwrap();
    }
    std::fs::copy(other.path().join("ca.pem"), mixed.path().join("ca.pem")).unwrap();
    let client = daemon.client_env(&[
        ("DOCKER_CERT_PATH", mixed.str()),
        ("DOCKER_TLS_VERIFY", "1"),
    ]);
    let error = list_containers(&client).await.unwrap_err();
    assert_eq!(error.kind(), ErrorKind::ConnectionFailed);
    assert!(
        error
            .to_string()
            .starts_with("error during connect: Get \"https://127.0.0.1:"),
        "{error}"
    );
    assert!(
        error.to_string().contains("tls: invalid peer certificate"),
        "{error}"
    );
}

/// The trust roots are Go's `tlsconfig.Client` ones: the system pool plus `ca.pem`.
#[test]
fn tls_roots_are_the_system_pool_plus_ca_pem() {
    let certs = TempDir::new();
    let _ = write_test_tls_material(certs.path());
    let roots = crate::tls::cert_pool(&certs.path().join("ca.pem")).unwrap();
    let mut system = RootCertStore::empty();
    let (system_count, _) =
        system.add_parsable_certificates(rustls_native_certs::load_native_certs().certs);
    assert_eq!(roots.len(), system_count + 1);
    // A CA file with no certificate in it is refused, as AppendCertsFromPEM's false is.
    std::fs::write(certs.path().join("ca.pem"), "not a certificate\n").unwrap();
    let error = crate::tls::cert_pool(&certs.path().join("ca.pem")).unwrap_err();
    assert_eq!(
        error.to_string(),
        format!(
            "failed to append certificates from PEM file: \"{}\"",
            certs.path().join("ca.pem").display()
        )
    );
}

/// Go: `TestNewDockerClient_TLSFromEnvMissingFiles`, with `DOCKER_TLS_VERIFY` set, since Q5
/// refuses Go's unset one first: the missing `ca.pem` is reported with its path, and the
/// `io::Error` is the source.
#[test]
fn tls_from_env_missing_files() {
    let certs = TempDir::new();
    let error = Settings::from_env(env(&[
        ("DOCKER_CERT_PATH", certs.str()),
        ("DOCKER_TLS_VERIFY", "1"),
    ]))
    .unwrap_err();
    let ca = certs.path().join("ca.pem");
    assert_eq!(
        error.to_string(),
        format!(
            "configure TLS from DOCKER_CERT_PATH={}: could not read CA certificate \"{}\": open \
             {}: no such file or directory",
            certs.str(),
            ca.display(),
            ca.display()
        )
    );
    let source = std::error::Error::source(&error).expect("the path error");
    let path_error = source.downcast_ref::<PathError>().expect("a PathError");
    assert_eq!(path_error.path(), ca);
    let io = std::error::Error::source(path_error)
        .and_then(|io| io.downcast_ref::<std::io::Error>())
        .expect("the io::Error");
    assert_eq!(io.kind(), std::io::ErrorKind::NotFound);

    // With ca.pem there, the client certificate is next, under Go's key pair prefix.
    let _ = write_test_tls_material(certs.path());
    std::fs::remove_file(certs.path().join("key.pem")).unwrap();
    let error = Settings::from_env(env(&[
        ("DOCKER_CERT_PATH", certs.str()),
        ("DOCKER_TLS_VERIFY", "1"),
    ]))
    .unwrap_err();
    assert_eq!(
        error.to_string(),
        format!(
            "configure TLS from DOCKER_CERT_PATH={}: could not load X509 key pair: open {}: no \
             such file or directory",
            certs.str(),
            certs.path().join("key.pem").display()
        )
    );
}

/// Q5: a certificate path with `DOCKER_TLS_VERIFY` empty is refused, with the fix in the
/// message, where Go connected without verifying the daemon. Any non-empty value, `0` included,
/// verifies, as in Go and the Docker CLI.
#[test]
fn tls_without_verify_is_refused() {
    let certs = TempDir::new();
    let _ = write_test_tls_material(certs.path());
    for verify in [None, Some("")] {
        let mut vars = vec![("DOCKER_CERT_PATH", certs.str())];
        if let Some(verify) = verify {
            vars.push(("DOCKER_TLS_VERIFY", verify));
        }
        let error = Settings::from_env(env(&vars)).unwrap_err();
        assert_eq!(
            error.to_string(),
            format!(
                "configure TLS from DOCKER_CERT_PATH={}: DOCKER_TLS_VERIFY is empty, which would \
                 connect without verifying the daemon's certificate; set DOCKER_TLS_VERIFY=1",
                certs.str()
            )
        );
    }
    for verify in ["1", "0", "false"] {
        assert!(
            Settings::from_env(env(&[
                ("DOCKER_CERT_PATH", certs.str()),
                ("DOCKER_TLS_VERIFY", verify),
            ]))
            .is_ok(),
            "{verify}"
        );
    }
    // No certificate path: plain TCP, whatever DOCKER_TLS_VERIFY says.
    assert!(Settings::from_env(env(&[("DOCKER_TLS_VERIFY", "1")])).is_ok());
}
