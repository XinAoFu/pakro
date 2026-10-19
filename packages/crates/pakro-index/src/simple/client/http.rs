//! One explicitly routed source request chain, with a shared deadline.
//!
//! Project cache/representation handling and raw file sinks share the same
//! authentication, redirects, headers and bounded body loop.

use crate::python_source::SimpleRepresentation;
use std::collections::BTreeSet;
use std::time::Instant;

use reqwest::header::{ACCEPT, ACCEPT_ENCODING, CONTENT_ENCODING, CONTENT_TYPE, LOCATION};

use super::*;

pub(super) const ACCEPT_VALUE: &str = "application/vnd.pypi.simple.v1+json, application/vnd.pypi.simple.v1+html;q=0.2, text/html;q=0.01";

/// Only this transport constructs an effective parser document after checking
/// the logical/physical chain. It preserves the session's original registry.
pub(in super::super) struct EffectiveSimpleDocument {
    canonical: Url,
    physical: Url,
}
impl EffectiveSimpleDocument {
    pub(in super::super) fn canonical(&self) -> &Url {
        &self.canonical
    }
    pub(in super::super) fn physical(&self) -> &Url {
        &self.physical
    }
}

pub(super) fn locator(url: &Url) -> SimpleClientResult<()> {
    if !matches!(url.scheme(), "http" | "https")
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.fragment().is_some()
    {
        return Err(SimpleClientError::InvalidLocator);
    }
    Ok(())
}
fn is_downgrade(from: &Url, to: &Url) -> bool {
    from.scheme() == "https" && to.scheme() == "http"
}
fn downgrade(from: &Url, to: &Url) -> SimpleClientResult<()> {
    locator(from)?;
    locator(to)?;
    if from.scheme() == "https"
        && to.scheme() == "http"
        && (from.query().is_some() || to.query().is_some())
    {
        return Err(SimpleClientError::InvalidLocator);
    }
    Ok(())
}
fn raw_location(value: &str) -> SimpleClientResult<()> {
    if value
        .chars()
        .any(|value| value.is_control() || value.is_whitespace())
    {
        return Err(SimpleClientError::InvalidLocator);
    }
    let authority = if let Some(rest) = value.strip_prefix("//") {
        Some(rest)
    } else {
        value.split_once(':').map(|(_, rest)| rest)
    };
    if authority.is_some_and(|rest| {
        rest.trim_start_matches(['/', '\\'])
            .split(['/', '\\', '?', '#'])
            .next()
            .is_some_and(|value| value.contains('@'))
    }) {
        return Err(SimpleClientError::InvalidLocator);
    }
    Ok(())
}
fn redirect(
    previous: &EffectiveSimpleDocument,
    value: &str,
    router: &dyn SourceRequestRouter,
) -> SimpleClientResult<EffectiveSimpleDocument> {
    raw_location(value)?;
    let resolved_physical = previous
        .physical
        .join(value)
        .map_err(|_| SimpleClientError::InvalidLocator)?;
    locator(&resolved_physical)?;
    // A scheme-relative reference also selects an authority. Its logical
    // target cannot be recovered from a physical-only mirror authority.
    let network_path = value
        .as_bytes()
        .get(..2)
        .is_some_and(|prefix| prefix.iter().all(|byte| matches!(byte, b'/' | b'\\')));
    let absolute = Url::parse(value)
        .ok()
        .or_else(|| network_path.then(|| resolved_physical.clone()));
    let ambiguous = absolute.as_ref().is_some_and(|url| {
        url.origin() == previous.physical.origin()
            && previous.physical.origin() != previous.canonical.origin()
    });
    let canonical = if ambiguous {
        router
            .canonical_redirect(&previous.canonical, &previous.physical, value)?
            .ok_or(SimpleClientError::AmbiguousRedirect)?
    } else {
        previous
            .canonical
            .join(value)
            .map_err(|_| SimpleClientError::InvalidLocator)?
    };
    locator(&canonical)?;
    let routed = router.request_url(&canonical)?;
    locator(&routed)?;
    let physical = if absolute.is_some() && !ambiguous {
        // The response names an absolute logical URI. Use the application's
        // explicit forward route, just as for the initial canonical request.
        routed
    } else {
        // Relative paths and physical-only absolute references must preserve
        // the independently resolved physical Location. An explicit reverse
        // binding is accepted only when its forward route proves that match.
        if routed != resolved_physical {
            return Err(SimpleClientError::AmbiguousRedirect);
        }
        resolved_physical
    };
    downgrade(&previous.physical, &physical)?;
    downgrade(&previous.canonical, &canonical)?;
    downgrade(&canonical, &physical)?;
    Ok(EffectiveSimpleDocument {
        canonical,
        physical,
    })
}
pub(super) fn remaining(deadline: Instant) -> SimpleClientResult<Duration> {
    deadline
        .checked_duration_since(Instant::now())
        .filter(|value| !value.is_zero())
        .ok_or(SimpleClientError::Timeout)
}
fn transport_error(error: reqwest::Error) -> SimpleClientError {
    #[cfg(test)]
    {
        // Do not format the error or any source: reqwest/HTTP errors can
        // retain request URLs, header values or peer-controlled text.
        eprintln!(
            "Simple transport flags: timeout={} connect={} body={} decode={} redirect={} request={} builder={} status={:?}",
            error.is_timeout(),
            error.is_connect(),
            error.is_body(),
            error.is_decode(),
            error.is_redirect(),
            error.is_request(),
            error.is_builder(),
            error.status().map(|value| value.as_u16()),
        );
        let mut source = std::error::Error::source(&error);
        for depth in 0..8 {
            let Some(current) = source else {
                break;
            };
            if let Some(io) = current.downcast_ref::<std::io::Error>() {
                eprintln!(
                    "Simple transport io depth={depth} kind={:?} errno={:?}",
                    io.kind(),
                    io.raw_os_error()
                );
            }
            source = current.source();
        }
    }
    if error.is_timeout() {
        SimpleClientError::Timeout
    } else {
        SimpleClientError::Transport
    }
}
fn headers(
    headers: &reqwest::header::HeaderMap,
    max_headers: usize,
    max_header_bytes: usize,
) -> SimpleClientResult<()> {
    if headers.len() > max_headers {
        return Err(SimpleClientError::LimitExceeded(
            SimpleClientLimitKind::HeaderCount,
        ));
    }
    let mut bytes = 0usize;
    for (name, value) in headers {
        bytes = bytes
            .checked_add(name.as_str().len())
            .and_then(|used| used.checked_add(value.as_bytes().len()))
            .ok_or(SimpleClientError::LimitExceeded(
                SimpleClientLimitKind::HeaderBytes,
            ))?;
        if bytes > max_header_bytes {
            return Err(SimpleClientError::LimitExceeded(
                SimpleClientLimitKind::HeaderBytes,
            ));
        }
    }
    Ok(())
}

pub(super) struct SourceRequestSession<'a> {
    pub(super) credential_ref: Option<&'a CredentialRef>,
    pub(super) auth_origin: HttpOrigin,
    pub(super) credentials: &'a dyn SourceCredentials,
    pub(super) auth_origins: &'a AuthOriginGrants,
    pub(super) transport: &'a dyn SourceRequestRouter,
}
pub(super) struct SourceHeaderLimits {
    pub(super) max_header_bytes: usize,
    pub(super) max_headers: usize,
}
pub(super) struct ConditionalSourceRequest<'a> {
    pub(super) canonical: &'a Url,
    pub(super) physical: &'a Url,
    pub(super) validator: &'a HeaderValue,
}
pub(super) struct CheckedSourceResponse {
    pub(super) document: EffectiveSimpleDocument,
    pub(super) response: reqwest::Response,
    pub(super) conditional_validator: Option<HeaderValue>,
}
pub(super) struct SourceRequest<'a> {
    pub(super) canonical: Url,
    pub(super) physical: Url,
    pub(super) accept: &'static str,
    pub(super) deadline: Instant,
    pub(super) limits: SourceHeaderLimits,
    pub(super) conditional: Option<ConditionalSourceRequest<'a>>,
}

pub(super) async fn send_source_request(
    client: &SimpleIndexClient,
    session: SourceRequestSession<'_>,
    request: SourceRequest<'_>,
) -> SimpleClientResult<CheckedSourceResponse> {
    let SourceRequest {
        canonical,
        physical,
        accept,
        deadline,
        limits,
        conditional,
    } = request;
    downgrade(&canonical, &physical)?;
    let mut document = EffectiveSimpleDocument {
        canonical,
        physical,
    };
    let mut previous_authorized = false;
    let mut header_downgrade = is_downgrade(&document.canonical, &document.physical);
    let mut seen = BTreeSet::new();
    let mut redirects = 0usize;
    loop {
        remaining(deadline)?;
        if !seen.insert((
            document.canonical.as_str().to_owned(),
            document.physical.as_str().to_owned(),
        )) {
            return Err(SimpleClientError::InvalidLocator);
        }
        if header_downgrade && previous_authorized {
            return Err(SimpleClientError::InvalidLocator);
        }
        let authorization = if let Some(reference) = session.credential_ref {
            let origin = HttpOrigin::new(&document.physical)?;
            if origin != session.auth_origin && !session.auth_origins.additional.contains(&origin) {
                return Err(SimpleClientError::Authentication);
            }
            remaining(deadline)?;
            let authorization = session.credentials.authorization_for(
                reference,
                &origin,
                NetworkPolicy::Allowed,
            )?;
            if let Some(authorization) = &authorization {
                if authorization.origin != origin || authorization.value.as_bytes().is_empty() {
                    return Err(SimpleClientError::Authentication);
                }
                if authorization.value.as_bytes().len() > limits.max_header_bytes {
                    return Err(SimpleClientError::LimitExceeded(
                        SimpleClientLimitKind::HeaderBytes,
                    ));
                }
            }
            authorization
        } else {
            None
        };
        if header_downgrade && authorization.is_some() {
            return Err(SimpleClientError::InvalidLocator);
        }
        remaining(deadline)?;
        let mut outgoing = client
            .client
            .get(document.physical.clone())
            .header(ACCEPT, accept)
            .header(ACCEPT_ENCODING, "identity")
            .timeout(remaining(deadline)?);
        let authorized = authorization.is_some();
        if let Some(authorization) = authorization {
            outgoing = outgoing.header(reqwest::header::AUTHORIZATION, authorization.value);
        }
        let validator = conditional.as_ref().and_then(|condition| {
            (condition.canonical == &document.canonical && condition.physical == &document.physical)
                .then(|| condition.validator.clone())
        });
        if let Some(validator) = &validator {
            outgoing = outgoing.header(reqwest::header::IF_NONE_MATCH, validator.clone());
        }
        let response = outgoing.send().await.map_err(transport_error)?;
        headers(
            response.headers(),
            limits.max_headers,
            limits.max_header_bytes,
        )?;
        if matches!(response.status().as_u16(), 301 | 302 | 303 | 307 | 308) {
            if redirects >= client.config.max_redirects {
                return Err(SimpleClientError::LimitExceeded(
                    SimpleClientLimitKind::Redirects,
                ));
            }
            let mut locations = response.headers().get_all(LOCATION).iter();
            let value = locations.next().ok_or(SimpleClientError::InvalidLocator)?;
            if locations.next().is_some() {
                return Err(SimpleClientError::InvalidLocator);
            }
            let next = redirect(
                &document,
                value
                    .to_str()
                    .map_err(|_| SimpleClientError::InvalidLocator)?,
                session.transport,
            )?;
            header_downgrade = is_downgrade(&document.canonical, &next.canonical)
                || is_downgrade(&document.physical, &next.physical)
                || is_downgrade(&next.canonical, &next.physical);
            previous_authorized = authorized;
            document = next;
            redirects += 1;
            continue;
        }
        remaining(deadline)?;
        return Ok(CheckedSourceResponse {
            document,
            response,
            conditional_validator: validator,
        });
    }
}
pub(super) fn identity_encoding(headers: &reqwest::header::HeaderMap) -> SimpleClientResult<()> {
    for value in headers.get_all(CONTENT_ENCODING) {
        if !value.as_bytes().eq_ignore_ascii_case(b"identity") {
            return Err(SimpleClientError::UnsupportedContentEncoding);
        }
    }
    Ok(())
}
pub(super) async fn copy_response_body(
    response: &mut reqwest::Response,
    deadline: Instant,
    max_wire_bytes: u64,
    mut consume: impl FnMut(&[u8]) -> SimpleClientResult<()>,
) -> SimpleClientResult<u64> {
    if response
        .content_length()
        .is_some_and(|value| value > max_wire_bytes)
    {
        return Err(SimpleClientError::LimitExceeded(
            SimpleClientLimitKind::WireBytes,
        ));
    }
    let mut size = 0u64;
    while let Some(chunk) = response.chunk().await.map_err(transport_error)? {
        remaining(deadline)?;
        let next = size
            .checked_add(chunk.len() as u64)
            .filter(|size| *size <= max_wire_bytes)
            .ok_or(SimpleClientError::LimitExceeded(
                SimpleClientLimitKind::WireBytes,
            ))?;
        consume(&chunk)?;
        size = next;
    }
    remaining(deadline)?;
    Ok(size)
}

pub(super) async fn project(
    client: &SimpleIndexClient,
    session: PythonSourceSession<'_>,
    project: &NormalizedPythonName,
    policy: SimpleFetchPolicy,
    limits: SimpleClientLimits,
    caller_deadline: Option<Instant>,
) -> SimpleClientResult<ProjectLookup> {
    validate_limits(limits)?;
    validate_config(&client.config)?;
    // The legacy entry starts its configured network timeout at its original
    // position below. The caller-budget entry also covers synchronous routing
    // and bounded local cache/codec work before that network phase.
    let caller_deadline = caller_deadline
        .map(|caller| {
            Instant::now()
                .checked_add(client.config.request_timeout)
                .map(|configured| caller.min(configured))
                .ok_or(SimpleClientError::InvalidConfiguration(
                    "request timeout overflow",
                ))
        })
        .transpose()?;
    if let Some(deadline) = caller_deadline {
        remaining(deadline)?;
    }
    let original = session
        .registry
        .project_url(project)
        .map_err(|_| SimpleClientError::InvalidLocator)?;
    let canonical = original.as_url().clone();
    let physical = session.transport.request_url(&canonical)?;
    if let Some(deadline) = caller_deadline {
        remaining(deadline)?;
    }
    downgrade(&canonical, &physical)?;
    let request = cache::CacheRequest {
        registry: session.registry,
        auth_scope: session.auth_scope,
        transport: session.transport.fingerprint(),
        project,
        canonical: &canonical,
        physical: &physical,
    };
    // A stale or explicitly refreshed entry is still validated before any
    // provider or network operation. Corruption is never a cache miss.
    let cached = if let Some(entry) = cache::read(&client.cache_root, &request, limits)? {
        let document = cached_document(&entry, session.transport)?;
        let context = super::super::SimpleParseContext {
            registry: session.registry,
            project,
            canonical_document_url: &original,
            limits: limits.parser,
        };
        let facts =
            decode::decode_response(&entry.body, &entry.content_type, context, limits, &document)?;
        if let Some(deadline) = caller_deadline {
            remaining(deadline)?;
        }
        if policy.network == NetworkPolicy::Offline || cache::is_fresh(&entry, policy) {
            return Ok(ProjectLookup::Present(facts));
        }
        Some((entry, facts))
    } else {
        None
    };
    if policy.network == NetworkPolicy::Offline {
        return Err(SimpleClientError::OfflineMissing);
    }
    let configured_deadline = Instant::now()
        .checked_add(client.config.request_timeout)
        .ok_or(SimpleClientError::InvalidConfiguration(
            "request timeout overflow",
        ))?;
    let deadline = caller_deadline.unwrap_or(configured_deadline);
    let checked = send_source_request(
        client,
        SourceRequestSession {
            credential_ref: session.registry.credential_ref(),
            auth_origin: HttpOrigin::new(session.registry.canonical_base().as_url())?,
            credentials: session.credentials,
            auth_origins: session.auth_origins,
            transport: session.transport,
        },
        SourceRequest {
            canonical: canonical.clone(),
            physical: physical.clone(),
            accept: ACCEPT_VALUE,
            deadline,
            limits: SourceHeaderLimits {
                max_header_bytes: limits.max_header_bytes,
                max_headers: limits.max_headers,
            },
            conditional: cached.as_ref().and_then(|(entry, _)| {
                entry
                    .etag
                    .as_ref()
                    .map(|validator| ConditionalSourceRequest {
                        canonical: &entry.canonical,
                        physical: &entry.physical,
                        validator,
                    })
            }),
        },
    )
    .await?;
    let CheckedSourceResponse {
        document,
        mut response,
        conditional_validator: validator,
    } = checked;
    let status = response.status();
    if status.as_u16() == 404 {
        return Ok(ProjectLookup::Missing404);
    }
    if status.as_u16() == 304 {
        let validator = validator.as_ref().ok_or(SimpleClientError::InvalidCache(
            "response has no matching conditional cached body",
        ))?;
        let updated_etag = revalidated_etag(validator, response.headers())?;
        let Some((mut entry, facts)) = cached else {
            return Err(SimpleClientError::InvalidCache(
                "response has no complete cached body",
            ));
        };
        let mut content_types = response.headers().get_all(CONTENT_TYPE).iter();
        if let Some(value) = content_types.next() {
            if content_types.next().is_some() {
                return Err(SimpleClientError::InvalidCache(
                    "conditional response has duplicate media types",
                ));
            }
            let content_type = value.to_str().map_err(|_| {
                SimpleClientError::InvalidCache("conditional response media type is invalid")
            })?;
            let old_media = decode::validate_media_type(&entry.content_type)?;
            let new_media = decode::validate_media_type(content_type).map_err(|_| {
                SimpleClientError::InvalidCache("conditional response media type is invalid")
            })?;
            if old_media.essence_str() != new_media.essence_str() {
                return Err(SimpleClientError::InvalidCache(
                    "conditional response changes its cached representation",
                ));
            }
            // Charset parameters can also change the interpretation of
            // an unchanged HTML body. Reuse the same bounded codec/parser
            // rather than guessing equivalence of charset aliases/BOMs.
            let context = super::super::SimpleParseContext {
                registry: session.registry,
                project,
                canonical_document_url: &original,
                limits: limits.parser,
            };
            let updated =
                decode::decode_response(&entry.body, content_type, context, limits, &document)
                    .map_err(|_| {
                        SimpleClientError::InvalidCache(
                            "conditional response cannot describe its cached body",
                        )
                    })?;
            if updated != facts {
                return Err(SimpleClientError::InvalidCache(
                    "conditional response changes its cached interpretation",
                ));
            }
            entry.content_type = content_type.to_owned();
        }
        entry.stored_at = policy.now;
        entry.max_age = cache::max_age(response.headers()).or(entry.max_age);
        if let Some(value) = updated_etag {
            entry.etag = Some(value);
        }
        remaining(deadline)?;
        cache::publish(
            &client.cache_root,
            &request,
            &entry,
            response.headers(),
            limits,
        )?;
        if caller_deadline.is_some() {
            remaining(deadline)?;
        }
        return Ok(ProjectLookup::Present(facts));
    }
    if !status.is_success() {
        return Err(SimpleClientError::HttpStatus(status.as_u16()));
    }
    identity_encoding(response.headers())?;
    let mut content_types = response.headers().get_all(CONTENT_TYPE).iter();
    let content_type = content_types
        .next()
        .ok_or(SimpleClientError::UnsupportedMediaType)?
        .to_str()
        .map_err(|_| SimpleClientError::UnsupportedMediaType)?
        .to_owned();
    if content_types.next().is_some() {
        return Err(SimpleClientError::UnsupportedMediaType);
    }
    // Validate MIME before retaining a possibly large body.
    let representation = match decode::validate_media_type(&content_type)?.essence_str() {
        "application/vnd.pypi.simple.v1+json" => SimpleRepresentation::JsonV1,
        "application/vnd.pypi.simple.v1+html" => SimpleRepresentation::HtmlV1,
        _ => SimpleRepresentation::LegacyHtml,
    };
    let mut body = Vec::new();
    copy_response_body(&mut response, deadline, limits.max_wire_bytes, |chunk| {
        body.try_reserve(chunk.len())
            .map_err(|_| SimpleClientError::LimitExceeded(SimpleClientLimitKind::WireBytes))?;
        body.extend_from_slice(chunk);
        Ok(())
    })
    .await?;
    let context = super::super::SimpleParseContext {
        registry: session.registry,
        project,
        canonical_document_url: &original,
        limits: limits.parser,
    };
    let project = decode::decode_response(&body, &content_type, context, limits, &document)?;
    remaining(deadline)?;
    let entry = cache::CachedResponse {
        representation,
        content_type,
        canonical: document.canonical,
        physical: document.physical,
        stored_at: policy.now,
        max_age: cache::max_age(response.headers()),
        etag: response
            .headers()
            .get(reqwest::header::ETAG)
            .cloned()
            .map(|mut value| {
                value.set_sensitive(true);
                value
            }),
        body,
    };
    cache::publish(
        &client.cache_root,
        &request,
        &entry,
        response.headers(),
        limits,
    )?;
    if caller_deadline.is_some() {
        remaining(deadline)?;
    }
    Ok(ProjectLookup::Present(project))
}

pub(super) fn validate_limits(limits: SimpleClientLimits) -> SimpleClientResult<()> {
    if limits.max_wire_bytes == 0
        || limits.max_cache_bytes == 0
        || limits.max_header_bytes == 0
        || limits.max_headers == 0
        || limits.max_decoded_bytes == 0
    {
        return Err(SimpleClientError::InvalidLimits);
    }
    super::super::facts::validate_limits(limits.parser)
        .map_err(|_| SimpleClientError::InvalidLimits)?;
    if [
        limits.html.max_tokens,
        limits.html.max_tree_nodes,
        limits.html.max_tree_depth,
        limits.html.max_tree_text_bytes,
        limits.html.max_tree_operations,
        limits.html.max_attributes_per_tag,
        limits.html.max_anchors,
        limits.html.feed_chunk_bytes,
    ]
    .contains(&0)
    {
        return Err(SimpleClientError::InvalidLimits);
    }
    Ok(())
}

fn cached_document(
    entry: &cache::CachedResponse,
    router: &dyn SourceRequestRouter,
) -> SimpleClientResult<EffectiveSimpleDocument> {
    // The envelope codec already rejects query-bearing cached documents and
    // raw user information. Recheck the active router before granting parser
    // base authority to the stored effective canonical document.
    downgrade(&entry.canonical, &entry.physical)
        .map_err(|_| SimpleClientError::InvalidCache("stored document route is invalid"))?;
    let physical = router.request_url(&entry.canonical)?;
    if physical != entry.physical {
        return Err(SimpleClientError::InvalidCache(
            "stored document differs from its active route",
        ));
    }
    Ok(EffectiveSimpleDocument {
        canonical: entry.canonical.clone(),
        physical: entry.physical.clone(),
    })
}

/// RFC 9110 entity-tag grammar over bytes: an opaque tag may contain obs-text,
/// so HeaderValue::to_str() would incorrectly exclude legal validators.
fn entity_tag(value: &HeaderValue) -> SimpleClientResult<(bool, &[u8])> {
    let value = value.as_bytes();
    let start = value
        .iter()
        .position(|byte| !matches!(byte, b' ' | b'\t'))
        .unwrap_or(value.len());
    let end = value
        .iter()
        .rposition(|byte| !matches!(byte, b' ' | b'\t'))
        .map_or(start, |index| index + 1);
    let value = &value[start..end];
    let (weak, opaque) = value
        .strip_prefix(b"W/")
        .map_or((false, value), |value| (true, value));
    let opaque = opaque
        .strip_prefix(b"\"")
        .and_then(|value| value.strip_suffix(b"\""))
        .ok_or(SimpleClientError::InvalidCache(
            "conditional validator is malformed",
        ))?;
    if !opaque
        .iter()
        .all(|byte| *byte == 0x21 || (0x23..=0x7e).contains(byte) || *byte >= 0x80)
    {
        return Err(SimpleClientError::InvalidCache(
            "conditional validator is malformed",
        ));
    }
    Ok((weak, opaque))
}

fn revalidated_etag(
    stored: &HeaderValue,
    headers: &reqwest::header::HeaderMap,
) -> SimpleClientResult<Option<HeaderValue>> {
    let mut tags = headers.get_all(reqwest::header::ETAG).iter();
    let Some(value) = tags.next() else {
        // Preserve the existing single conditional-response compatibility:
        // omission keeps the exact validator sent, never inventing a new tag.
        return Ok(None);
    };
    if tags.next().is_some() {
        return Err(SimpleClientError::InvalidCache(
            "conditional response has duplicate validators",
        ));
    }
    let (old_weak, old_opaque) = entity_tag(stored)?;
    let (new_weak, new_opaque) = entity_tag(value)?;
    // RFC 9111 §4.3.4: a new strong validator selects the same stored strong
    // validator. A weak response can select the one weakly matching record.
    if old_opaque != new_opaque || (!new_weak && old_weak) {
        return Err(SimpleClientError::InvalidCache(
            "conditional validator differs from its cached body",
        ));
    }
    let mut value = value.clone();
    value.set_sensitive(true);
    Ok(Some(value))
}
