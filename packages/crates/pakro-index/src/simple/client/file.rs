//! Explicit file-body transport over the source client's checked HTTP kernel.
//!
//! This is a transport receipt, never a fresh pin or an expected-digest claim.
//! File credentials are supplied independently of the registry session.

use std::io::Write;

use super::*;

pub struct SourceFileSession<'a> {
    pub credential_ref: Option<&'a CredentialRef>,
    pub auth_scope: &'a AuthScopeKey,
    pub credentials: &'a dyn SourceCredentials,
    pub auth_origins: &'a AuthOriginGrants,
    pub transport: &'a dyn SourceRequestRouter,
}
impl fmt::Debug for SourceFileSession<'_> {
    fn fmt(&self, output: &mut fmt::Formatter<'_>) -> fmt::Result {
        output
            .debug_struct("SourceFileSession")
            .field("has_credential_ref", &self.credential_ref.is_some())
            .finish_non_exhaustive()
    }
}

#[derive(Debug, Clone, Copy)]
pub struct SourceFileLimits {
    pub max_wire_bytes: u64,
    pub max_header_bytes: usize,
    pub max_headers: usize,
}
impl Default for SourceFileLimits {
    fn default() -> Self {
        Self {
            max_wire_bytes: 512 * 1024 * 1024,
            max_header_bytes: 64 * 1024,
            max_headers: 64,
        }
    }
}

/// A completed body copy over one checked logical/physical request chain.
/// Its raw URL getters are request-only facts for explicit application binding;
/// their values must never be logged or treated as portable file identities.
pub struct CompletedSourceTransfer {
    initial_canonical: Url,
    initial_request: Url,
    canonical: Url,
    request: Url,
    auth_scope: AuthScopeKey,
    transport_fingerprint: TransportRulesFingerprint,
    size: u64,
}
impl CompletedSourceTransfer {
    pub fn initial_canonical_url(&self) -> &Url {
        &self.initial_canonical
    }
    pub fn initial_request_url(&self) -> &Url {
        &self.initial_request
    }
    pub fn canonical_url(&self) -> &Url {
        &self.canonical
    }
    pub fn request_url(&self) -> &Url {
        &self.request
    }
    pub fn auth_scope(&self) -> &AuthScopeKey {
        &self.auth_scope
    }
    pub fn transport_fingerprint(&self) -> TransportRulesFingerprint {
        self.transport_fingerprint
    }
    pub fn size(&self) -> u64 {
        self.size
    }
}
impl fmt::Debug for CompletedSourceTransfer {
    fn fmt(&self, output: &mut fmt::Formatter<'_>) -> fmt::Result {
        output
            .debug_struct("CompletedSourceTransfer")
            .field("size", &self.size)
            .finish_non_exhaustive()
    }
}

impl SimpleIndexClient {
    /// Copy a complete raw file body to a caller-owned sink. The caller owns
    /// cleanup after any error: partial bytes are never a completed transfer.
    /// No artifact cache, source pin, metadata or installation is published.
    pub async fn copy_source_file(
        &self,
        session: SourceFileSession<'_>,
        canonical: &Url,
        network: NetworkPolicy,
        limits: SourceFileLimits,
        output: &mut (dyn Write + Send),
    ) -> SimpleClientResult<CompletedSourceTransfer> {
        self.copy_source_file_with_budget(session, canonical, network, limits, output, None)
            .await
    }

    /// The same bounded transfer with a caller-owned absolute budget. The
    /// earlier configured/client deadline remains in force for every hop.
    pub async fn copy_source_file_at_deadline(
        &self,
        session: SourceFileSession<'_>,
        canonical: &Url,
        network: NetworkPolicy,
        limits: SourceFileLimits,
        output: &mut (dyn Write + Send),
        caller_deadline: std::time::Instant,
    ) -> SimpleClientResult<CompletedSourceTransfer> {
        self.copy_source_file_with_budget(
            session,
            canonical,
            network,
            limits,
            output,
            Some(caller_deadline),
        )
        .await
    }

    async fn copy_source_file_with_budget(
        &self,
        session: SourceFileSession<'_>,
        canonical: &Url,
        network: NetworkPolicy,
        limits: SourceFileLimits,
        output: &mut (dyn Write + Send),
        caller_deadline: Option<std::time::Instant>,
    ) -> SimpleClientResult<CompletedSourceTransfer> {
        if limits.max_wire_bytes == 0 || limits.max_header_bytes == 0 || limits.max_headers == 0 {
            return Err(SimpleClientError::InvalidLimits);
        }
        validate_config(&self.config)?;
        if network == NetworkPolicy::Offline {
            return Err(SimpleClientError::OfflineMissing);
        }
        let configured_deadline = std::time::Instant::now()
            .checked_add(self.config.request_timeout)
            .ok_or(SimpleClientError::InvalidConfiguration(
                "request timeout overflow",
            ))?;
        let deadline =
            caller_deadline.map_or(configured_deadline, |limit| limit.min(configured_deadline));
        http::remaining(deadline)?;
        http::locator(canonical)?;
        let initial_request = session.transport.request_url(canonical)?;
        http::remaining(deadline)?;
        let transfer = http::send_source_request(
            self,
            http::SourceRequestSession {
                credential_ref: session.credential_ref,
                auth_origin: HttpOrigin::new(canonical)?,
                credentials: session.credentials,
                auth_origins: session.auth_origins,
                transport: session.transport,
            },
            http::SourceRequest {
                canonical: canonical.clone(),
                physical: initial_request.clone(),
                accept: "*/*",
                deadline,
                limits: http::SourceHeaderLimits {
                    max_header_bytes: limits.max_header_bytes,
                    max_headers: limits.max_headers,
                },
                conditional: None,
            },
        )
        .await?;
        let http::CheckedSourceResponse {
            document,
            mut response,
            ..
        } = transfer;
        // A partial representation cannot become the complete bytes of a
        // fresh content pin, even if its raw response itself has a valid EOF.
        if response.status().as_u16() != 200 {
            return Err(SimpleClientError::HttpStatus(response.status().as_u16()));
        }
        if response
            .headers()
            .contains_key(reqwest::header::CONTENT_RANGE)
        {
            return Err(SimpleClientError::InvalidLocator);
        }
        http::identity_encoding(response.headers())?;
        let size =
            http::copy_response_body(&mut response, deadline, limits.max_wire_bytes, |chunk| {
                output.write_all(chunk).map_err(|_| SimpleClientError::Io)
            })
            .await?;
        output.flush().map_err(|_| SimpleClientError::Io)?;
        http::remaining(deadline)?;
        Ok(CompletedSourceTransfer {
            initial_canonical: canonical.clone(),
            initial_request,
            canonical: document.canonical().clone(),
            request: document.physical().clone(),
            auth_scope: *session.auth_scope,
            transport_fingerprint: session.transport.fingerprint(),
            size,
        })
    }
}

#[cfg(test)]
#[path = "file/tests.rs"]
mod tests;
