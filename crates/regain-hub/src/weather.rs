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
    /// Prefer the interface's spelling on the wire. Names remain caseless in
    /// configuration; this also works with drivers that incorrectly use a
    /// case-sensitive sensor dictionary. Unknown property names are preserved.
    pub(crate) fn sensor_name(property: &str) -> &str {
        match serde_json::from_value::<Self>(serde_json::Value::String(
            property.to_ascii_lowercase(),
        )) {
            Ok(metric) => metric.state_name(),
            Err(_) => property,
        }
    }
    pub(crate) fn state_name(self) -> &'static str {
        match self {
            Self::CloudCover => "CloudCover",
            Self::DewPoint => "DewPoint",
            Self::Humidity => "Humidity",
            Self::Pressure => "Pressure",
            Self::RainRate => "RainRate",
            Self::SkyBrightness => "SkyBrightness",
            Self::SkyQuality => "SkyQuality",
            Self::SkyTemperature => "SkyTemperature",
            Self::StarFwhm => "StarFWHM",
            Self::Temperature => "Temperature",
            Self::WindDirection => "WindDirection",
            Self::WindGust => "WindGust",
            Self::WindSpeed => "WindSpeed",
        }
    }
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
        let (minimum, maximum, exclusive) = self.bounds();
        value.is_finite()
            && minimum.is_none_or(|min| if exclusive { value > min } else { value >= min })
            && maximum.is_none_or(|max| value <= max)
    }
    /// The same physical limits validate readings and describe setup controls.
    pub(crate) fn bounds(self) -> (Option<f64>, Option<f64>, bool) {
        match self {
            Self::CloudCover | Self::Humidity => (Some(0.0), Some(100.0), false),
            Self::WindDirection => (Some(0.0), Some(360.0), false),
            Self::DewPoint | Self::SkyTemperature | Self::Temperature => {
                (Some(-273.15), None, false)
            }
            Self::Pressure => (Some(0.0), None, true),
            Self::SkyQuality => (None, None, false),
            _ => (Some(0.0), None, false),
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

#[derive(Clone, Debug, Serialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WeatherReading {
    pub value: f64,
    pub unit: &'static str,
    pub source: Uuid,
    pub revision: Uuid,
    pub generation: Uuid,
    pub sequence: u64,
    pub readout: Readout,
    pub age_seconds: f64,
    pub sample_count: usize,
    pub average_seconds: f64,
}
#[derive(Clone)]
struct Point {
    at: f64,
    age: f64,
    value: f64,
}
#[derive(Clone, Default)]
struct History {
    identity: Option<(usize, Uuid, Uuid)>,
    sequence: u64,
    points: VecDeque<Point>,
}
pub struct WeatherEngine {
    resume_epoch: u64,
    measurements: BTreeMap<WeatherMetric, Measurement>,
    states: BTreeMap<Uuid, SourceSnapshot>,
    history: BTreeMap<WeatherMetric, History>,
    average: f64,
    last_now: f64,
    clock_fault: bool,
    last_valid: BTreeMap<WeatherMetric, (f64, f64)>,
}
impl WeatherEngine {
    fn diagnostic_copy(&self, metrics: &[WeatherMetric]) -> Self {
        let mut metrics: std::collections::BTreeSet<_> = metrics.iter().copied().collect();
        if metrics.contains(&WeatherMetric::WindDirection) {
            metrics.insert(WeatherMetric::WindSpeed);
        }
        let measurements: BTreeMap<_, _> = self
            .measurements
            .iter()
            .filter(|(metric, _)| metrics.contains(metric))
            .map(|(metric, measurement)| (*metric, measurement.clone()))
            .collect();
        let mut keys: BTreeMap<Uuid, std::collections::BTreeSet<String>> = BTreeMap::new();
        for readout in measurements
            .values()
            .flat_map(|measurement| &measurement.sources)
        {
            keys.entry(readout.source())
                .or_default()
                .insert(readout.sample_key());
        }
        Self {
            resume_epoch: self.resume_epoch,
            measurements,
            states: keys
                .iter()
                .filter_map(|(source, keys)| {
                    self.states
                        .get(source)
                        .map(|state| (*source, state.project_samples(keys)))
                })
                .collect(),
            history: self
                .history
                .iter()
                .filter(|(metric, _)| metrics.contains(metric))
                .map(|(metric, history)| (*metric, history.clone()))
                .collect(),
            average: self.average,
            last_now: self.last_now,
            clock_fault: self.clock_fault,
            last_valid: self
                .last_valid
                .iter()
                .filter(|(metric, _)| metrics.contains(metric))
                .map(|(metric, time)| (*metric, *time))
                .collect(),
        }
    }
    pub fn new(measurements: BTreeMap<WeatherMetric, Measurement>) -> Self {
        let average = measurements
            .iter()
            .find(|(metric, _)| **metric != WeatherMetric::WindGust)
            .map_or(0.0, |(_, measurement)| measurement.average_seconds);
        Self {
            resume_epoch: 0,
            measurements,
            states: BTreeMap::new(),
            history: BTreeMap::new(),
            average,
            last_now: 0.0,
            clock_fault: false,
            last_valid: BTreeMap::new(),
        }
    }
    fn sync_resume(&mut self, clock: &dyn Clock) {
        let epoch = clock.resume_epoch();
        if self.resume_epoch != epoch || !clock.resume_clock_valid() {
            self.resume_epoch = epoch;
            self.states.clear();
            self.history.clear();
            self.last_valid.clear();
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
        // A reader may have fetched a source just before resume and acquired
        // this engine lock just after it. Never seed the new epoch with it.
        if state.resume_epoch != self.resume_epoch {
            return;
        }
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
            revision: sample.revision,
            generation: sample.generation,
            sequence: sample.sequence,
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
            let source = registry.get(*source)?;
            let mut status = source.status();
            let engine = engine.clone();
            let clock = clock.clone();
            let mut stop = stop.subscribe();
            tokio::spawn(async move {
                loop {
                    if *stop.borrow() {
                        break;
                    }
                    status.borrow_and_update();
                    {
                        let mut engine = engine.lock().unwrap();
                        engine.sync_resume(clock.as_ref());
                        engine.observe(source.snapshot(), clock.now());
                    }
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
    pub(crate) fn diagnostics(
        &self,
        start: u32,
        end: u32,
        now: Duration,
    ) -> (f64, Vec<crate::diagnostics::WeatherMeasurement>) {
        // Resume must withdraw old evidence in every cached view. After that,
        // inspect averaging/fallback on a private copy without pruning, seeding
        // or changing live history/last-valid clocks through diagnostic reads.
        let mut engine = self.engine.lock().unwrap();
        engine.sync_resume(self.clock.as_ref());
        let measurements: Vec<_> = engine
            .measurements
            .iter()
            .skip(start as usize)
            .take((end - start) as usize)
            .map(|(metric, configuration)| (*metric, configuration.clone()))
            .collect();
        let copy = engine.diagnostic_copy(
            &measurements
                .iter()
                .map(|(metric, _)| *metric)
                .collect::<Vec<_>>(),
        );
        drop(engine);
        let mut engine = copy;
        let average = engine.average_period_hours();
        let measurements = measurements
            .into_iter()
            .map(|(metric, configuration)| {
                let sample = engine.read(metric, now).into();
                let sources = configuration
                    .sources
                    .iter()
                    .map(|readout| {
                        // The registry is validated and immutable in this runtime.
                        self.registry
                            .get(readout.source())
                            .unwrap()
                            .with_snapshot(|state| crate::diagnostics::SourceHealth::from(state))
                    })
                    .collect();
                crate::diagnostics::WeatherMeasurement {
                    metric,
                    configuration,
                    sample,
                    sources,
                }
            })
            .collect();
        (average, measurements)
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
    /// One shared-engine lock and one monotonic time for this cached collection.
    /// A failed sensor does not suppress valid readings from another sensor.
    pub(crate) fn device_state(&self) -> BTreeMap<WeatherMetric, f64> {
        let mut engine = self.output.engine.lock().unwrap();
        engine.sync_resume(self.output.clock.as_ref());
        let now = self.output.clock.now();
        let metrics: Vec<_> = engine.measurements.keys().copied().collect();
        metrics
            .into_iter()
            .filter_map(|metric| {
                engine
                    .read(metric, now)
                    .ok()
                    .map(|reading| (metric, reading.value))
            })
            .collect()
    }
    pub(crate) fn sensor_description(&self, property: &str) -> Result<String, SourceError> {
        let metric: WeatherMetric =
            serde_json::from_value(serde_json::Value::from(property.to_ascii_lowercase()))
                .map_err(|_| invalid("Unknown weather property"))?;
        if !self
            .output
            .engine
            .lock()
            .unwrap()
            .measurements
            .contains_key(&metric)
        {
            return Err(SourceError::new(
                ErrorKind::Unsupported,
                "Weather measurement is not configured",
            ));
        }
        Ok(format!("Regain {} ({})", metric.property(), metric.unit()))
    }
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
        let mut engine = self.output.engine.lock().unwrap();
        engine.sync_resume(self.output.clock.as_ref());
        engine.read(metric, self.output.clock.now())
    }
    pub fn time_since_last_update(&self, property: &str) -> Result<f64, SourceError> {
        let mut engine = self.output.engine.lock().unwrap();
        engine.sync_resume(self.output.clock.as_ref());
        engine.time_since_last_update(property, self.output.clock.now())
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

#[cfg(test)]
mod diagnostic_tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn canonical_sensor_parameters_preserve_configuration_and_cache_keys() {
        use crate::sampling::{PropertyPoll, SampleRequest};
        for (key, name) in [
            ("cloudcover", "CloudCover"),
            ("dewpoint", "DewPoint"),
            ("humidity", "Humidity"),
            ("pressure", "Pressure"),
            ("rainrate", "RainRate"),
            ("skybrightness", "SkyBrightness"),
            ("skyquality", "SkyQuality"),
            ("skytemperature", "SkyTemperature"),
            ("starfwhm", "StarFWHM"),
            ("temperature", "Temperature"),
            ("winddirection", "WindDirection"),
            ("windgust", "WindGust"),
            ("windspeed", "WindSpeed"),
        ] {
            assert_eq!(WeatherMetric::sensor_name(key), name);
            assert_eq!(WeatherMetric::sensor_name(&key.to_ascii_uppercase()), name);
            let sample = SampleRequest::readout(
                &Readout::Property {
                    source: Uuid::new_v4(),
                    property: key.into(),
                    unit: None,
                },
                true,
            );
            assert_eq!(sample.key, key);
            assert_eq!(sample.member, key);
            assert_eq!(sample.sensor_age.as_deref(), Some(key));
            let poll = PropertyPoll::new(DeviceType::ObservingConditions, vec![sample], 1).unwrap();
            let request = poll.prepare().unwrap();
            assert_eq!(request.member, "timesincelastupdate");
            assert_eq!(request.parameters["SensorName"], name);
        }
        for unknown in ["gain", "CustomSensor", ""] {
            assert_eq!(WeatherMetric::sensor_name(unknown), unknown);
        }
    }
    #[tokio::test]
    async fn resume_rejects_a_snapshot_fetched_before_the_engine_lock() {
        let source = Uuid::new_v4();
        let clock =
            crate::resume::ResumeClock::manual(Arc::new(crate::safety::MonotonicClock::default()));
        let mut engine = WeatherEngine::new(BTreeMap::from([(
            WeatherMetric::Temperature,
            Measurement {
                sources: vec![Readout::Property {
                    source,
                    property: "temperature".into(),
                    unit: None,
                }],
                maximum_age_seconds: 60.0,
                average_seconds: 10.0,
            },
        )]));
        let mut old = SourceSnapshot {
            resume_epoch: 0,
            source,
            revision: Uuid::new_v4(),
            generation: Uuid::new_v4(),
            sequence: 1,
            transport_connected: true,
            write_uncertain: false,
            connection_info: None,
            simulated: false,
            simulation: None,
            lease_count: 1,
            values: BTreeMap::from([("temperature".into(), json!(20))]),
            sample_errors: BTreeMap::new(),
            sample_ages_seconds: BTreeMap::new(),
            sample_started_seconds: BTreeMap::new(),
            sample_sequences: BTreeMap::new(),
            completed_passes: 1,
            sampled_at_seconds: Some(0.0),
            error: None,
            polling: Default::default(),
        };
        engine.observe(old.clone(), Duration::ZERO);
        assert_eq!(
            engine
                .read(WeatherMetric::Temperature, Duration::ZERO)
                .unwrap()
                .value,
            20.0
        );
        clock.notify_resume();
        engine.sync_resume(clock.as_ref());
        engine.observe(old.clone(), Duration::ZERO);
        assert!(
            engine
                .read(WeatherMetric::Temperature, Duration::ZERO)
                .is_err()
        );
        assert_eq!(
            engine
                .time_since_last_update("temperature", Duration::ZERO)
                .unwrap(),
            -1.0
        );
        old.resume_epoch = clock.resume_epoch();
        old.generation = Uuid::new_v4();
        old.values.insert("temperature".into(), json!(30));
        engine.observe(old, Duration::ZERO);
        let fresh = engine
            .read(WeatherMetric::Temperature, Duration::ZERO)
            .unwrap();
        assert_eq!(fresh.value, 30.0);
        assert_eq!(fresh.sample_count, 1);
    }

    #[test]
    fn diagnostic_copy_retains_sample_identity_and_cannot_mutate_live_history_or_clocks() {
        let source = Uuid::new_v4();
        let revision = Uuid::new_v4();
        let generation = Uuid::new_v4();
        let mut engine = WeatherEngine::new(BTreeMap::from([(
            WeatherMetric::Temperature,
            Measurement {
                sources: vec![Readout::Property {
                    source,
                    property: "temperature".into(),
                    unit: None,
                }],
                maximum_age_seconds: 5.0,
                average_seconds: 2.0,
            },
        )]));
        let state = SourceSnapshot {
            resume_epoch: 0,
            source,
            revision,
            generation,
            sequence: 1,
            transport_connected: true,
            write_uncertain: false,
            connection_info: None,
            simulated: false,
            simulation: None,
            lease_count: 1,
            values: BTreeMap::from([
                ("temperature".into(), json!(20)),
                (
                    "arbitraryVendorText".into(),
                    json!("PRIVATE_FIXTURE_SECRET".repeat(100_000)),
                ),
            ]),
            sample_errors: BTreeMap::new(),
            sample_ages_seconds: BTreeMap::new(),
            sample_started_seconds: BTreeMap::new(),
            sample_sequences: BTreeMap::new(),
            completed_passes: 1,
            sampled_at_seconds: Some(0.0),
            error: None,
            polling: Default::default(),
        };
        engine.observe(state, Duration::ZERO);
        let history_count = engine.history[&WeatherMetric::Temperature].points.len();
        let last_valid = engine.last_valid.clone();
        let mut copy = engine.diagnostic_copy(&[WeatherMetric::Temperature]);
        assert_eq!(copy.states[&source].values.len(), 1);
        let reading = copy
            .read(WeatherMetric::Temperature, Duration::from_secs(1))
            .unwrap();
        assert_eq!(reading.revision, revision);
        assert_eq!(reading.generation, generation);
        assert_eq!(reading.sequence, 1);
        assert_eq!(reading.sample_count, history_count);
        assert!(
            copy.read(WeatherMetric::Temperature, Duration::from_secs(5))
                .is_err()
        );
        assert!(!copy.history.contains_key(&WeatherMetric::Temperature));
        assert_eq!(
            engine.history[&WeatherMetric::Temperature].points.len(),
            history_count
        );
        assert_eq!(engine.last_now, 0.0);
        assert_eq!(engine.last_valid, last_valid);
        assert_eq!(engine.states[&source].values.len(), 2);
    }
}
