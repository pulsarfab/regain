//! Weather selection and averaging are per metric. Cached data never becomes
//! younger when read, and switching sources discards the previous history.
use crate::{
    config::{DeviceType, HubConfig, Measurement, Readout, VirtualDevice, WeatherMetric},
    parameters::FieldError,
    readout::{ScalarSample, SourceLease, invalid, scalar, unavailable},
    safety::Clock,
    source::{ErrorKind, SourceError, SourceRegistry, SourceSnapshot},
};
use serde::Serialize;
use std::{
    collections::{BTreeMap, VecDeque},
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::sync::watch;
use uuid::Uuid;

impl WeatherMetric {
    pub fn property(self) -> String {
        serde_json::to_value(self).unwrap().as_str().unwrap().into()
    }
    pub fn unit(self) -> &'static str {
        match self {
            Self::CloudCover | Self::Humidity => "%",
            Self::DewPoint | Self::SkyTemperature | Self::Temperature => "°C",
            Self::Pressure => "hPa",
            Self::RainRate => "mm/h",
            Self::SkyBrightness => "lux",
            Self::SkyQuality => "mag/arcsec²",
            Self::StarFwhm => "arcsec",
            Self::WindDirection => "deg",
            Self::WindGust | Self::WindSpeed => "m/s",
        }
    }
    pub(crate) fn accepts(self, value: f64) -> bool {
        value.is_finite()
            && match self {
                Self::CloudCover | Self::Humidity => (0.0..=100.0).contains(&value),
                Self::WindDirection => (0.0..=360.0).contains(&value),
                Self::DewPoint | Self::SkyTemperature | Self::Temperature => value >= -273.15,
                Self::Pressure => value > 0.0,
                Self::SkyQuality => true,
                _ => value >= 0.0,
            }
    }
}
pub fn validate_measurements(
    config: &HubConfig,
    measurements: &BTreeMap<WeatherMetric, Measurement>,
) -> Vec<FieldError> {
    let mut errors = Vec::new();
    let mut error =
        |key: String, message: &str| errors.push(FieldError::new(key, "weather", message));
    if measurements.contains_key(&WeatherMetric::Humidity)
        != measurements.contains_key(&WeatherMetric::DewPoint)
    {
        error(
            "measurements".into(),
            "ASCOM requires humidity and dew point together",
        );
    }
    if measurements.contains_key(&WeatherMetric::WindDirection)
        && !measurements.contains_key(&WeatherMetric::WindSpeed)
    {
        error(
            "measurements.winddirection".into(),
            "Wind direction needs wind speed to report calm correctly",
        );
    }
    let average = measurements
        .iter()
        .find(|(metric, _)| **metric != WeatherMetric::WindGust)
        .map(|(_, measurement)| measurement.average_seconds);
    for (metric, measurement) in measurements {
        if *metric != WeatherMetric::WindGust && Some(measurement.average_seconds) != average {
            error(
                format!("measurements.{}.averageSeconds", metric.property()),
                "ASCOM exposes one averaging period per weather output",
            );
        }
        for (index, readout) in measurement.sources.iter().enumerate() {
            let (known, declared) = match readout {
                Readout::Channel { unit, .. } => (None, unit.as_deref()),
                Readout::Property {
                    source,
                    property,
                    unit,
                } => {
                    let kind = config.source_type(*source);
                    let known = if kind == Some(DeviceType::ObservingConditions) {
                        serde_json::from_value::<WeatherMetric>(serde_json::Value::from(
                            property.clone(),
                        ))
                        .ok()
                        .map(WeatherMetric::unit)
                    } else if (kind == Some(DeviceType::Focuser) && property == "temperature")
                        || (kind == Some(DeviceType::Camera) && property == "ccdtemperature")
                    {
                        Some("°C")
                    } else if kind == Some(DeviceType::SafetyMonitor) && property == "issafe" {
                        Some("")
                    } else {
                        None
                    };
                    (known, unit.as_deref())
                }
            };
            if known.is_some() && declared.is_some() && known != declared
                || known.or(declared) != Some(metric.unit())
            {
                error(
                    format!("measurements.{}.sources[{index}]", metric.property()),
                    "Source unit must match the metric's canonical unit; no conversion is implied",
                );
            }
            if *metric == WeatherMetric::WindGust
                && !matches!(readout,Readout::Property {source,property,..} if config.source_type(*source)==Some(DeviceType::ObservingConditions) && property=="windgust")
            {
                error(
                    "measurements.windgust".into(),
                    "Wind gust requires a source reporting the ASCOM three-second peak over two minutes",
                );
            }
        }
    }
    errors
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WeatherReading {
    pub value: f64,
    pub unit: &'static str,
    pub source: Uuid,
    pub readout: Readout,
    pub age_seconds: f64,
    pub sample_count: usize,
    pub average_seconds: f64,
}
struct Point {
    at: f64,
    age: f64,
    value: f64,
}
#[derive(Default)]
struct History {
    identity: Option<(usize, Uuid, Uuid)>,
    sequence: u64,
    points: VecDeque<Point>,
}
pub struct WeatherEngine {
    measurements: BTreeMap<WeatherMetric, Measurement>,
    states: BTreeMap<Uuid, SourceSnapshot>,
    history: BTreeMap<WeatherMetric, History>,
    average: f64,
    last_now: f64,
    clock_fault: bool,
    last_valid: BTreeMap<WeatherMetric, (f64, f64)>,
}
impl WeatherEngine {
    pub fn new(measurements: BTreeMap<WeatherMetric, Measurement>) -> Self {
        let average = measurements
            .iter()
            .find(|(metric, _)| **metric != WeatherMetric::WindGust)
            .map_or(0.0, |(_, measurement)| measurement.average_seconds);
        Self {
            measurements,
            states: BTreeMap::new(),
            history: BTreeMap::new(),
            average,
            last_now: 0.0,
            clock_fault: false,
            last_valid: BTreeMap::new(),
        }
    }
    fn tick(&mut self, now: Duration) -> Result<(), SourceError> {
        if now.as_secs_f64() < self.last_now {
            self.clock_fault = true;
            self.history.clear();
            self.states.clear();
        }
        self.last_now = now.as_secs_f64();
        if self.clock_fault {
            return Err(unavailable("Weather clock changed; reconnect the output"));
        }
        Ok(())
    }
    pub fn observe(&mut self, state: SourceSnapshot, now: Duration) {
        if self.tick(now).is_err() {
            return;
        }
        if self.states.get(&state.source).is_some_and(|old| {
            old.generation == state.generation
                && old.revision == state.revision
                && old.sequence > state.sequence
        }) {
            return;
        }
        self.states.insert(state.source, state);
        for metric in self.measurements.keys().copied().collect::<Vec<_>>() {
            let _ = self.read(metric, now);
        }
    }
    pub fn average_period_hours(&self) -> f64 {
        self.average / 3600.0
    }
    pub fn set_average_period_hours(&mut self, hours: f64) -> Result<(), SourceError> {
        if !hours.is_finite() || !(0.0..=1.0).contains(&hours) {
            return Err(invalid(
                "Averaging period must be between zero and one hour",
            ));
        }
        if self.average != hours * 3600.0 {
            self.average = hours * 3600.0;
            self.history.clear();
        }
        Ok(())
    }
    fn select(
        &self,
        metric: WeatherMetric,
        now: Duration,
    ) -> Result<(usize, ScalarSample), SourceError> {
        let measurement = self.measurements.get(&metric).ok_or_else(|| {
            SourceError::new(
                ErrorKind::Unsupported,
                "Weather measurement is not configured",
            )
        })?;
        measurement
            .sources
            .iter()
            .enumerate()
            .find_map(|(index, readout)| {
                let state = self.states.get(&readout.source())?;
                if !state.values.get(&readout.sample_key())?.is_number() {
                    return None;
                }
                scalar(state, readout, now)
                    .ok()
                    .filter(|sample| {
                        sample.age_seconds < measurement.maximum_age_seconds
                            && metric.accepts(sample.value)
                    })
                    .map(|sample| (index, sample))
            })
            .ok_or_else(|| unavailable("No fresh valid source for this weather measurement"))
    }
    pub fn read(
        &mut self,
        metric: WeatherMetric,
        now: Duration,
    ) -> Result<WeatherReading, SourceError> {
        self.tick(now)?;
        let (index, sample) = match self.select(metric, now) {
            Ok(sample) => sample,
            Err(error) => {
                self.history.remove(&metric);
                return Err(error);
            }
        };
        let measurement = &self.measurements[&metric];
        self.last_valid
            .insert(metric, (now.as_secs_f64(), sample.age_seconds));
        let history = self.history.entry(metric).or_default();
        let identity = (index, sample.generation, sample.revision);
        if history.identity != Some(identity)
            || sample.sequence > history.sequence.saturating_add(1)
        {
            *history = History {
                identity: Some(identity),
                ..History::default()
            };
        }
        if history.sequence != sample.sequence || history.points.is_empty() {
            history.sequence = sample.sequence;
            history.points.push_back(Point {
                at: now.as_secs_f64(),
                age: sample.age_seconds,
                value: sample.value,
            });
        }
        let average = if metric == WeatherMetric::WindGust {
            0.0
        } else {
            self.average
        };
        // Keep one predecessor for the beginning of a time-weighted window.
        let start = now.as_secs_f64() - average;
        while history.points.len() > 1
            && (history.points[1].at <= start
                || history.points[0].age + now.as_secs_f64() - history.points[0].at
                    >= measurement.maximum_age_seconds
                || history.points.len() > 36002)
        {
            history.points.pop_front();
        }
        let mut value = sample.value;
        if average > 0.0 && history.points.len() > 1 {
            let total = (now.as_secs_f64() - history.points[0].at.max(start)).max(0.0);
            let mut sum = 0.0;
            let mut cosine = 0.0;
            for (point, next) in history.points.iter().zip(
                history
                    .points
                    .iter()
                    .skip(1)
                    .map(|point| point.at)
                    .chain(std::iter::once(now.as_secs_f64())),
            ) {
                let weight = if total > 0.0 {
                    (next - point.at.max(start)).max(0.0) / total
                } else {
                    0.0
                };
                if metric == WeatherMetric::WindDirection {
                    sum += weight * point.value.to_radians().sin();
                    cosine += weight * point.value.to_radians().cos();
                } else {
                    sum += weight * point.value;
                }
            }
            if total > 0.0 {
                if metric == WeatherMetric::WindDirection {
                    if sum.hypot(cosine) <= 1e-12 {
                        return Err(unavailable("Wind directions have no defined circular mean"));
                    }
                    value = sum.atan2(cosine).to_degrees().rem_euclid(360.0);
                } else {
                    value = sum;
                }
            }
        }
        if !value.is_finite() {
            return Err(unavailable("Weather average is outside the numeric range"));
        }
        let reading = WeatherReading {
            value,
            unit: metric.unit(),
            source: sample.source,
            readout: measurement.sources[index].clone(),
            age_seconds: sample.age_seconds,
            sample_count: history.points.len(),
            average_seconds: average,
        };
        if metric == WeatherMetric::WindDirection
            && self.read(WeatherMetric::WindSpeed, now)?.value == 0.0
        {
            return Ok(WeatherReading {
                value: 0.0,
                ..reading
            });
        }
        Ok(reading)
    }
    pub fn time_since_last_update(
        &mut self,
        property: &str,
        now: Duration,
    ) -> Result<f64, SourceError> {
        self.tick(now)?;
        for metric in self.measurements.keys().copied().collect::<Vec<_>>() {
            if let Ok((_, sample)) = self.select(metric, now) {
                self.last_valid
                    .insert(metric, (now.as_secs_f64(), sample.age_seconds));
            }
        }
        if property.is_empty() {
            return Ok(self
                .last_valid
                .values()
                .map(|(at, age)| age + now.as_secs_f64() - at)
                .reduce(f64::min)
                .unwrap_or(-1.0));
        }
        let metric: WeatherMetric =
            serde_json::from_value(serde_json::Value::from(property.to_ascii_lowercase()))
                .map_err(|_| invalid("Unknown weather property"))?;
        if !self.measurements.contains_key(&metric) {
            return Err(SourceError::new(
                ErrorKind::Unsupported,
                "Weather measurement is not configured",
            ));
        }
        Ok(self
            .last_valid
            .get(&metric)
            .map_or(-1.0, |(at, age)| age + now.as_secs_f64() - at))
    }
}

pub struct WeatherOutput {
    engine: Arc<Mutex<WeatherEngine>>,
    registry: Arc<SourceRegistry>,
    sources: Vec<Uuid>,
    clock: Arc<dyn Clock>,
    stop: watch::Sender<bool>,
}
impl WeatherOutput {
    pub fn new(
        config: &HubConfig,
        id: Uuid,
        registry: Arc<SourceRegistry>,
        clock: Arc<dyn Clock>,
    ) -> Result<Arc<Self>, SourceError> {
        if !config.validate().is_empty() {
            return Err(invalid("Invalid hub configuration"));
        }
        let Some(VirtualDevice::Weather { measurements }) = config
            .outputs
            .iter()
            .find(|output| output.id == id)
            .map(|output| &output.device)
        else {
            return Err(invalid("Expected a weather output"));
        };
        let sources: Vec<_> = measurements
            .values()
            .flat_map(|measurement| measurement.sources.iter().map(Readout::source))
            .collect::<std::collections::BTreeSet<_>>()
            .into_iter()
            .collect();
        let engine = Arc::new(Mutex::new(WeatherEngine::new(measurements.clone())));
        let (stop, _) = watch::channel(false);
        for source in &sources {
            let mut status = registry.get(*source)?.status();
            let engine = engine.clone();
            let clock = clock.clone();
            let mut stop = stop.subscribe();
            tokio::spawn(async move {
                loop {
                    if *stop.borrow() {
                        break;
                    }
                    engine
                        .lock()
                        .unwrap()
                        .observe(status.borrow_and_update().clone(), clock.now());
                    tokio::select! {biased;_=stop.changed()=>break,changed=status.changed()=>if changed.is_err(){break;}}
                }
            });
        }
        Ok(Arc::new(Self {
            engine,
            registry,
            sources,
            clock,
            stop,
        }))
    }
    pub async fn connect(self: &Arc<Self>) -> Result<WeatherSession, SourceError> {
        let mut leases = Vec::new();
        for source in &self.sources {
            leases.push(SourceLease::acquire(self.registry.get(*source)?).await?);
        }
        Ok(WeatherSession {
            output: self.clone(),
            _leases: leases,
        })
    }
}
impl Drop for WeatherOutput {
    fn drop(&mut self) {
        self.stop.send_replace(true);
    }
}
pub struct WeatherSession {
    output: Arc<WeatherOutput>,
    _leases: Vec<SourceLease>,
}
impl WeatherSession {
    pub async fn refresh(&self) -> Result<(), SourceError> {
        let mut requests = tokio::task::JoinSet::new();
        for lease in &self._leases {
            let source = lease.source.clone();
            let id = lease.id;
            requests.spawn(async move { source.refresh(id).await });
        }
        let mut error = None;
        while let Some(result) = requests.join_next().await {
            let result = result.unwrap_or_else(|_| Err(unavailable("Source refresh task stopped")));
            if let Err(failure) = result {
                error.get_or_insert(failure);
            }
        }
        error.map_or(Ok(()), Err)
    }
    pub fn read(&self, metric: WeatherMetric) -> Result<WeatherReading, SourceError> {
        self.output
            .engine
            .lock()
            .unwrap()
            .read(metric, self.output.clock.now())
    }
    pub fn time_since_last_update(&self, property: &str) -> Result<f64, SourceError> {
        self.output
            .engine
            .lock()
            .unwrap()
            .time_since_last_update(property, self.output.clock.now())
    }
    pub fn average_period_hours(&self) -> f64 {
        self.output.engine.lock().unwrap().average_period_hours()
    }
    pub fn set_average_period_hours(&self, hours: f64) -> Result<(), SourceError> {
        self.output
            .engine
            .lock()
            .unwrap()
            .set_average_period_hours(hours)
    }
}
