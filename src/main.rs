use std::collections::HashMap;
use std::env;
use std::fmt;
use std::fs::File;
use std::io::Read;

/// ---- Facts ----

const DELIMITER: u8 = b';';
const NEW_LINE_MARKER: u8 = b'\n';
/// Station Name (100 bytes) + Delimiter (1 byte) + Value (5 bytes)
const MAX_LINE_BYTES: usize = 100 + 1 + 5;
const MAX_UNIQUE_STATIONS: usize = 10_000;

/// ---- Heuristics ----

/// Size of buffer read file into at a time.
const DEFAULT_BUF_SIZE: usize = 8 * 1024 * 1024; // 8 MiB

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

struct Entry {
    station: Vec<u8>,
    stats: StationStats,
}

/// Responsible for holding the running aggregation state for stations.
///
/// Represented as a HashMap, mapping fingerprints of station name to list of entries.
struct Aggregator {
    station_map: HashMap<u64, Vec<Entry>>,
}

impl Aggregator {
    fn new(station_map_size: usize) -> Self {
        Self {
            station_map: HashMap::with_capacity(station_map_size),
        }
    }

    fn station_map(&self) -> &HashMap<u64, Vec<Entry>> {
        &self.station_map
    }

    fn update(&mut self, fingerprint: u64, station: &[u8], val: i32) {
        if let Some(bucket) = self.station_map.get_mut(&fingerprint) {
            if let Some(entry) = bucket.iter_mut().find(|entry| entry.station == station) {
                entry.stats.update_stats(val);
            } else {
                bucket.push(Entry {
                    station: station.to_vec(),
                    stats: StationStats::new(val),
                });
            }
        } else {
            self.station_map.insert(
                fingerprint,
                vec![Entry {
                    station: station.to_vec(),
                    stats: StationStats::new(val),
                }],
            );
        }
    }
}

/// Responsible for scanning files and processing their bytes into a useful format.
struct FileScanner {
    file: File,
    buf: Vec<u8>,
    leftover: Vec<u8>,
}

impl FileScanner {
    fn new(file: File, buf_size: usize) -> Self {
        Self {
            file,
            buf: vec![0u8; buf_size],
            leftover: Vec::with_capacity(MAX_LINE_BYTES),
        }
    }

    /// Reads the nexxt chunk of `self.file` into `self.buf` accounting for leftover from teh last
    /// chunk read.
    ///
    /// Returns:
    /// - `(valid_len, at_eof)` where `valid_len` is the total bytes read into the buffer ready to
    ///    process and `at_eof` is a marker indicating if we the chunk read through the end of the
    ///    file.
    /// - None if no bytes were read from `self.file`.
    fn read_next_chunk(&mut self) -> std::io::Result<Option<(usize, bool)>> {
        let leftover_len = self.leftover.len();
        if leftover_len >= self.buf.len() {
            self.buf.resize(leftover_len + DEFAULT_BUF_SIZE, 0);
        }

        self.buf[..leftover_len].copy_from_slice(&self.leftover);
        self.leftover.clear();

        let bytes_read = self.file.read(&mut self.buf[leftover_len..])?;
        let valid_len = leftover_len + bytes_read;

        if valid_len == 0 {
            return Ok(None);
        }

        let at_eof = bytes_read == 0;
        Ok(Some((valid_len, at_eof)))
    }

    /// Reads one chunk, scans complete rows, and updates the aggregator.
    /// If the last row is split across chunks, its bytes are saved in `leftover`.
    ///
    /// Returns:
    /// - `true` if any bytes wee scanned from `self.file`.
    /// - `false` otherwise.
    fn scan_chunk(&mut self, aggregator: &mut Aggregator) -> std::io::Result<bool> {
        let Some((valid_len, at_eof)) = self.read_next_chunk()? else {
            return Ok(false);
        };

        let mut idx = 0;

        while idx < valid_len {
            let station_start = idx;
            let mut fingerprint = 0u64;
            while idx < valid_len && self.buf[idx] != DELIMITER {
                fingerprint = fingerprint_step(fingerprint, self.buf[idx]);
                idx += 1;
            }

            if idx == valid_len {
                if !at_eof {
                    self.leftover
                        .extend_from_slice(&self.buf[station_start..valid_len]);
                }
                break;
            }

            let delimiter_idx = idx;
            let station = &self.buf[station_start..delimiter_idx];
            idx += 1;

            while idx < valid_len && self.buf[idx] != NEW_LINE_MARKER {
                idx += 1;
            }

            if idx == valid_len {
                if at_eof {
                    let val = parse_temp_tenths(&self.buf[delimiter_idx + 1..idx]);
                    aggregator.update(fingerprint, station, val);
                } else {
                    self.leftover
                        .extend_from_slice(&self.buf[station_start..valid_len]);
                }
                break;
            }

            let val = parse_temp_tenths(&self.buf[delimiter_idx + 1..idx]);
            aggregator.update(fingerprint, station, val);

            idx += 1;
        }

        Ok(true)
    }
}

/// Computes a single step of a Polynomial Rolling Hash function.
///
/// Formula: `hash = (hash * 31) + byte`
///
/// This is a popular basic hash function because:
///
/// - Compiler will actually optimize this into: `hash = ((hash << 5) - hash) + byte` which avoids
///   the multiplication overehad on the CPU
/// - Multiplying by `31` gives us a good distribution of keys because it is a prime number leading
///   to less common factors and is a large enough prime to shift meaningfully.
///
/// NOTE: This is not the best hash function but a simple one. It is vulnerable to collisions which
/// lead to linear traversals over the routed bucket. May want to update if this shows up in
/// profiles.
fn fingerprint_step(hash: u64, byte: u8) -> u64 {
    hash.wrapping_mul(31).wrapping_add(byte as u64)
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

fn print_final_results(station_entries: &[&Entry]) {
    print!("{{");

    for (i, entry) in station_entries.iter().enumerate() {
        if i > 0 {
            print!(", ");
        }
        let station = String::from_utf8_lossy(&entry.station);
        let stats = &entry.stats;
        print!("{station}={stats}");
    }

    println!("}}");
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let path = env::args()
        .nth(1)
        .ok_or("usage: weather <path-to-measurements-file>")?;
    let file = File::open(path)?;
    let mut scanner = FileScanner::new(file, DEFAULT_BUF_SIZE);
    let mut aggregator = Aggregator::new(MAX_UNIQUE_STATIONS);

    while scanner.scan_chunk(&mut aggregator)? {}

    let mut station_entries: Vec<&Entry> = aggregator.station_map().values().flatten().collect();
    station_entries.sort_by(|a, b| a.station.cmp(&b.station));
    print_final_results(&station_entries);

    Ok(())
}
