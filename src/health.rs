//! The Autumn health indicator.

use std::collections::HashMap;
use std::sync::Arc;

use autumn_web::actuator::{HealthCheckOutput, HealthIndicator};
use futures_util::future::BoxFuture;

use crate::server::Shared;

/// `UP` while the proxy serves, else `DOWN`. It is in the readiness
/// group, so a draining instance leaves the load balancer. An upstream
/// outage does not make it `DOWN`: the details show it.
pub struct ProxyHealth(pub Arc<Shared>);

impl HealthIndicator for ProxyHealth {
    fn check(&self) -> BoxFuture<'_, HealthCheckOutput> {
        Box::pin(async move {
            let state = self.0.lifecycle.get();
            let mut details = HashMap::new();
            details.insert("state".to_owned(), serde_json::json!(state.as_str()));
            if let Some(addr) = self.0.local_addr.get() {
                details.insert("address".to_owned(), serde_json::json!(addr.to_string()));
            }
            let routes: serde_json::Map<String, serde_json::Value> = self
                .0
                .upstream_health()
                .into_iter()
                .map(|(name, healthy, total)| {
                    (name, serde_json::json!(format!("{healthy}/{total}")))
                })
                .collect();
            if !routes.is_empty() {
                details.insert("routes".to_owned(), serde_json::Value::Object(routes));
            }
            let output = if state.is_ready() {
                HealthCheckOutput::up()
            } else {
                HealthCheckOutput::down()
            };
            output.with_details(details)
        })
    }
}
