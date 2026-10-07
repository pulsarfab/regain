//! Explicit server catalog discovery. No source lease, device GET/PUT, retry,
//! automatic configuration or safety observation is created by this path.
use super::{
    bad_response, http_client, invalid, parse_response, read_body, server_root, transport_error,
};
use crate::{
    config::{DeviceType, normalize_alpaca_id, valid_alpaca_id},
    source::SourceError,
};
use reqwest::header::{AUTHORIZATION, HeaderMap, HeaderValue};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{collections::BTreeSet, time::Duration};
use uuid::Uuid;

pub const TIMEOUT: Duration = Duration::from_secs(5);
pub const MAX_DEVICES: usize = 256;

#[derive(Clone, Debug, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Device {
    #[schemars(length(min = 1, max = 256))]
    pub name: String,
    #[schemars(length(min = 1, max = 64), regex(pattern = "^[A-Za-z]+$"))]
    pub reported_device_type: String,
    pub supported_device_type: Option<DeviceType>,
    #[schemars(range(min = 0, max = 4294967295u64))]
    pub number: u32,
    /// Alpaca UIDs are stable ASCII strings, not necessarily UUIDs.
    #[schemars(length(min = 1, max = 256), regex(pattern = "^[ -~]+$"))]
    pub unique_id: String,
}
#[derive(Debug, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Catalog {
    pub configuration_revision: Uuid,
    #[schemars(length(min = 1, max = 4096))]
    pub base_url: String,
    #[schemars(length(max = 256))]
    pub devices: Vec<Device>,
}
#[derive(Deserialize)]
struct WireDevice {
    #[serde(rename = "DeviceName")]
    name: String,
    #[serde(rename = "DeviceType")]
    device_type: String,
    #[serde(rename = "DeviceNumber")]
    number: u32,
    #[serde(rename = "UniqueID")]
    unique_id: String,
}
#[derive(Deserialize)]
struct WireCatalog {
    #[serde(rename = "Value")]
    devices: Vec<WireDevice>,
}
pub fn description() -> Value {
    json!({"operation":"discoverAlpaca", "opensSource":false, "writesEquipment":false,
    "persistsConfiguration":false, "timeoutSeconds":TIMEOUT.as_secs(), "maximumDevices":MAX_DEVICES,
    "responseSchema":schemars::schema_for!(Catalog),
    "parameters":{
        "baseUrl":{"type":"string","label":"Alpaca server URL","description":"Query this server's configured device catalog without connecting equipment.","maxLength":4096},
        "credentialReference":{"type":"string","label":"Credential reference","description":"Optional protected credential reference for this server. The secret is never included in discovery results.","sensitive":true}
    }})
}
pub async fn discover(
    base_url: &str,
    authorization: Option<HeaderValue>,
    revision: Uuid,
) -> Result<Catalog, SourceError> {
    if revision.is_nil() {
        return Err(invalid("Select the current configuration revision"));
    }
    let root = server_root(base_url)?;
    // Respect the same reverse-proxy prefix as ordinary Alpaca source URLs.
    let base_url = root.as_str().trim_end_matches('/').to_owned();
    let mut headers = HeaderMap::new();
    if let Some(mut authorization) = authorization {
        authorization.set_sensitive(true);
        headers.insert(AUTHORIZATION, authorization);
    }
    let client = http_client(headers, TIMEOUT, true)?;
    let devices = query(&client, root, 1, 1).await?;
    Ok(Catalog {
        configuration_revision: revision,
        base_url,
        devices,
    })
}
// Source pins use the ordinary source client, scalar timeout and transaction
// counter. Catalog setup and runtime identity checks share one strict decoder.
pub(super) async fn query(
    client: &reqwest::Client,
    mut root: url::Url,
    client_id: u32,
    transaction: u32,
) -> Result<Vec<Device>, SourceError> {
    root.set_path(&format!(
        "{}/management/v1/configureddevices",
        root.path().trim_end_matches('/')
    ));
    let mut response = client
        .get(root)
        .query(&[
            ("ClientID", client_id),
            ("ClientTransactionID", transaction),
        ])
        .send()
        .await
        .map_err(|_| transport_error(false))?;
    let body = read_body(&mut response, false, "configureddevices").await?;
    parse_response(&body, transaction, false)?;
    // Decode the original body into typed entries: going through Value would
    // silently collapse duplicate device identity fields before validation.
    let catalog: WireCatalog = serde_json::from_slice(&body).map_err(|_| bad_response(false))?;
    devices(catalog.devices)
}
fn devices(entries: Vec<WireDevice>) -> Result<Vec<Device>, SourceError> {
    if entries.len() > MAX_DEVICES {
        return Err(bad_response(false));
    }
    let mut addresses = BTreeSet::new();
    let mut identities = BTreeSet::new();
    entries
        .into_iter()
        .map(|wire| {
            if wire.name.trim().is_empty()
                || wire.name.chars().count() > 256
                || wire.name.chars().any(char::is_control)
                || wire.device_type.is_empty()
                || wire.device_type.len() > 64
                || !wire.device_type.bytes().all(|b| b.is_ascii_alphabetic())
                || !valid_alpaca_id(&wire.unique_id)
                || !addresses.insert((wire.device_type.to_ascii_lowercase(), wire.number))
                || !identities.insert(normalize_alpaca_id(&wire.unique_id))
            {
                return Err(bad_response(false));
            }
            let supported =
                serde_json::from_value(Value::String(wire.device_type.to_ascii_lowercase())).ok();
            Ok(Device {
                name: wire.name,
                reported_device_type: wire.device_type,
                supported_device_type: supported,
                number: wire.number,
                unique_id: wire.unique_id,
            })
        })
        .collect()
}
