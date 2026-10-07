//! HTTP syntax admission, before a request can reach equipment. GET query keys
//! are case insensitive; PUT form keys follow the Alpaca API's specified casing.
use crate::device::Params;
use axum::http::StatusCode;
use regain_hub::{
    camera::properties::CameraProperty, config::WeatherMetric,
    covercalibrator::CoverCalibratorProperty, filterwheel::FilterWheelProperty,
    focuser::FocuserProperty, rotator::RotatorProperty,
};

pub(crate) fn known_member(kind: &str, member: &str, put: bool) -> bool {
    if put {
        return form_parameters(kind, member).is_some();
    }
    if matches!(
        member,
        "connected"
            | "connecting"
            | "description"
            | "driverinfo"
            | "driverversion"
            | "interfaceversion"
            | "name"
            | "supportedactions"
            | "devicestate"
    ) {
        return true;
    }
    match kind {
        "camera" => {
            CameraProperty::from_member(member).is_some()
                || matches!(member, "imagearray" | "imagearrayvariant")
        }
        "focuser" => member == "link" || FocuserProperty::ALL.iter().any(|p| p.member() == member),
        "rotator" => RotatorProperty::ALL.iter().any(|p| p.member() == member),
        "filterwheel" => FilterWheelProperty::ALL
            .iter()
            .any(|p| p.member() == member),
        "covercalibrator" => CoverCalibratorProperty::ALL
            .iter()
            .any(|p| p.member() == member),
        "safetymonitor" => member == "issafe",
        "switch" => matches!(
            member,
            "maxswitch"
                | "getswitch"
                | "getswitchvalue"
                | "getswitchname"
                | "getswitchdescription"
                | "canwrite"
                | "canasync"
                | "statechangecomplete"
                | "minswitchvalue"
                | "maxswitchvalue"
                | "switchstep"
        ),
        "observingconditions" => {
            matches!(
                member,
                "averageperiod" | "sensordescription" | "timesincelastupdate"
            ) || serde_json::from_value::<WeatherMetric>(serde_json::json!(member)).is_ok()
        }
        _ => false,
    }
}

fn form_parameters(kind: &str, member: &str) -> Option<&'static [&'static str]> {
    Some(match member {
        "connected" => &["Connected"],
        "connect" | "disconnect" => &[],
        "action" => &["Action", "Parameters"],
        "commandblind" | "commandbool" | "commandstring" => &["Command", "Raw"],
        _ => match (kind, member) {
            ("camera", "abortexposure" | "stopexposure") => &[],
            ("camera", "startexposure") => &["Duration", "Light"],
            ("camera", "pulseguide") => &["Direction", "Duration"],
            ("camera", "binx") => &["BinX"],
            ("camera", "biny") => &["BinY"],
            ("camera", "numx") => &["NumX"],
            ("camera", "numy") => &["NumY"],
            ("camera", "startx") => &["StartX"],
            ("camera", "starty") => &["StartY"],
            ("camera", "gain") => &["Gain"],
            ("camera", "offset") => &["Offset"],
            ("camera", "readoutmode") => &["ReadoutMode"],
            ("camera", "fastreadout") => &["FastReadout"],
            ("camera", "cooleron") => &["CoolerOn"],
            ("camera", "setccdtemperature") => &["SetCCDTemperature"],
            ("camera", "subexposureduration") => &["SubExposureDuration"],
            ("focuser", "link") => &["Link"],
            ("focuser", "tempcomp") => &["TempComp"],
            ("focuser" | "rotator", "move") => &["Position"],
            ("focuser" | "rotator", "halt") => &[],
            ("rotator", "moveabsolute" | "movemechanical" | "sync") => &["Position"],
            ("rotator", "reverse") => &["Reverse"],
            ("filterwheel", "position") => &["Position"],
            ("covercalibrator", "opencover" | "closecover" | "haltcover" | "calibratoroff") => &[],
            ("covercalibrator", "calibratoron") => &["Brightness"],
            ("switch", "setswitch" | "setasync") => &["Id", "State"],
            ("switch", "setswitchvalue" | "setasyncvalue") => &["Id", "Value"],
            ("switch", "setswitchname") => &["Id", "Name"],
            ("switch", "cancelasync") => &["Id"],
            ("observingconditions", "averageperiod") => &["AveragePeriod"],
            ("observingconditions", "refresh") => &[],
            _ => return None,
        },
    })
}

pub(crate) fn form(kind: &str, member: &str, body: &str) -> Result<Params, StatusCode> {
    let required = form_parameters(kind, member).ok_or(StatusCode::NOT_FOUND)?;
    let pairs: Vec<(String, String)> =
        serde_urlencoded::from_str(body).map_err(|_| StatusCode::BAD_REQUEST)?;
    if required
        .iter()
        .any(|name| !pairs.iter().any(|(key, _)| key == name))
    {
        return Err(StatusCode::BAD_REQUEST);
    }
    let mut values = std::collections::BTreeMap::new();
    for (key, value) in pairs {
        // These are optional form fields: an incorrectly cased name is absent,
        // not a request to act as that client or echo its transaction number.
        if ["ClientID", "ClientTransactionID"]
            .iter()
            .any(|name| key.eq_ignore_ascii_case(name) && key != *name)
        {
            continue;
        }
        if values.insert(key.to_lowercase(), value).is_some() {
            return Err(StatusCode::BAD_REQUEST);
        }
    }
    let params = Params(values);
    validate_ids(&params)?;
    // Switch IDs must be syntactically valid even for optional operations such
    // as SetSwitchName which this host recognises but does not implement.
    if required.contains(&"Id") {
        params.integer("Id").map_err(|_| StatusCode::BAD_REQUEST)?;
    }
    Ok(params)
}

pub(crate) fn query(text: &str) -> Result<Params, StatusCode> {
    let params = Params::parse(text).map_err(|_| StatusCode::BAD_REQUEST)?;
    validate_ids(&params)?;
    Ok(params)
}

fn validate_ids(params: &Params) -> Result<(), StatusCode> {
    for name in ["ClientID", "ClientTransactionID"] {
        params
            .optional_id(name)
            .map_err(|_| StatusCode::BAD_REQUEST)?;
    }
    Ok(())
}
