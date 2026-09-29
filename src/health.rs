//! The Autumn health indicator.

use std::collections::HashMap;
use std::sync::Arc;

use autumn_web::actuator::{HealthCheckOutput, HealthIndicator};
use futures_util::future::BoxFuture;

use crate::server::Shared;

/// The status is `UP` when the proxy serves, else `DOWN`. The indicator
/// is in the readiness group. Thus a draining instance gets no new
/// traffic. Upstream failures show only in the details.
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
                .map(|h| {
                    (
                        h.route,
                        serde_json::json!(format!("{}/{}", h.healthy, h.total)),
                    )
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
