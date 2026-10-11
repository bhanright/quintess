//! RFC 3161 trusted timestamps: asking a timestamp authority to vouch that
//! some data existed at a given time, and reading its answer.
//!
//! A timestamp request carries only a hash, so the document never leaves
//! the computer. The answer ("token") is a CMS SignedData signed by the
//! authority; checking its signature and trust is in [`crate::validation`].

use anyhow::{Context, Result, anyhow, bail};

use crate::cms::{self, OID_SHA256};
use crate::der;
use crate::net::HttpTransport;

const OID_TST_INFO: &str = "1.2.840.113549.1.9.16.1.4";

/// What a timestamp token says (its TSTInfo).
#[derive(Clone, Debug, PartialEq)]
pub struct TimestampInfo {
    /// The moment the authority vouches for.
    pub time: chrono::DateTime<chrono::Utc>,
    /// The authority's policy identifier.
    pub policy: String,
    /// The hash algorithm of the stamped data.
    pub hash_algorithm: String,
    /// The hash of the stamped data.
    pub imprint: Vec<u8>,
    /// The request's random number, echoed back.
    pub nonce: Option<Vec<u8>>,
}

/// A TimeStampReq asking for a token over a SHA-256 hash. `nonce` lets the
/// caller recognize that the answer belongs to this request.
pub fn build_request(sha256_digest: &[u8], nonce: u64) -> Vec<u8> {
    let imprint = der::sequence(&[der::sequence(&[der::oid(OID_SHA256), der::null()]), der::octet_string(sha256_digest)]);
    der::sequence(&[
        der::integer(1),
        imprint,
        // Kept positive so the integer encodes without a sign byte surprise.
        der::integer((nonce & (i64::MAX as u64)) as i64),
        // Ask the authority to include its certificates, which verification needs.
        der::boolean(true),
    ])
}

/// Reads a TimeStampResp. Returns the token (a ContentInfo) after checking
/// that the authority granted the request and the token stamps `sha256_digest`
/// with this request's `nonce`.
pub fn parse_response(response: &[u8], sha256_digest: &[u8], nonce: u64) -> Result<Vec<u8>> {
    let (outer, _) = der::read(response).context("the timestamp server's answer is not a timestamp response")?;
    let parts = der::children(outer.content)?;
    let status_info = der::children(parts.first().ok_or_else(|| anyhow!("empty timestamp response"))?.content)?;
    let status = status_info.first().map(|s| s.content.iter().fold(0u32, |acc, b| (acc << 8) | *b as u32)).ok_or_else(|| anyhow!("timestamp response has no status"))?;
    // 0 granted, 1 granted with modifications; everything else is a refusal.
    if status > 1 {
        let message: Vec<String> = status_info.get(1).and_then(|texts| der::children(texts.content).ok()).unwrap_or_default().iter().map(|t| String::from_utf8_lossy(t.content).into_owned()).collect();
        bail!("the timestamp server refused the request (status {status}{})", if message.is_empty() { String::new() } else { format!(": {}", message.join("; ")) });
    }
    let token = parts.get(1).ok_or_else(|| anyhow!("the timestamp server granted the request but sent no token"))?.encoded;
    let info = token_info(token)?;
    if info.hash_algorithm != OID_SHA256 || info.imprint != sha256_digest {
        bail!("the timestamp token is for different data than was sent");
    }
    let expected_nonce = der::integer((nonce & (i64::MAX as u64)) as i64);
    // The nonce is optional in a response, but when present it must be ours.
    if let Some(echoed) = &info.nonce {
        if der::tlv(der::INTEGER, echoed) != expected_nonce {
            bail!("the timestamp token answers a different request (nonce mismatch)");
        }
    }
    Ok(token.to_vec())
}

/// Asks the server at `url` to timestamp a SHA-256 hash and returns the token.
pub fn request_token(transport: &dyn HttpTransport, url: &str, sha256_digest: &[u8]) -> Result<Vec<u8>> {
    let mut random = [0u8; 8];
    getrandom::fill(&mut random).map_err(|error| anyhow!("no random source: {error}"))?;
    let nonce = u64::from_be_bytes(random);
    let request = build_request(sha256_digest, nonce);
    let response = transport.post(url, "application/timestamp-query", &request).with_context(|| format!("the timestamp server at {url} did not answer"))?;
    parse_response(&response, sha256_digest, nonce)
}

/// The TSTInfo inside a timestamp token.
pub fn token_info(token: &[u8]) -> Result<TimestampInfo> {
    let signed = cms::parse(token).context("the timestamp token is not readable")?;
    if signed.content_type != OID_TST_INFO {
        bail!("the token does not hold timestamp information");
    }
    parse_tst_info(signed.content.ok_or_else(|| anyhow!("the timestamp token has no content"))?)
}

/// Reads a TSTInfo structure.
pub fn parse_tst_info(tst_info: &[u8]) -> Result<TimestampInfo> {
    let (outer, _) = der::read(tst_info)?;
    let parts = der::children(outer.content)?;
    // version, policy, messageImprint, serialNumber, genTime, [accuracy], [ordering], [nonce], ...
    let policy = der::oid_to_string(parts.get(1).ok_or_else(|| anyhow!("timestamp information is incomplete"))?.content);
    let imprint_parts = der::children(parts.get(2).ok_or_else(|| anyhow!("timestamp information has no imprint"))?.content)?;
    let hash_algorithm = der::oid_to_string(der::children(imprint_parts.first().ok_or_else(|| anyhow!("no imprint algorithm"))?.content)?.first().ok_or_else(|| anyhow!("no imprint algorithm"))?.content);
    let imprint = imprint_parts.get(1).ok_or_else(|| anyhow!("no imprint hash"))?.content.to_vec();
    let time = der::parse_time(parts.get(4).ok_or_else(|| anyhow!("timestamp information has no time"))?).ok_or_else(|| anyhow!("the timestamp's time is unreadable"))?;
    let nonce = parts.iter().skip(5).find(|e| e.tag == der::INTEGER).map(|e| e.content.to_vec());
    Ok(TimestampInfo { time, policy, hash_algorithm, imprint, nonce })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn requests_ask_for_a_sha256_token_with_certificates() {
        let request = build_request(&[7u8; 32], 42);
        let (outer, _) = der::read(&request).unwrap();
        let parts = der::children(outer.content).unwrap();
        assert_eq!(parts.len(), 4);
        assert_eq!(parts[0].content, [1]);
        let imprint = der::children(parts[1].content).unwrap();
        assert_eq!(imprint[1].content, [7u8; 32]);
        assert_eq!(parts[2].content, [42]);
        assert_eq!(parts[3].content, [0xFF]);
    }

    #[test]
    fn refusals_are_reported_with_the_servers_reason() {
        let refusal = der::sequence(&[der::sequence(&[der::integer(2), der::sequence(&[der::tlv(0x0C, b"unknown policy")])])]);
        let message = parse_response(&refusal, &[0u8; 32], 1).unwrap_err().to_string();
        assert!(message.contains("status 2") && message.contains("unknown policy"), "{message}");
    }

    #[test]
    fn answers_that_are_not_responses_are_rejected() {
        assert!(parse_response(b"<html>502 Bad Gateway</html>", &[0u8; 32], 1).is_err());
    }
}
