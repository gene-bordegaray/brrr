use std::collections::HashMap;
use std::env;
use std::fmt;
use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom};
use std::ops::Range;
use std::os::unix::fs::FileExt;
use std::path::{Path, PathBuf};

/// ---- Facts ----

const DELIMITER: u8 = b';';
const NEW_LINE_MARKER: u8 = b'\n';
const DECIMAL_POINT: u8 = b'.';
/// Station Name (100 bytes) + Delimiter (1 byte) + Value (5 bytes)
const MAX_LINE_BYTES: usize = 100 + 1 + 5;
const MAX_UNIQUE_STATIONS: usize = 10_000;
const DEFAULT_NUM_PARTITIONS: usize = 16;

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

    fn merge(&mut self, other: StationStats) {
        self.min = self.min.min(other.min);
        self.max = self.max.max(other.max);
        self.sum += other.sum;
        self.count += other.count;
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

enum Bucket {
    Single(Entry),
    Multi(Vec<Entry>),
}

impl Bucket {
    fn new(entry: Entry) -> Self {
        Self::Single(entry)
    }

    fn get_entry_mut(&mut self, station: &[u8]) -> Option<&mut Entry> {
        return match self {
            Bucket::Single(entry) => {
                if entry.station == station {
                    Some(entry)
                } else {
                    None
                }
            }
            Bucket::Multi(entries) => entries.iter_mut().find(|entry| entry.station == station),
        };
    }

    fn push_entry(&mut self, entry: Entry) {
        match self {
            Bucket::Single(_) => {
                let old_bucket = std::mem::replace(self, Bucket::Multi(Vec::new()));

                let Bucket::Single(old_entry) = old_bucket else {
                    unreachable!();
                };

                *self = Bucket::Multi(vec![old_entry, entry]);
            }
            Bucket::Multi(entries) => {
                entries.push(entry);
            }
        }
    }

    fn into_entries(self) -> Vec<Entry> {
        match self {
            Bucket::Single(entry) => vec![entry],
            Bucket::Multi(entries) => entries,
        }
    }

    fn entries(&self) -> &[Entry] {
        match self {
            Bucket::Single(entry) => std::slice::from_ref(entry),
            Bucket::Multi(entries) => entries,
        }
    }
}

/// Responsible for holding the running aggregation state for stations.
///
/// Represented as a HashMap, mapping fingerprints of station name to list of entries.
struct Aggregator {
    station_map: HashMap<u64, Bucket>,
}

impl Aggregator {
    fn new(station_map_size: usize) -> Self {
        Self {
            station_map: HashMap::with_capacity(station_map_size),
        }
    }

    fn station_map(&self) -> &HashMap<u64, Bucket> {
        &self.station_map
    }

    fn update(&mut self, fingerprint: u64, station: &[u8], val: i32) {
        if let Some(bucket) = self.station_map.get_mut(&fingerprint) {
            if let Some(entry) = bucket.get_entry_mut(station) {
                entry.stats.update_stats(val);
            } else {
                bucket.push_entry(Entry {
                    station: station.to_vec(),
                    stats: StationStats::new(val),
                });
            }
        } else {
            self.station_map.insert(
                fingerprint,
                Bucket::new(Entry {
                    station: station.to_vec(),
                    stats: StationStats::new(val),
                }),
            );
        }
    }

    fn merge(&mut self, other: Aggregator) {
        for (fingerprint, bucket) in other.station_map {
            for entry in bucket.into_entries() {
                self.merge_entry(fingerprint, entry);
            }
        }
    }

    fn merge_entry(&mut self, fingerprint: u64, entry: Entry) {
        if let Some(bucket) = self.station_map.get_mut(&fingerprint) {
            if let Some(existing) = bucket.get_entry_mut(&entry.station) {
                existing.stats.merge(entry.stats);
            } else {
                bucket.push_entry(entry);
            }
        } else {
            self.station_map.insert(fingerprint, Bucket::new(entry));
        }
    }
}

/// Responsible for scanning files and processing their bytes into a useful format.
struct FileScanner {
    file: File,
    buf: Vec<u8>,
    buf_size: usize,
    leftover: Vec<u8>,
    position: u64,
    range_end: u64,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ChunkEnd {
    MoreInput,
    End,
}

impl FileScanner {
    fn try_new(mut file: File, range: Range<u64>, buf_size: usize) -> std::io::Result<Self> {
        file.seek(SeekFrom::Start(range.start))?;
        Ok(Self {
            file,
            buf: vec![0u8; buf_size],
            buf_size,
            leftover: Vec::with_capacity(MAX_LINE_BYTES),
            position: range.start,
            range_end: range.end,
        })
    }

    /// Reads the nexxt chunk of `self.file` into `self.buf` accounting for leftover from teh last
    /// chunk read.
    ///
    /// Returns:
    /// - `(valid_len, chunk_end)` where `valid_len` is the total bytes read into the buffer ready to
    ///    process and `chunk_end` indicates whether this chunk reaches the end of the
    ///    file.
    /// - None if no bytes were read from `self.file`.
    fn read_next_chunk(&mut self) -> std::io::Result<Option<(usize, ChunkEnd)>> {
        let leftover_len = self.leftover.len();

        if leftover_len >= self.buf.len() {
            self.buf.resize(leftover_len + self.buf_size, 0);
        }

        self.buf[..leftover_len].copy_from_slice(&self.leftover);
        self.leftover.clear();

        if self.position >= self.range_end {
            if leftover_len == 0 {
                return Ok(None);
            }

            return Ok(Some((leftover_len, ChunkEnd::End)));
        }

        let remaining = (self.range_end - self.position) as usize;
        let read_capacity = self.buf.len() - leftover_len;
        let read_len = remaining.min(read_capacity);

        let bytes_read = self
            .file
            .read(&mut self.buf[leftover_len..leftover_len + read_len])?;

        self.position += bytes_read as u64;

        let valid_len = leftover_len + bytes_read;

        if valid_len == 0 {
            return Ok(None);
        }

        let chunk_end = if self.position >= self.range_end || bytes_read == 0 {
            ChunkEnd::End
        } else {
            ChunkEnd::MoreInput
        };

        Ok(Some((valid_len, chunk_end)))
    }

    /// Extends the leftover buffer to save partial read of a row that was cut off by the buffer.
    fn save_incomplete_row(&mut self, row_start: usize, valid_len: usize, chunk_end: ChunkEnd) {
        if chunk_end == ChunkEnd::MoreInput {
            self.leftover
                .extend_from_slice(&self.buf[row_start..valid_len]);
        }
    }

    /// Reads one chunk, scans complete rows, and updates the aggregator.
    /// If the last row is split across chunks, its bytes are saved in `leftover`.
    ///
    /// Returns:
    /// - `true` if any bytes wee scanned from `self.file`.
    /// - `false` otherwise.
    fn scan_chunk(&mut self, aggregator: &mut Aggregator) -> std::io::Result<bool> {
        let Some((valid_len, chunk_end)) = self.read_next_chunk()? else {
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
                self.save_incomplete_row(station_start, valid_len, chunk_end);
                break;
            }

            let delimiter_idx = idx;
            let station = &self.buf[station_start..delimiter_idx];
            idx += 1;

            if idx == valid_len {
                self.save_incomplete_row(station_start, valid_len, chunk_end);
                break;
            }

            let Some((val, line_end_idx)) = parse_temp_field(idx, valid_len, chunk_end, &self.buf)
            else {
                self.save_incomplete_row(station_start, valid_len, chunk_end);
                break;
            };

            aggregator.update(fingerprint, station, val);
            idx = line_end_idx + 1;
        }

        Ok(true)
    }
}

/// ---- Data / Conversion Helpers -----

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

fn parse_temp_field(
    mut idx: usize,
    valid_len: usize,
    chunk_end: ChunkEnd,
    buf: &[u8],
) -> Option<(i32, usize)> {
    let mut sign = 1;
    let mut val = 0;

    if buf[idx] == b'-' {
        sign = -1;
        idx += 1;
    }

    while idx < valid_len && buf[idx] != DECIMAL_POINT {
        val = val * 10 + (buf[idx] - b'0') as i32;
        idx += 1;
    }

    if idx == valid_len {
        return None;
    }

    idx += 1;

    while idx < valid_len && buf[idx] != NEW_LINE_MARKER {
        val = val * 10 + (buf[idx] - b'0') as i32;
        idx += 1;
    }

    if idx == valid_len {
        if chunk_end == ChunkEnd::End {
            return Some((sign * val, idx));
        }

        return None;
    }

    Some((sign * val, idx))
}

/// ---- File Helpers ----

fn find_next_line_start(file: &File, offset: u64, file_len: u64) -> io::Result<u64> {
    if offset >= file_len {
        return Ok(file_len);
    }

    let mut buf = vec![0u8; MAX_LINE_BYTES];
    let bytes_read = file.read_at(&mut buf, offset)?;

    for (idx, byte) in buf[..bytes_read].iter().enumerate() {
        if *byte == NEW_LINE_MARKER {
            return Ok((offset + idx as u64 + 1).min(file_len));
        }
    }

    Ok(file_len)
}

fn find_partition_ranges(path: &Path, partitions: usize) -> io::Result<Vec<Range<u64>>> {
    let file = File::open(path)?;
    let file_len = file.metadata()?.len();
    if file_len == 0 || partitions == 0 {
        return Ok(Vec::new());
    }

    let partition_size = file_len.div_ceil(partitions as u64);
    let mut ranges = Vec::with_capacity(partitions);
    let mut start = 0;

    for partition_idx in 1..partitions {
        let target = partition_size * partition_idx as u64;
        if target >= file_len {
            break;
        }

        let end = find_next_line_start(&file, target, file_len)?;
        ranges.push(start..end);
        start = end;
    }

    ranges.push(start..file_len);
    Ok(ranges)
}

fn process_range(path: PathBuf, range: Range<u64>) -> std::io::Result<Aggregator> {
    let file = File::open(path)?;

    let mut scanner = FileScanner::try_new(file, range, DEFAULT_BUF_SIZE)?;
    let mut aggregator = Aggregator::new(MAX_UNIQUE_STATIONS);

    while scanner.scan_chunk(&mut aggregator)? {}

    Ok(aggregator)
}

/// ---- Display Helpers ----

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
    let path = PathBuf::from(path);

    let ranges = find_partition_ranges(&path, DEFAULT_NUM_PARTITIONS)?;

    let handles: Vec<_> = ranges
        .into_iter()
        .map(|range| {
            let path = path.clone();
            std::thread::spawn(move || process_range(path, range))
        })
        .collect();

    let mut final_aggregator = Aggregator::new(MAX_UNIQUE_STATIONS);

    for handle in handles {
        let partial = handle.join().unwrap()?;
        final_aggregator.merge(partial);
    }

    let mut station_entries: Vec<&Entry> = final_aggregator
        .station_map()
        .values()
        .flat_map(|bucket| bucket.entries())
        .collect();
    station_entries.sort_by(|a, b| a.station.cmp(&b.station));
    print_final_results(&station_entries);

    Ok(())
}
