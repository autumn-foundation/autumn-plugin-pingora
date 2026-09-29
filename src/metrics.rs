//! `pingora_proxy_*` metrics. Labels are route names from config,
//! `fallback`, `unmatched`, and status classes, so the series count is
//! bounded.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use autumn_web::actuator::{MetricFamily, MetricKind, MetricSample, MetricsSource};

use crate::server::{Shared, UpstreamHealth};

/// Label for requests that go to the Autumn app.
pub const FALLBACK: &str = "fallback";
/// Label for requests that match no route and get 404.
pub const UNMATCHED: &str = "unmatched";

/// Status classes. `error`: no response was sent.
const CLASSES: [&str; 6] = ["1xx", "2xx", "3xx", "4xx", "5xx", "error"];

/// The class label of a status code.
pub const fn status_class(status: u16) -> &'static str {
    match status {
        100..=199 => CLASSES[0],
        200..=299 => CLASSES[1],
        300..=399 => CLASSES[2],
        400..=499 => CLASSES[3],
        500..=599 => CLASSES[4],
        _ => CLASSES[5],
    }
}

/// Counters for one label.
#[derive(Debug, Default)]
struct RouteCounters {
    by_class: [AtomicU64; 6],
    upstream_errors: AtomicU64,
}

/// Request counters. The label set is fixed at boot.
#[derive(Debug, Default)]
pub struct Metrics {
    labels: BTreeMap<String, RouteCounters>,
}

impl Metrics {
    /// Counters for `routes`, `fallback` and `unmatched`.
    pub fn new<'a>(routes: impl IntoIterator<Item = &'a str>) -> Self {
        let labels = routes
            .into_iter()
            .chain([FALLBACK, UNMATCHED])
            .map(|name| (name.to_owned(), RouteCounters::default()))
            .collect();
        Self { labels }
    }

    /// Count one response.
    pub fn record(&self, label: &str, status: u16) {
        if let Some(counters) = self.labels.get(label) {
            let index = CLASSES
                .iter()
                .position(|c| *c == status_class(status))
                .unwrap_or(CLASSES.len() - 1);
            counters.by_class[index].fetch_add(1, Ordering::Relaxed);
        }
    }

    /// Count one failed upstream attempt.
    pub fn upstream_error(&self, label: &str) {
        if let Some(counters) = self.labels.get(label) {
            counters.upstream_errors.fetch_add(1, Ordering::Relaxed);
        }
    }

    fn requests(&self) -> Vec<MetricSample> {
        let mut samples = Vec::new();
        for (label, counters) in &self.labels {
            for (class, counter) in CLASSES.iter().zip(&counters.by_class) {
                let value = counter.load(Ordering::Relaxed);
                if value > 0 {
                    samples.push(sample(
                        vec![("route", label.clone()), ("status", (*class).to_owned())],
                        value,
                    ));
                }
            }
        }
        samples
    }

    fn upstream_errors(&self) -> Vec<MetricSample> {
        self.labels
            .iter()
            .filter_map(|(label, counters)| {
                let value = counters.upstream_errors.load(Ordering::Relaxed);
                (value > 0).then(|| sample(vec![("route", label.clone())], value))
            })
            .collect()
    }
}

#[allow(clippy::cast_precision_loss)] // counters stay far below 2^52
fn sample(labels: Vec<(&str, String)>, value: u64) -> MetricSample {
    MetricSample {
        labels: labels.into_iter().map(|(k, v)| (k.to_owned(), v)).collect(),
        value: value as f64,
    }
}

fn family(name: &str, help: &str, kind: MetricKind, samples: Vec<MetricSample>) -> MetricFamily {
    MetricFamily {
        name: name.to_owned(),
        help: help.to_owned(),
        kind,
        samples,
    }
}

/// All metric families of the proxy.
pub fn families(shared: &Shared) -> Vec<MetricFamily> {
    let up = u64::from(shared.lifecycle.get().is_ready());
    let active = shared.active_connections() as u64;
    let health = shared.upstream_health();
    let per_route = |pick: fn(&UpstreamHealth) -> usize| {
        health
            .iter()
            .map(|entry| sample(vec![("route", entry.route.clone())], pick(entry) as u64))
            .collect()
    };
    let mut out = vec![
        family(
            "pingora_proxy_up",
            "1 while the proxy serves, else 0.",
            MetricKind::Gauge,
            vec![sample(Vec::new(), up)],
        ),
        family(
            "pingora_proxy_connections_active",
            "Open client connections.",
            MetricKind::Gauge,
            vec![sample(Vec::new(), active)],
        ),
        family(
            "pingora_proxy_upstreams_healthy",
            "Healthy upstreams by route.",
            MetricKind::Gauge,
            per_route(|e| e.healthy),
        ),
        family(
            "pingora_proxy_upstreams",
            "Upstreams by route.",
            MetricKind::Gauge,
            per_route(|e| e.total),
        ),
    ];
    if let Some(metrics) = shared.metrics.get() {
        out.push(family(
            "pingora_proxy_requests_total",
            "Responses by route and status class.",
            MetricKind::Counter,
            metrics.requests(),
        ));
        out.push(family(
            "pingora_proxy_upstream_errors_total",
            "Failed upstream attempts by route.",
            MetricKind::Counter,
            metrics.upstream_errors(),
        ));
    }
    out
}

/// The Autumn metrics source.
pub struct ProxyMetrics(pub Arc<Shared>);

impl MetricsSource for ProxyMetrics {
    fn collect(&self) -> Vec<MetricFamily> {
        families(&self.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_classes_cover_every_code() {
        assert_eq!(status_class(101), "1xx");
        assert_eq!(status_class(204), "2xx");
        assert_eq!(status_class(308), "3xx");
        assert_eq!(status_class(404), "4xx");
        assert_eq!(status_class(599), "5xx");
        assert_eq!(status_class(0), "error");
        assert_eq!(status_class(600), "error");
    }

    #[test]
    fn unknown_labels_are_ignored() {
        let metrics = Metrics::new(["a"]);
        metrics.record("zzz", 200);
        metrics.upstream_error("zzz");
        metrics.record("a", 200);
        metrics.record(FALLBACK, 0);
        assert_eq!(metrics.requests().len(), 2);
        assert!(metrics.upstream_errors().is_empty());
    }
}
