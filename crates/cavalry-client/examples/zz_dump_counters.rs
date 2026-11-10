//! THROWAWAY: prints every allocated counter of a driver directory that is not 0.
use cavalry_shm::counters::CountersReader;
use cavalry_shm::counters::generated::COUNTERS;

fn main() {
    let dir = std::env::args().nth(1).expect("a driver directory");
    let reader = CountersReader::open(std::path::Path::new(&dir)).expect("the counters file opens");
    for id in 0..reader.capacity() {
        let Some(record) = reader.record(id).ok().flatten() else {
            continue;
        };
        let value = reader.read_acquire(id).unwrap_or(i64::MIN);
        if value == 0 {
            continue;
        }
        let name = COUNTERS
            .iter()
            .find(|def| def.id.0 == record.type_id())
            .map_or("?", |def| def.name);
        let label = record.label();
        println!("{id:5} {name:40} {value:14} {label}");
    }
}
