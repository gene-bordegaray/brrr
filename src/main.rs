use std::collections::HashMap;
use std::env;
use std::fmt;
use std::fs::File;
use std::io::{BufRead, BufReader};

const DELIMITER: char = ';';
/// Station Name (100 bytes) + Delimiter (1 byte) + Value (5 bytes)
const MAX_LINE_BYTES: u8 = 100 + 1 + 5;

struct StationStats {
    min: f32,
    max: f32,
    sum: f32,
    count: u32,
}

impl StationStats {
    fn new(val: f32) -> Self {
        StationStats {
            min: val,
            max: val,
            sum: val,
            count: 1,
        }
    }

    fn update_stats(&mut self, val: f32) {
        self.min = self.min.min(val);
        self.max = self.max.max(val);
        self.sum += val;
        self.count += 1;
    }

    fn mean(&self) -> f32 {
        self.sum / self.count as f32
    }
}

impl fmt::Display for StationStats {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:.1}/{:.1}/{:.1}", self.min, self.mean(), self.max)
    }
}

fn print_final_results(station_stats_map: &HashMap<String, StationStats>) {
    let mut sorted_stations: Vec<_> = station_stats_map.iter().collect();

    sorted_stations.sort_by_key(|(station, _stats)| *station);

    print!("{{");

    for (i, (station, stats)) in sorted_stations.iter().enumerate() {
        if i > 0 {
            print!(", ");
        }

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

    let mut station_stats_map: HashMap<String, StationStats> = HashMap::new();
    let mut curr_line = String::with_capacity(MAX_LINE_BYTES as usize);

    loop {
        let bytes_read = reader.read_line(&mut curr_line)?;
        if bytes_read <= 0 {
            break;
        }

        let (station, val) = curr_line
            .trim()
            .split_once(DELIMITER)
            .ok_or_else(|| "malformed line, did not find delimiter ';'")?;
        let val: f32 = val.trim().parse()?;

        if let Some(stats) = station_stats_map.get_mut(station) {
            stats.update_stats(val);
        } else {
            station_stats_map.insert(station.to_string(), StationStats::new(val));
        }

        curr_line.clear();
    }

    print_final_results(&station_stats_map);

    Ok(())
}
