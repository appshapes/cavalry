//! Pre-touching: every page of a shared mapping is made resident when the mapping is
//! made, so that the data plane never takes a page fault
//! ([ADR 0027](../../../docs/decisions/0027-pre-touch-shared-mappings-at-map-time.md)).
//! A page fault is a kernel entry, and two mappings faulting the same fresh page race on
//! its lock, so the loser sleeps and the winner wakes it with an interrupt. Aeron calls
//! this `aeron.pre.touch.mapped.memory` and defaults it off; Cavalry does it always, for
//! every mapping it makes, on the control plane, where `create` and `open` run.

use std::io;

use memmap2::{Mmap, MmapMut};

/// The stride of the touch loop: the smallest page Cavalry runs on. A host with larger
/// pages, 16 KiB on Apple silicon, is touched four times per page, which costs about a
/// microsecond per megabyte and nothing else.
const PAGE: usize = 4096;

/// Makes every page of a read-only mapping resident. On Linux, `MADV_POPULATE_READ`
/// (5.14) faults the pages in with one call and reports a page the kernel cannot back,
/// so a driver directory out of memory fails here, by name, and not with a bus error on
/// the data plane; a kernel without it falls back to the touch loop, as every other
/// system does.
pub(crate) fn readable(map: &Mmap) -> io::Result<()> {
    #[cfg(target_os = "linux")]
    match map.advise(memmap2::Advice::PopulateRead) {
        Ok(()) => return Ok(()),
        Err(error) if error.kind() == io::ErrorKind::InvalidInput => {}
        Err(error) => return Err(cannot_back(error)),
    }
    touch(map);
    Ok(())
}

/// Makes every page of a writable mapping resident and writable. On Linux,
/// `MADV_POPULATE_WRITE` (5.14) faults the pages in as a write would, without writing,
/// so the first store to each page costs nothing later; a kernel without it falls back to
/// the touch loop, as every other system does, where a read maps the page and the first
/// store may still take a fault that tracks the page as dirty.
pub(crate) fn writable(map: &MmapMut) -> io::Result<()> {
    #[cfg(target_os = "linux")]
    match map.advise(memmap2::Advice::PopulateWrite) {
        Ok(()) => return Ok(()),
        Err(error) if error.kind() == io::ErrorKind::InvalidInput => {}
        Err(error) => return Err(cannot_back(error)),
    }
    touch(map);
    Ok(())
}

/// Reads one byte per page and discards it; the page is the point.
fn touch(bytes: &[u8]) {
    let mut offset = 0;
    while offset < bytes.len() {
        std::hint::black_box(bytes[offset]);
        offset += PAGE;
    }
}

/// The error an operator reads when a mapping cannot be made resident: what happened,
/// what to do, and the docs anchor.
#[cfg(target_os = "linux")]
fn cannot_back(error: io::Error) -> io::Error {
    io::Error::new(
        error.kind(),
        format!(
            "could not make the mapping resident ({error}); the driver directory's \
             filesystem has no room for it: give it room or move it with --dir, and run \
             `cavalry doctor` (driver-directory-backing); see \
             docs/reference/config.md#dir"
        ),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_touch_reads_one_byte_per_page_and_the_last_partial_page() {
        // Nothing to observe but that it runs over every page, including a partial last
        // one, without reading past the end.
        let bytes = vec![7u8; PAGE * 3 + 1];
        touch(&bytes);
        touch(&bytes[..1]);
        touch(&[]);
    }
}
