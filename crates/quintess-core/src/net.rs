//! The two kinds of web request signing needs (timestamp servers, OCSP
//! responders, CRL downloads), behind a trait so tests can answer them
//! without a network.

use std::time::Duration;

use anyhow::{Context, Result};

pub trait HttpTransport {
    fn get(&self, url: &str) -> Result<Vec<u8>>;
    fn post(&self, url: &str, content_type: &str, body: &[u8]) -> Result<Vec<u8>>;
}

/// Real requests over the network.
pub struct NetworkTransport {
    agent: ureq::Agent,
}

/// Responses larger than this are refused (a CRL can run to a few megabytes).
const LARGEST_RESPONSE_BYTES: u64 = 32 * 1024 * 1024;

impl NetworkTransport {
    pub fn new(timeout: Duration) -> Self {
        let config = ureq::Agent::config_builder().timeout_global(Some(timeout)).build();
        Self { agent: ureq::Agent::new_with_config(config) }
    }
}

impl Default for NetworkTransport {
    fn default() -> Self {
        Self::new(Duration::from_secs(20))
    }
}

fn user_agent() -> String {
    format!("Quintess/{}", crate::updates::CURRENT_VERSION)
}

impl HttpTransport for NetworkTransport {
    fn get(&self, url: &str) -> Result<Vec<u8>> {
        let mut response = self.agent.get(url).header("User-Agent", &user_agent()).call().with_context(|| format!("could not reach {url}"))?;
        Ok(response.body_mut().with_config().limit(LARGEST_RESPONSE_BYTES).read_to_vec()?)
    }

    fn post(&self, url: &str, content_type: &str, body: &[u8]) -> Result<Vec<u8>> {
        let mut response = self.agent.post(url).header("User-Agent", &user_agent()).header("Content-Type", content_type).send(body).with_context(|| format!("could not reach {url}"))?;
        Ok(response.body_mut().with_config().limit(LARGEST_RESPONSE_BYTES).read_to_vec()?)
    }
}
