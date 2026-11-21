//! Current weather + next hours from Open-Meteo (free, no API key).
//! Coordinates are rounded to ~1 km before they leave the machine.

use std::process::Command;
use std::sync::mpsc::{Receiver, Sender};

#[derive(Clone, Debug, PartialEq)]
pub struct Hour {
    pub hour: u32,
    pub code: i64,
    pub temp: f64,
    pub is_day: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Weather {
    pub temp: f64,
    pub feels: f64,
    pub humidity: f64,
    pub code: i64,
    pub is_day: bool,
    pub max: f64,
    pub min: f64,
    pub hours: Vec<Hour>,
}

/// SF Symbol + Thai description for a WMO weather code.
pub fn describe(code: i64, is_day: bool) -> (&'static str, &'static str) {
    match code {
        0 => (if is_day { "sun.max.fill" } else { "moon.stars.fill" }, "ท้องฟ้าแจ่มใส"),
        1 | 2 => (if is_day { "cloud.sun.fill" } else { "cloud.moon.fill" }, "มีเมฆบางส่วน"),
        3 => ("cloud.fill", "เมฆมาก"),
        45 | 48 => ("cloud.fog.fill", "มีหมอก"),
        51..=57 => ("cloud.drizzle.fill", "ฝนปรอยๆ"),
        61 | 63 | 66 | 80 | 81 => ("cloud.rain.fill", "ฝนตก"),
        65 | 67 | 82 => ("cloud.heavyrain.fill", "ฝนตกหนัก"),
        71..=77 | 85 | 86 => ("cloud.snow.fill", "หิมะตก"),
        95..=99 => ("cloud.bolt.rain.fill", "พายุฝนฟ้าคะนอง"),
        _ => ("cloud.fill", "—"),
    }
}

pub fn fetch(lat: f64, lon: f64) -> Result<Weather, String> {
    let url = format!(
        "https://api.open-meteo.com/v1/forecast?latitude={lat:.2}&longitude={lon:.2}\
         &current=temperature_2m,apparent_temperature,relative_humidity_2m,weather_code,is_day\
         &hourly=temperature_2m,weather_code,is_day\
         &daily=temperature_2m_max,temperature_2m_min\
         &timezone=auto&forecast_days=2"
    );
    let out = Command::new("/usr/bin/curl")
        .args(["-sf", "--max-time", "10", &url])
        .output()
        .map_err(|e| e.to_string())?;
    if !out.status.success() {
        return Err("โหลดข้อมูลอากาศไม่ได้".into());
    }
    parse(&out.stdout)
}

fn parse(body: &[u8]) -> Result<Weather, String> {
    let j: serde_json::Value = serde_json::from_slice(body).map_err(|e| e.to_string())?;
    let c = &j["current"];
    let f = |v: &serde_json::Value| v.as_f64().unwrap_or(0.0);
    let now_time = c["time"].as_str().unwrap_or(""); // "2026-11-22T14:15" local
    let now_hour_key = now_time.get(..13).unwrap_or(""); // "2026-11-22T14"

    let h = &j["hourly"];
    let times = h["time"].as_array().cloned().unwrap_or_default();
    let start = times
        .iter()
        .position(|t| t.as_str().is_some_and(|t| t.starts_with(now_hour_key)))
        .map(|i| i + 1) // next full hour
        .unwrap_or(0);
    let hours = (start..(start + 6).min(times.len()))
        .map(|i| Hour {
            hour: times[i].as_str().and_then(|t| t.get(11..13)).and_then(|s| s.parse().ok()).unwrap_or(0),
            code: h["weather_code"][i].as_i64().unwrap_or(0),
            temp: f(&h["temperature_2m"][i]),
            is_day: h["is_day"][i].as_i64().unwrap_or(1) == 1,
        })
        .collect();

    Ok(Weather {
        temp: f(&c["temperature_2m"]),
        feels: f(&c["apparent_temperature"]),
        humidity: f(&c["relative_humidity_2m"]),
        code: c["weather_code"].as_i64().unwrap_or(0),
        is_day: c["is_day"].as_i64().unwrap_or(1) == 1,
        max: f(&j["daily"]["temperature_2m_max"][0]),
        min: f(&j["daily"]["temperature_2m_min"][0]),
        hours,
    })
}

/// Background loop: fetch for each coordinate pair it receives.
pub fn run(rx: Receiver<(f64, f64)>, tx: Sender<Result<Weather, String>>) {
    while let Ok((lat, lon)) = rx.recv() {
        if tx.send(fetch(lat, lon)).is_err() {
            return;
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn live_fetch() {
        // generic city-centre coordinates (Bangkok), not the user's location
        let w = super::fetch(13.75, 100.50);
        println!("{w:#?}");
        let w = w.unwrap();
        assert_eq!(w.hours.len(), 6);
    }
}
