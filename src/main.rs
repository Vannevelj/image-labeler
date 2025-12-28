use clap::Parser;
use exif::{In, Tag};
use serde::Deserialize;
use std::fs;
use std::path::{Path, PathBuf};
use tokio::time::{sleep, Duration};

#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
struct Args {
    /// Path to the directory containing JPEG files
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

const API_KEY: &str = "692f950529d1f964657378ztj33fdb0";

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = Args::parse();

    if !args.path.is_dir() {
        eprintln!("Error: Provided path is not a directory.");
        std::process::exit(1);
    }

    let mut sequence = 1;

    for entry in fs::read_dir(args.path)? {
        let entry = entry?;
        let path = entry.path();

        if is_jpeg(&path) {
            println!("Processing: {:?}", path);
            let metadata = extract_metadata(&path);
            if let Some((lat, lon, date)) = metadata {
                println!("  Found coordinates: {}, {}", lat, lon);
                println!("  Found date: {}", date);
                // Sleep for 1 second to respect API rate limits
                sleep(Duration::from_secs(1)).await;
                match get_location(lat, lon).await {
                    Ok(location_response) => {
                        rename_file(&path, &location_response, &date, sequence)?;
                        sequence += 1;
                    }
                    Err(e) => eprintln!("  Error getting location: {}", e),
                }
            } else {
                println!("  Missing GPS or Date metadata.");
            }
        }
    }

    Ok(())
}

fn is_jpeg(path: &Path) -> bool {
    let ext = path.extension().and_then(|s| s.to_str()).unwrap_or("").to_lowercase();
    ext == "jpg" || ext == "jpeg"
}

fn extract_metadata(path: &Path) -> Option<(f64, f64, String)> {
    let file = fs::File::open(path).ok()?;
    let mut bufreader = std::io::BufReader::new(&file);
    let reader = exif::Reader::new();
    let exif = reader.read_from_container(&mut bufreader).ok()?;

    let lat = exif.get_field(Tag::GPSLatitude, In::PRIMARY)?;
    let lat_ref = exif.get_field(Tag::GPSLatitudeRef, In::PRIMARY)?;
    let lon = exif.get_field(Tag::GPSLongitude, In::PRIMARY)?;
    let lon_ref = exif.get_field(Tag::GPSLongitudeRef, In::PRIMARY)?;

    let latitude = to_decimal(lat)?;
    let longitude = to_decimal(lon)?;

    let lat_final = if lat_ref.display_value().to_string().contains('S') { -latitude } else { latitude };
    let lon_final = if lon_ref.display_value().to_string().contains('W') { -longitude } else { longitude };

    // Extract date
    let date_str = exif.get_field(Tag::DateTimeOriginal, In::PRIMARY)
        .or_else(|| exif.get_field(Tag::DateTime, In::PRIMARY))?
        .display_value()
        .to_string();

    // Format yyyy:mm:dd hh:mm:ss to yyyyMMdd
    // exif display_value is often "2023:10:24 12:00:00"
    let yyyymmdd = date_str.chars()
        .filter(|c| c.is_digit(10))
        .take(8)
        .collect::<String>();

    if yyyymmdd.len() == 8 {
        Some((lat_final, lon_final, yyyymmdd))
    } else {
        None
    }
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

fn rename_file(path: &Path, response: &GeocodeResponse, date: &str, sequence: u32) -> std::io::Result<()> {
    let extension = path.extension().and_then(|s| s.to_str()).unwrap_or("");
    
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
    
    // Sanitize location for filename
    let safe_location = location.chars()
        .map(|c| if c.is_alphanumeric() || c == ' ' || c == ',' { c } else { '_' })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");

    let new_name = format!("{}_{}_{}, {}.{}", date, sequence, country_code, safe_location, extension);
    let new_path = path.with_file_name(new_name);

    println!("  Renaming to: {:?}", new_path);
    fs::rename(path, new_path)?;
    Ok(())
}
