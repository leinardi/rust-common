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

//! The engine's address and client settings, and the connection built from them.
//!
//! Extracted from swarm-rootless-docker/src/docker/mod.rs at e0961f77655f (`Bollard::with_timeout`,
//! the unix connection with a whole-second header timeout).

use std::str::FromStr;
use std::time::Duration;

use bollard::errors::Error as BollardError;
use bollard::{API_DEFAULT_VERSION, ClientVersion, Docker};
use http::HeaderValue;
use http::header::USER_AGENT;
use http::uri::{PathAndQuery, Uri};

use crate::error::Error;

/// The socket `DOCKER_HOST` defaults to.
const DEFAULT_HOST: &str = "unix:///var/run/docker.sock";
/// bollard's own default for the response-header timeout, in seconds.
const DEFAULT_HEADER_SECS: u64 = 120;

/// Why a client could not be built.
#[derive(Debug, thiserror::Error)]
pub enum ConnectError {
    /// A setting no engine could be reached with: the text says which.
    #[error("{0}")]
    Invalid(String),
    /// The setting needs a cargo feature this build lacks (`tcp`, `tls`).
    #[error("{0} is not supported by this build")]
    Unsupported(&'static str),
    /// bollard refused to build the client (a unix socket that does not exist, a certificate that
    /// cannot be read).
    #[error(transparent)]
    Client(#[from] BollardError),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Endpoint {
    Unix(String),
    #[cfg(feature = "tcp")]
    Tcp(String),
    #[cfg(feature = "tls")]
    Tls {
        addr: String,
        certs: std::path::PathBuf,
    },
}

/// How to reach an engine and how the client behaves; build it with [`Config::unix`] or
/// [`Config::from_env`], then [`connect`] it.
#[derive(Debug, Clone, PartialEq)]
pub struct Config {
    pub(crate) endpoint: Endpoint,
    pub(crate) api_version: Option<ClientVersion>,
    pub(crate) user_agent: Option<String>,
    header_timeout: Option<Duration>,
}

impl Config {
    /// The unix socket at `path`.
    #[must_use]
    pub fn unix(path: impl Into<String>) -> Self {
        Self::new(Endpoint::Unix(path.into()))
    }

    /// The engine `DOCKER_HOST` names (`unix://`, and with `tcp` also `tcp://` and `http://`, with
    /// `tls` also `https://`), `/var/run/docker.sock` when unset, and `DOCKER_API_VERSION`.
    ///
    /// A `tcp://` host is spoken to over TLS when `DOCKER_TLS_VERIFY` is set to anything
    /// non-empty, `0` included, as moby does; TLS then reads its `key.pem`, `cert.pem` and
    /// `ca.pem` from `DOCKER_CERT_PATH`, or `$HOME/.docker` when that is unset, as moby does. A
    /// unix host ignores both. An `http://` host with `DOCKER_TLS_VERIFY` set is refused, the two
    /// contradicting each other; `https://` is always TLS. An empty variable counts as unset. `DOCKER_TLS` (TLS without
    /// verification) is not supported and is refused rather than ignored.
    ///
    /// `getenv` is the caller's lookup, `|name| std::env::var(name).ok()` for the process.
    ///
    /// # Errors
    ///
    /// [`ConnectError::Invalid`] for an unknown scheme, an empty address, TLS with
    /// neither `DOCKER_CERT_PATH` nor `HOME` set, or a `DOCKER_API_VERSION` that is not
    /// `<major>.<minor>`; [`ConnectError::Unsupported`] for TCP or TLS this build lacks, or
    /// `DOCKER_TLS` set.
    pub fn from_env(getenv: impl Fn(&str) -> Option<String>) -> Result<Self, ConnectError> {
        let var = |name: &str| getenv(name).filter(|value| !value.is_empty());
        if var("DOCKER_TLS").is_some() {
            return Err(ConnectError::Unsupported("DOCKER_TLS (unverified TLS)"));
        }
        let host = var("DOCKER_HOST").unwrap_or_else(|| DEFAULT_HOST.to_owned());
        let endpoint = if let Some(path) = host.strip_prefix("unix://") {
            if path.is_empty() {
                return Err(invalid(format!("DOCKER_HOST {host:?} names no socket")));
            }
            Endpoint::Unix(path.to_owned())
        } else if let Some((address, tls)) = host
            .strip_prefix("tcp://")
            .map(|rest| (rest, var("DOCKER_TLS_VERIFY").is_some()))
            .or_else(|| host.strip_prefix("http://").map(|rest| (rest, false)))
            .or_else(|| host.strip_prefix("https://").map(|rest| (rest, true)))
        {
            if address.is_empty() {
                return Err(invalid(format!("DOCKER_HOST {host:?} names no address")));
            }
            if host.starts_with("http://") && var("DOCKER_TLS_VERIFY").is_some() {
                return Err(invalid(format!(
                    "DOCKER_HOST {host:?} says plain HTTP but DOCKER_TLS_VERIFY asks for TLS: \
                     use tcp:// or https://, or unset DOCKER_TLS_VERIFY"
                )));
            }
            let certs = var("DOCKER_CERT_PATH")
                .or_else(|| var("HOME").map(|home| format!("{home}/.docker")));
            remote(address, tls, certs)?
        } else {
            return Err(invalid(format!(
                "DOCKER_HOST {host:?} has an unknown scheme"
            )));
        };
        let mut config = Self::new(endpoint);
        config.api_version = var("DOCKER_API_VERSION")
            .map(|version| parse_version(&version))
            .transpose()?;
        Ok(config)
    }

    /// The API version to speak, fixed: every request goes to `/v<major>.<minor>/...`, and
    /// [`negotiate`] leaves it alone, as `DOCKER_API_VERSION` does for the docker CLI. Without it
    /// requests are sent unversioned (the engine answers at its newest API) until [`negotiate`]
    /// settles one, the engine's or bollard's default when that is lower.
    ///
    /// # Errors
    ///
    /// [`ConnectError::Invalid`] unless `version` is `<major>.<minor>`.
    pub fn api_version(mut self, version: &str) -> Result<Self, ConnectError> {
        self.api_version = Some(parse_version(version)?);
        Ok(self)
    }

    /// The `User-Agent` of every request, through bollard's request modifier (which bollard's
    /// builders have no setter for).
    #[must_use]
    pub fn user_agent(mut self, user_agent: impl Into<String>) -> Self {
        self.user_agent = Some(user_agent.into());
        self
    }

    /// bollard's timeout for the response headers, in whole seconds (at least one): the body is
    /// outside it, which is what [`bounded`](crate::bounded) is for. 120 seconds when unset, as bollard's.
    #[must_use]
    pub fn response_header_timeout(mut self, timeout: Duration) -> Self {
        self.header_timeout = Some(timeout);
        self
    }

    fn new(endpoint: Endpoint) -> Self {
        Self {
            endpoint,
            api_version: None,
            user_agent: None,
            header_timeout: None,
        }
    }
}

/// Builds the client for `config`. Nothing is sent until the first call.
///
/// # Errors
///
/// [`ConnectError::Invalid`] for a `User-Agent` no header can carry, [`ConnectError::Client`] when
/// bollard refuses (a unix socket that does not exist, a certificate it cannot read).
pub fn connect(config: Config) -> Result<Docker, ConnectError> {
    let seconds = config
        .header_timeout
        .map_or(DEFAULT_HEADER_SECS, |timeout| timeout.as_secs().max(1));
    let fixed = config.api_version;
    let version = fixed.as_ref().unwrap_or(API_DEFAULT_VERSION);
    let docker = match config.endpoint {
        Endpoint::Unix(path) => Docker::connect_with_unix(&path, seconds, version)?,
        #[cfg(feature = "tcp")]
        Endpoint::Tcp(addr) => Docker::connect_with_http(&addr, seconds, version)?,
        #[cfg(feature = "tls")]
        Endpoint::Tls { addr, certs } => Docker::connect_with_ssl(
            &addr,
            &certs.join("key.pem"),
            &certs.join("cert.pem"),
            &certs.join("ca.pem"),
            seconds,
            version,
        )?,
    };
    let user_agent = parse_user_agent(config.user_agent.as_deref())?;
    Ok(with_modifier(docker, user_agent, fixed))
}

/// The API version the requests of a client carry, the one rule for bollard's calls (the request
/// modifier) and for the header-awaiting streaming calls: the version `config` fixes; else, for a
/// client [`negotiate`] lowered below bollard's default, the version it settled on; else none, the
/// engine's newest API. `client` is the client's own [`Docker::client_version`].
pub(crate) fn sent_version(config: &Config, client: ClientVersion) -> Option<ClientVersion> {
    config
        .api_version
        .or_else(|| (client != *API_DEFAULT_VERSION).then_some(client))
}

/// Installs the request modifier on `docker`, replacing any earlier one (bollard keeps a single
/// modifier per client), so the version and user agent it carries belong to that client alone.
///
/// bollard builds every URL as `/v<version>` + path and then joins the absolute path onto it,
/// which replaces the prefix, so each request leaves unversioned. This puts it back.
fn with_modifier(
    docker: Docker,
    user_agent: Option<HeaderValue>,
    version: Option<ClientVersion>,
) -> Docker {
    docker.with_request_modifier(move |mut request| {
        if let Some(value) = &user_agent {
            request.headers_mut().insert(USER_AGENT, value.clone());
        }
        if let Some(version) = version
            && let Some(uri) = versioned(request.uri(), version)
        {
            *request.uri_mut() = uri;
        }
        request
    })
}

/// `uri` with `/v<major>.<minor>` in front of its path, or `None` when it has the prefix already
/// (or cannot be rebuilt, which leaves the request as bollard made it).
pub(crate) fn versioned(uri: &Uri, version: ClientVersion) -> Option<Uri> {
    if has_version_prefix(uri.path()) {
        return None;
    }
    let target = format!(
        "/v{}.{}{}",
        version.major_version,
        version.minor_version,
        uri.path_and_query().map_or("/", PathAndQuery::as_str)
    );
    let mut parts = uri.clone().into_parts();
    parts.path_and_query = Some(PathAndQuery::from_str(&target).ok()?);
    Uri::from_parts(parts).ok()
}

/// Whether `path` starts with `/v<digits>.<digits>/` (or is exactly that).
fn has_version_prefix(path: &str) -> bool {
    let Some(rest) = path.strip_prefix("/v") else {
        return false;
    };
    let segment = rest.split('/').next().unwrap_or_default();
    segment.split_once('.').is_some_and(|(major, minor)| {
        [major, minor]
            .iter()
            .all(|n| !n.is_empty() && n.bytes().all(|byte| byte.is_ascii_digit()))
    })
}

/// Settles the API version: left as it is when `config` fixed one, else lowered to the engine's
/// (bollard's `negotiate_version`, one `GET /version`, sent unversioned) and sent on every request
/// of the returned client after it when it is below bollard's default (a newer engine is spoken to at its newest, as before negotiating; the version belongs to that client, not to `config`). `config` must be the one `docker` was connected from (or a clone of it).
///
/// # Errors
///
/// The `GET /version` failure, sorted into its kind; the call is not bounded, wrap it in
/// [`bounded`](crate::bounded) for a whole-call timeout (`negotiate_version` takes `docker` by value, so it cannot
/// be wrapped here without losing the client on a timeout).
pub async fn negotiate(docker: Docker, config: &Config) -> Result<Docker, Error> {
    if config.api_version.is_some() {
        return Ok(docker);
    }
    let docker = docker.negotiate_version().await.map_err(Error::from)?;
    // `connect` accepted this user agent already, so it parses again.
    let user_agent = parse_user_agent(config.user_agent.as_deref()).unwrap_or(None);
    let version = sent_version(config, docker.client_version());
    Ok(with_modifier(docker, user_agent, version))
}

fn parse_user_agent(text: Option<&str>) -> Result<Option<HeaderValue>, ConnectError> {
    text.map(|text| {
        HeaderValue::from_str(text)
            .map_err(|error| invalid(format!("user agent {text:?}: {error}")))
    })
    .transpose()
}

fn invalid(message: String) -> ConnectError {
    ConnectError::Invalid(message)
}

fn parse_version(version: &str) -> Result<ClientVersion, ConnectError> {
    let number = |text: &str| {
        (!text.is_empty() && text.bytes().all(|byte| byte.is_ascii_digit()))
            .then(|| text.parse::<usize>().ok())
            .flatten()
    };
    version
        .split_once('.')
        .and_then(|(major, minor)| {
            Some(ClientVersion {
                major_version: number(major)?,
                minor_version: number(minor)?,
            })
        })
        .ok_or_else(|| invalid(format!("API version {version:?} is not <major>.<minor>")))
}

#[cfg(feature = "tls")]
fn remote(address: &str, tls: bool, certs: Option<String>) -> Result<Endpoint, ConnectError> {
    if !tls {
        return Ok(Endpoint::Tcp(format!("tcp://{address}")));
    }
    let certs = certs.ok_or_else(|| invalid("neither DOCKER_CERT_PATH nor HOME is set, so there is nowhere to read the TLS certificates from".to_owned()))?;
    Ok(Endpoint::Tls {
        addr: format!("tcp://{address}"),
        certs: certs.into(),
    })
}

#[cfg(all(feature = "tcp", not(feature = "tls")))]
fn remote(address: &str, tls: bool, _certs: Option<String>) -> Result<Endpoint, ConnectError> {
    if tls {
        return Err(ConnectError::Unsupported("TLS"));
    }
    Ok(Endpoint::Tcp(format!("tcp://{address}")))
}

#[cfg(not(feature = "tcp"))]
fn remote(_address: &str, _tls: bool, _certs: Option<String>) -> Result<Endpoint, ConnectError> {
    Err(ConnectError::Unsupported("TCP"))
}
