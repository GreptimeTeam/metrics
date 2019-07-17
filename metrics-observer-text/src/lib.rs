//! Observes metrics in a hierarchical, text-based format.
//!
//! Metric scopes are used to provide the hierarchy and indentation of metrics.  As an example, for
//! a snapshot with two metrics — `server.msgs_received` and `server.msgs_sent` — we would
//! expect to see this output:
//!
//! ```c
//! root:
//!   server:
//!     msgs_received: 42
//!     msgs_sent: 13
//! ```
//!
//! If we added another metric — `configuration_reloads` — we would expect to see:
//!
//! ```c
//! root:
//!   configuration_reloads: 2
//!   server:
//!     msgs_received: 42
//!     msgs_sent: 13
//! ```
//!
//! Metrics are sorted alphabetically.
//!
//! ## Histograms
//!
//! Histograms are rendered with a configurable set of quantiles that are provided when creating an
//! instance of `TextObserver`.  They are formatted using human-readable labels when displayed to
//! the user.  For example, 0.0 is rendered as "min", 1.0 as "max", and anything in between using
//! the common "pXXX" format i.e. a quantile of 0.5 or percentile of 50 would be p50, a quantile of
//! 0.999 or percentile of 99.9 would be p999, and so on.
//!
//! All histograms have the sample count of the histogram provided in the output.
//!
//! ```c
//! root:
//!   connect_time count: 15
//!   connect_time min: 1334
//!   connect_time p50: 1934
//!   connect_time p99: 5330
//!   connect_time max: 139389
//! ```
//!
#![deny(missing_docs)]
use hdrhistogram::Histogram;
use metrics_core::{Builder, Drain, Key, Label, Observer};
use metrics_util::{parse_quantiles, Quantile};
use std::{
    collections::{HashMap, VecDeque},
    fmt::Display,
};

/// Builder for [`TextRecorder`].
pub struct TextBuilder {
    quantiles: Vec<Quantile>,
}

impl TextBuilder {
    /// Creates a new [`TextBuilder`] with a default set of quantiles.
    ///
    /// Configures the observer with these default quantiles: 0.0, 0.5, 0.9, 0.95, 0.99, 0.999, and
    /// 1.0.  If you want to customize the quantiles used, you can call
    /// [`TextBuilder::with_quantiles`].
    ///
    /// The configured quantiles are used when rendering any histograms.
    pub fn new() -> Self {
        Self::default()
    }

    /// Creates a new [`TextBuilder`] with the given set of quantiles.
    ///
    /// The configured quantiles are used when rendering any histograms.
    pub fn with_quantiles(quantiles: &[f64]) -> Self {
        let actual_quantiles = parse_quantiles(quantiles);

        Self {
            quantiles: actual_quantiles,
        }
    }
}

impl Builder for TextBuilder {
    type Output = TextObserver;

    fn build(&self) -> Self::Output {
        TextObserver {
            quantiles: self.quantiles.clone(),
            structure: MetricsTree::with_level(0),
            histos: HashMap::new(),
        }
    }
}

impl Default for TextBuilder {
    fn default() -> Self {
        Self::with_quantiles(&[0.0, 0.5, 0.9, 0.95, 0.99, 0.999, 1.0])
    }
}

/// Records metrics in a hierarchical, text-based format.
pub struct TextObserver {
    pub(crate) quantiles: Vec<Quantile>,
    pub(crate) structure: MetricsTree,
    pub(crate) histos: HashMap<Key, Histogram<u64>>,
}

impl Observer for TextObserver {
    fn observe_counter(&mut self, key: Key, value: u64) {
        let (name_parts, name) = key_to_parts(key);
        let mut values = single_value_to_values(name, value);
        self.structure.insert(name_parts, &mut values);
    }

    fn observe_gauge(&mut self, key: Key, value: i64) {
        let (name_parts, name) = key_to_parts(key);
        let mut values = single_value_to_values(name, value);
        self.structure.insert(name_parts, &mut values);
    }

    fn observe_histogram(&mut self, key: Key, values: &[u64]) {
        let entry = self
            .histos
            .entry(key)
            .or_insert_with(|| Histogram::<u64>::new(3).expect("failed to create histogram"));

        for value in values {
            entry
                .record(*value)
                .expect("failed to observe histogram value");
        }
    }
}

struct MetricsTree {
    level: usize,
    current: Vec<String>,
    next: HashMap<String, MetricsTree>,
}

impl MetricsTree {
    pub fn with_level(level: usize) -> Self {
        MetricsTree {
            level,
            current: Vec::new(),
            next: HashMap::new(),
        }
    }

    pub fn insert(&mut self, mut name_parts: VecDeque<String>, values: &mut Vec<String>) {
        match name_parts.len() {
            0 => {
                let indent = "  ".repeat(self.level);
                let mut indented = values
                    .iter()
                    .map(move |x| format!("{}{}", indent, x))
                    .collect::<Vec<_>>();
                self.current.append(&mut indented);
            }
            _ => {
                let name = name_parts
                    .pop_front()
                    .expect("failed to get next name component");
                let current_level = self.level;
                let inner = self
                    .next
                    .entry(name)
                    .or_insert_with(move || MetricsTree::with_level(current_level + 1));
                inner.insert(name_parts, values);
            }
        }
    }

    pub fn render(&mut self) -> String {
        let indent = "  ".repeat(self.level);
        let mut output = String::new();

        let mut sorted = self
            .current
            .drain(..)
            .map(SortEntry::Inline)
            .chain(self.next.drain().map(|(k, v)| SortEntry::Nested(k, v)))
            .collect::<Vec<_>>();
        sorted.sort();

        for entry in sorted {
            match entry {
                SortEntry::Inline(s) => {
                    output.push_str(s.as_str());
                    output.push_str("\n");
                }
                SortEntry::Nested(s, mut inner) => {
                    output.push_str(indent.as_str());
                    output.push_str(s.as_str());
                    output.push_str(":\n");

                    let layer_output = inner.render();
                    output.push_str(layer_output.as_str());
                }
            }
        }

        output
    }
}

impl Drain<String> for TextObserver {
    fn drain(&mut self) -> String {
        for (key, h) in self.histos.drain() {
            let (name_parts, name) = key_to_parts(key);
            let mut values = hist_to_values(name, h.clone(), &self.quantiles);
            self.structure.insert(name_parts, &mut values);
        }
        self.structure.render()
    }
}

enum SortEntry {
    Inline(String),
    Nested(String, MetricsTree),
}

impl SortEntry {
    fn name(&self) -> &str {
        match self {
            SortEntry::Inline(s) => s,
            SortEntry::Nested(s, _) => s,
        }
    }
}

impl PartialEq for SortEntry {
    fn eq(&self, other: &SortEntry) -> bool {
        self.name() == other.name()
    }
}

impl Eq for SortEntry {}

impl std::cmp::PartialOrd for SortEntry {
    fn partial_cmp(&self, other: &SortEntry) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl std::cmp::Ord for SortEntry {
    fn cmp(&self, other: &SortEntry) -> std::cmp::Ordering {
        self.name().cmp(other.name())
    }
}

fn key_to_parts(key: Key) -> (VecDeque<String>, String) {
    let (name, labels) = key.into_parts();
    let mut parts = name
        .split('.')
        .map(ToOwned::to_owned)
        .collect::<VecDeque<_>>();
    let name = parts.pop_back().expect("name didn't have a single part");

    let labels = labels
        .into_iter()
        .map(Label::into_parts)
        .map(|(k, v)| format!("{}=\"{}\"", k, v))
        .collect::<Vec<_>>()
        .join(",");
    let label = if labels.is_empty() {
        String::new()
    } else {
        format!("{{{}}}", labels)
    };

    let fname = format!("{}{}", name, label);

    (parts, fname)
}

fn single_value_to_values<T>(name: String, value: T) -> Vec<String>
where
    T: Display,
{
    let fvalue = format!("{}: {}", name, value);
    vec![fvalue]
}

fn hist_to_values(name: String, hist: Histogram<u64>, quantiles: &[Quantile]) -> Vec<String> {
    let mut values = Vec::new();

    values.push(format!("{} count: {}", name, hist.len()));
    for quantile in quantiles {
        let value = hist.value_at_quantile(quantile.value());
        values.push(format!("{} {}: {}", name, quantile.label(), value));
    }

    values
}