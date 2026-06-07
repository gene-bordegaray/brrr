use std::collections::HashMap;
use std::env;
use std::fmt;
use std::fs::File;
use std::io::{BufRead, BufReader};

const DELIMITER: u8 = b';';
const NEW_LINE_MARKER: u8 = b'\n';
/// Station Name (100 bytes) + Delimiter (1 byte) + Value (5 bytes)
const MAX_LINE_BYTES: usize = 100 + 1 + 5;
const MAX_UNIQUE_STATIONS: usize = 10_000;

struct StationStats {
    min: i32,
    max: i32,
    sum: i64,
    count: u32,
}

impl StationStats {
    fn new(val: i32) -> Self {
        StationStats {
            min: val,
            max: val,
            sum: val as i64,
            count: 1,
        }
    }

    fn update_stats(&mut self, val: i32) {
        self.min = self.min.min(val);
        self.max = self.max.max(val);
        self.sum += val as i64;
        self.count += 1;
    }

    fn mean_rounded_tenths(&self) -> i32 {
        (self.sum as f32 / self.count as f32).round() as i32
    }
}

fn fmt_tenths(f: &mut fmt::Formatter<'_>, val: i32) -> fmt::Result {
    if val < 0 && val > -10 {
        write!(f, "-0.{}", -val)
    } else {
        write!(f, "{}.{}", val / 10, val.abs() % 10)
    }
}

impl fmt::Display for StationStats {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt_tenths(f, self.min)?;
        write!(f, "/")?;
        fmt_tenths(f, self.mean_rounded_tenths())?;
        write!(f, "/")?;
        fmt_tenths(f, self.max)
    }
}

fn read_line<R: BufRead>(reader: &mut R, buf: &mut Vec<u8>) -> std::io::Result<Option<usize>> {
    buf.clear();

    let bytes_read = reader.read_until(NEW_LINE_MARKER, buf)?;
    if bytes_read == 0 {
        return Ok(None);
    }

    if buf.ends_with(&[NEW_LINE_MARKER]) {
        buf.pop();
    }

    for (delimiter_idx, byte) in buf.iter().enumerate() {
        if *byte == DELIMITER {
            return Ok(Some(delimiter_idx));
        }
    }

    Ok(None)
}

fn parse_temp_tenths(bytes: &[u8]) -> i32 {
    let mut sign = 1;
    let mut idx = 0;

    if bytes[0] == b'-' {
        sign = -1;
        idx = 1;
    }

    let mut val = 0;
    while bytes[idx] != b'.' {
        val = val * 10 + (bytes[idx] - b'0') as i32;
        idx += 1;
    }

    idx += 1;
    val = val * 10 + (bytes[idx] - b'0') as i32;

    sign * val
}

fn print_final_results(station_stats_map: &HashMap<Vec<u8>, StationStats>) {
    let mut sorted_stations: Vec<_> = station_stats_map.iter().collect();

    sorted_stations.sort_by_key(|(station, _stats)| *station);

    print!("{{");

    for (i, (station, stats)) in sorted_stations.iter().enumerate() {
        if i > 0 {
            print!(", ");
        }
        let station = String::from_utf8_lossy(&station);
        print!("{station}={stats}");
    }

    println!("}}");
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let path = env::args()
        .nth(1)
        .ok_or("usage: weather <path-to-measurements-file>")?;
    let file = File::open(path)?;
    let mut reader = BufReader::new(file);

    let mut station_stats_map: HashMap<Vec<u8>, StationStats> =
        HashMap::with_capacity(MAX_UNIQUE_STATIONS);
    let mut curr_line: Vec<u8> = Vec::with_capacity(MAX_LINE_BYTES);

    loop {
        let Some(delimiter_idx) = read_line(&mut reader, &mut curr_line)? else {
            break;
        };
        let delimiter_idx = delimiter_idx as usize;
        let station = &curr_line[..delimiter_idx];
        let val = parse_temp_tenths(&curr_line[delimiter_idx + 1..]);

        if let Some(stats) = station_stats_map.get_mut(station) {
            stats.update_stats(val);
        } else {
            station_stats_map.insert(station.to_vec(), StationStats::new(val));
        }
    }

    print_final_results(&station_stats_map);

    Ok(())
}
