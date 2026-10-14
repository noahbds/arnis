//! Feature download from the Géoplateforme WFS (`data.geopf.fr`).
//!
//! Open data, no key. The service pages at 5000 features and answers a client
//! that goes over its rate limit with HTTP 429 and a `retry-after` of a few
//! seconds, which is honoured here rather than treated as a failure.

use crate::coordinate_system::geographic::LLBBox;
use reqwest::blocking::Client;
use serde_json::Value;
use std::time::Duration;

const WFS_URL: &str = "https://data.geopf.fr/wfs/ows";
/// The service's own page size; asking for more is silently capped.
const PAGE_SIZE: usize = 5000;
/// Guards against a runaway area: 100 pages is half a million features.
const MAX_PAGES: usize = 100;
const HTTP_TIMEOUT_SECS: u64 = 60;
const MAX_ATTEMPTS: u32 = 4;
/// Longest the service is waited on after a 429; its own block lasts 5 seconds.
const MAX_RETRY_AFTER_SECS: u64 = 15;

pub fn client() -> Result<Client, Box<dyn std::error::Error>> {
    Ok(Client::builder()
        .timeout(Duration::from_secs(HTTP_TIMEOUT_SECS))
        .user_agent(concat!(
            "Arnis/",
            env!("CARGO_PKG_VERSION"),
            " (+https://github.com/louis-e/arnis)"
        ))
        .build()?)
}

/// All features of `type_name` intersecting `bbox`, as GeoJSON feature values
/// with coordinates in longitude/latitude order.
pub fn fetch_features(
    client: &Client,
    type_name: &str,
    bbox: &LLBBox,
) -> Result<Vec<Value>, Box<dyn std::error::Error>> {
    // WFS 2.0 with an EPSG:4326 URN takes the bbox in latitude/longitude order.
    let bbox_param = format!(
        "{},{},{},{},urn:ogc:def:crs:EPSG::4326",
        bbox.min().lat(),
        bbox.min().lng(),
        bbox.max().lat(),
        bbox.max().lng()
    );

    let mut features = Vec::new();
    for page in 0..MAX_PAGES {
        let start_index = (page * PAGE_SIZE).to_string();
        let count = PAGE_SIZE.to_string();
        let query = [
            ("SERVICE", "WFS"),
            ("VERSION", "2.0.0"),
            ("REQUEST", "GetFeature"),
            ("TYPENAMES", type_name),
            ("OUTPUTFORMAT", "application/json"),
            ("COUNT", count.as_str()),
            ("STARTINDEX", start_index.as_str()),
            ("BBOX", bbox_param.as_str()),
        ];
        let body = get_json(client, &query)?;
        let Some(page_features) = body.get("features").and_then(Value::as_array) else {
            return Err(format!("{type_name}: response carries no feature list").into());
        };
        let received = page_features.len();
        features.extend(page_features.iter().cloned());
        if received < PAGE_SIZE {
            break;
        }
    }
    Ok(features)
}

fn get_json(client: &Client, query: &[(&str, &str)]) -> Result<Value, Box<dyn std::error::Error>> {
    let mut last_error: Box<dyn std::error::Error> = "no attempt made".into();
    for attempt in 0..MAX_ATTEMPTS {
        match client.get(WFS_URL).query(query).send() {
            Ok(response) if response.status().as_u16() == 429 => {
                let wait = response
                    .headers()
                    .get("retry-after")
                    .and_then(|v| v.to_str().ok())
                    .and_then(|v| v.trim().parse::<u64>().ok())
                    .unwrap_or(5)
                    .min(MAX_RETRY_AFTER_SECS);
                last_error = "rate limited (HTTP 429)".into();
                std::thread::sleep(Duration::from_secs(wait + 1));
            }
            Ok(response) if response.status().is_server_error() => {
                last_error = format!("HTTP {}", response.status()).into();
                std::thread::sleep(Duration::from_millis(500 << attempt));
            }
            Ok(response) => {
                let response = response.error_for_status()?;
                return Ok(response.json::<Value>()?);
            }
            Err(e) => {
                last_error = e.into();
                std::thread::sleep(Duration::from_millis(500 << attempt));
            }
        }
    }
    Err(last_error)
}
