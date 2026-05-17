use clap::Parser;
use exif::{In, Tag};
use serde::Deserialize;
use std::collections::HashMap;
use std::fs;
use std::io::{self, Read as _};
use std::path::{Path, PathBuf};
use tokio::time::{sleep, Duration};

#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
struct Args {
    /// Path to the directory containing image files
    #[arg(default_value = ".")]
    path: PathBuf,
}

#[derive(Deserialize, Debug)]
struct Address {
    road: Option<String>,
    city: Option<String>,
    town: Option<String>,
    village: Option<String>,
    state: Option<String>,
    country: Option<String>,
    country_code: Option<String>
}

#[derive(Deserialize, Debug)]
struct GeocodeResponse {
    display_name: String,
    address: Address,
}

struct FileInfo {
    path: PathBuf,
    lat: Option<f64>,
    lon: Option<f64>,
    date: String,
    sort_key: String,
}

const API_KEY: &str = match option_env!("API_KEY") {
    Some(key) => key,
    None => "REPLACE_ME_AT_BUILD_TIME",
};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    if API_KEY == "REPLACE_ME_AT_BUILD_TIME" {
        eprintln!("Warning: API_KEY was not provided at build time. Reverse geocoding will fail.");
    }

    let args = Args::parse();

    if !args.path.is_dir() {
        eprintln!("Error: Provided path is not a directory.");
        std::process::exit(1);
    }

    // Collect all supported image files with their metadata
    let mut file_infos: Vec<FileInfo> = Vec::new();

    for entry in fs::read_dir(&args.path)? {
        let entry = entry?;
        let path = entry.path();

        if !is_supported_image(&path) {
            continue;
        }

        println!("Scanning: {:?}", path);
        match extract_metadata(&path) {
            Some(info) => {
                file_infos.push(info);
            }
            None => {
                println!("  Missing GPS or Date metadata, skipping.");
            }
        }
    }

    // Group files by date (yyyymmdd)
    let mut by_date: HashMap<String, Vec<FileInfo>> = HashMap::new();
    for info in file_infos {
        by_date.entry(info.date.clone()).or_default().push(info);
    }

    // Sort dates so we process them in chronological order
    let mut dates: Vec<String> = by_date.keys().cloned().collect();
    dates.sort();

    for date in dates {
        let group = by_date.get_mut(&date).unwrap();
        // Sort within the day by full datetime (sort_key is the 14-digit numeric string)
        group.sort_by(|a, b| a.sort_key.cmp(&b.sort_key));

        let mut sequence: u32 = 1;

        for info in group.iter() {
            println!("Processing: {:?}", info.path);
            println!("  Found date: {}", info.date);

            if let (Some(lat), Some(lon)) = (info.lat, info.lon) {
                println!("  Found coordinates: {}, {}", lat, lon);
                sleep(Duration::from_secs(1)).await;
                match get_location(lat, lon).await {
                    Ok(location_response) => {
                        rename_file(&info.path, Some(&location_response), &info.date, sequence)?;
                        sequence += 1;
                    }
                    Err(e) => eprintln!("  Error getting location: {}", e),
                }
            } else {
                println!("  No GPS coordinates, renaming with date only.");
                rename_file(&info.path, None, &info.date, sequence)?;
                sequence += 1;
            }
        }
    }

    println!("\nDone. Press Enter to exit...");
    let _ = io::stdin().read(&mut [0u8]);

    Ok(())
}

fn is_supported_image(path: &Path) -> bool {
    let ext = path.extension().and_then(|s| s.to_str()).unwrap_or("").to_lowercase();
    ext == "jpg" || ext == "jpeg" || ext == "heic"
}

fn extract_metadata(path: &Path) -> Option<FileInfo> {
    let file = fs::File::open(path).ok()?;
    let mut bufreader = std::io::BufReader::new(&file);
    let reader = exif::Reader::new();
    let exif = reader.read_from_container(&mut bufreader).ok();

    let mut coords: Option<(f64, f64)> = None;
    let mut date_digits: Option<String> = None;

    if let Some(ref exif) = exif {
        // Try to extract GPS coordinates
        if let (Some(lat), Some(lat_ref), Some(lon), Some(lon_ref)) = (
            exif.get_field(Tag::GPSLatitude, In::PRIMARY),
            exif.get_field(Tag::GPSLatitudeRef, In::PRIMARY),
            exif.get_field(Tag::GPSLongitude, In::PRIMARY),
            exif.get_field(Tag::GPSLongitudeRef, In::PRIMARY),
        ) {
            if let (Some(latitude), Some(longitude)) = (to_decimal(lat), to_decimal(lon)) {
                let lat_final = if lat_ref.display_value().to_string().contains('S') { -latitude } else { latitude };
                let lon_final = if lon_ref.display_value().to_string().contains('W') { -longitude } else { longitude };
                coords = Some((lat_final, lon_final));
            }
        }

        // Try to extract EXIF date
        if let Some(date_field) = exif.get_field(Tag::DateTimeOriginal, In::PRIMARY)
            .or_else(|| exif.get_field(Tag::DateTime, In::PRIMARY))
        {
            let all_digits: String = date_field.display_value().to_string()
                .chars()
                .filter(|c| c.is_ascii_digit())
                .collect();
            if all_digits.len() >= 8 {
                date_digits = Some(all_digits);
            }
        }
    }

    // Fallback: use earliest filesystem timestamp (created or modified)
    if date_digits.is_none() {
        if let Ok(meta) = fs::metadata(path) {
            let earliest = [meta.modified().ok(), meta.created().ok()]
                .into_iter()
                .flatten()
                .min();
            if let Some(time) = earliest {
                let datetime: chrono::DateTime<chrono::Local> = time.into();
                date_digits = Some(datetime.format("%Y%m%d%H%M%S").to_string());
            }
        }
    }

    let all_digits = date_digits?;
    let yyyymmdd = all_digits[..8].to_string();
    let sort_key = all_digits[..all_digits.len().min(14)].to_string();

    Some(FileInfo {
        path: path.to_path_buf(),
        lat: coords.map(|(lat, _)| lat),
        lon: coords.map(|(_, lon)| lon),
        date: yyyymmdd,
        sort_key,
    })
}

fn to_decimal(field: &exif::Field) -> Option<f64> {
    if let exif::Value::Rational(ref v) = field.value {
        if v.len() >= 3 {
            let degrees = v[0].to_f64();
            let minutes = v[1].to_f64();
            let seconds = v[2].to_f64();
            return Some(degrees + minutes / 60.0 + seconds / 3600.0);
        }
    }
    None
}

async fn get_location(lat: f64, lon: f64) -> Result<GeocodeResponse, Box<dyn std::error::Error>> {
    let url = format!(
        "https://geocode.maps.co/reverse?lat={}&lon={}&api_key={}&accept-language={}",
        lat, lon, API_KEY, "en"
    );

    let client = reqwest::Client::new();
    let response = client.get(url)
        .header("User-Agent", "image-labeler/0.1.0")
        .send()
        .await?
        .json::<GeocodeResponse>()
        .await?;

    Ok(response)
}

fn rename_file(path: &Path, response: Option<&GeocodeResponse>, date: &str, sequence: u32) -> std::io::Result<()> {
    let extension = path.extension().and_then(|s| s.to_str()).unwrap_or("");

    let new_name = if let Some(response) = response {
        let road = response.address.road.as_deref();
        let town_or_city = response.address.town.as_deref()
            .or(response.address.city.as_deref())
            .or(response.address.village.as_deref());

        let country = response.address.country.as_deref();
        let country_code = response.address.country_code.as_deref().unwrap_or("unknown").to_uppercase();

        let mut location_parts = Vec::new();

        if let Some(place) = town_or_city {
            location_parts.push(place.to_string());
        }

        if let Some(r) = road {
            location_parts.push(r.to_string());
        }

        if location_parts.is_empty() && let Some(c) = country {
            location_parts.push(c.to_string());
        }

        let location = if location_parts.is_empty() {
            response.display_name.clone()
        } else {
            location_parts.join(", ")
        };

        let safe_location = location.chars()
            .map(|c| if c.is_alphanumeric() || c == ' ' || c == ',' { c } else { '_' })
            .collect::<String>()
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ");

        format!("{}_{}_{}, {}.{}", date, sequence, country_code, safe_location, extension)
    } else {
        format!("{}_{}.{}", date, sequence, extension)
    };
    let new_path = path.with_file_name(new_name);

    println!("  Renaming to: {:?}", new_path);
    fs::rename(path, new_path)?;
    Ok(())
}
